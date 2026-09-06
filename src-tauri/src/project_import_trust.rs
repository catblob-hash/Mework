//! Persistent trust decisions for project-memory sources that resolve outside
//! the current workspace.
//!
//! Discovery deliberately happens before approval and never reads a pending
//! source. The approval surface receives only [`ProjectImportCandidate`],
//! while canonical workspace/target paths remain backend-only authorization
//! identities.

use crate::project_memory::{
    self, ProjectMemoryDiagnosticKind, ProjectMemoryOptions, ProjectMemoryReport,
};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::fs::File;
use std::{
    collections::{BTreeSet, HashMap},
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const MAX_RESOLUTION_ROUNDS: usize = 16;
const MAX_APPROVAL_TARGETS: usize = 16;
/// Version stamped into the encrypted ledger. A ledger written by any other
/// version is rejected rather than migrated, exactly as the document store does:
/// there is no migration ladder here either, and a rejected ledger fails closed
/// so no external import is trusted on that launch.
const STORE_VERSION: u32 = 1;
const INITIAL_GENERATION: u64 = 1;
const MAX_STORED_RECORDS: usize = 512;
const MAX_ENCRYPTED_STORE_BYTES: u64 = 4 * 1024 * 1024;
const STORE_FILE_NAME: &str = "project-import-trust.v1.bin";
const STORE_LOCK_FILE_NAME: &str = "project-import-trust.v1.lock";
const STORE_MAGIC: &[u8] = b"MEWORK_PROJECT_IMPORT_TRUST\x00\x01";
const STORE_ID_BYTES: usize = 16;
const NONCE_BYTES: usize = 12;
const KEYRING_SERVICE: &str = "com.mework.app.project-import-trust.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectImportDecision {
    Allowed,
    Denied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectImportTargetKind {
    File,
    Directory,
}

impl ProjectImportTargetKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "file" => Ok(Self::File),
            "directory" => Ok(Self::Directory),
            _ => Err("项目记忆导入信任记录包含无效目标类型".into()),
        }
    }
}

impl ProjectImportDecision {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "allowed" => Ok(Self::Allowed),
            "denied" => Ok(Self::Denied),
            _ => Err("项目记忆导入信任记录包含无效决策".into()),
        }
    }
}

/// Content- and path-free candidate shown in the native approval dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectImportCandidate {
    pub label: String,
    /// Rich but redacted path identity for the native approval dialog only.
    /// This type deliberately does not implement `Serialize`.
    pub approval_display: String,
    pub target_kind: ProjectImportTargetKind,
}

/// Renderer-facing record. Canonical paths are intentionally absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectImportTrustSummary {
    pub id: String,
    pub workspace_id: String,
    pub label: String,
    pub target_kind: ProjectImportTargetKind,
    pub decision: ProjectImportDecision,
    /// False for a decision bound to an older/moved workspace root or a target
    /// that is no longer present with the approved canonical identity.
    pub active_for_current_workspace: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Backend-only persisted authorization record.
///
/// Do not implement `Serialize` or `Debug`: both canonical paths are private
/// authorization material and must never cross IPC or enter ordinary logs.
#[derive(Clone)]
pub(crate) struct StoredProjectImportTrust {
    pub id: String,
    pub workspace_id: String,
    pub canonical_workspace: PathBuf,
    pub canonical_target: PathBuf,
    pub label: String,
    pub target_kind: ProjectImportTargetKind,
    pub decision: ProjectImportDecision,
    pub created_at: String,
    pub updated_at: String,
}

impl StoredProjectImportTrust {
    pub(crate) fn summary(&self, active_for_current_workspace: bool) -> ProjectImportTrustSummary {
        ProjectImportTrustSummary {
            id: self.id.clone(),
            workspace_id: self.workspace_id.clone(),
            label: self.label.clone(),
            target_kind: self.target_kind,
            decision: self.decision,
            active_for_current_workspace,
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

/// Storage boundary implemented by the encrypted application-data store.
pub(crate) trait ProjectImportTrustRepository {
    fn list_records(&self) -> Result<Vec<StoredProjectImportTrust>, String>;

    /// Inserts the complete batch atomically. Implementations must reject an
    /// ID collision rather than replacing an existing authorization.
    /// Returns `false` when another writer already decided one of the same
    /// canonical targets. The caller then reloads and honors that persisted
    /// decision instead of using its stale dialog result.
    fn insert_records(&self, records: &[StoredProjectImportTrust]) -> Result<bool, String>;

    /// Removes exactly one record after verifying its workspace owner.
    fn revoke_record(&self, workspace_id: &str, record_id: &str) -> Result<bool, String>;

    /// Acquires the current ledger generation and workspace identity in one
    /// repository snapshot. Implementations must not assemble this lease from
    /// separate reads.
    fn acquire_lease(
        &self,
        workspace_id: &str,
        workspace_root: &Path,
    ) -> Result<ProjectImportTrustLease, String>;

    /// Checks the caller's current workspace identity and lease generation in
    /// one repository snapshot. Storage/authentication errors must be returned
    /// rather than treated as a current lease.
    fn lease_is_current(
        &self,
        lease: &ProjectImportTrustLease,
        workspace_id: &str,
        workspace_root: &Path,
    ) -> Result<bool, String>;
}

/// Backend-only capability proving which encrypted trust-ledger revision a
/// run observed for one workspace identity.
///
/// This deliberately implements neither `Serialize` nor `Deserialize` (nor
/// `Debug`), so canonical paths and the authorization revision cannot cross
/// renderer IPC or ordinary logs.
#[derive(Clone)]
pub(crate) struct ProjectImportTrustLease {
    workspace_id: String,
    canonical_workspace: PathBuf,
    generation: u64,
}

pub(crate) struct ProjectImportResolution {
    pub report: ProjectMemoryReport,
}

#[derive(Clone)]
struct PendingTarget {
    canonical_path: PathBuf,
    label: String,
    target_kind: ProjectImportTargetKind,
}

#[derive(Clone)]
enum MasterKeySource {
    SystemKeyring,
    #[cfg(test)]
    Fixed([u8; 32]),
    #[cfg(test)]
    Missing,
}

/// AEAD-encrypted, application-data repository. The random encryption key is
/// held only by the operating-system credential vault. The file never stores
/// plaintext canonical paths.
#[derive(Clone)]
pub(crate) struct EncryptedProjectImportTrustStore {
    path: PathBuf,
    lock_path: PathBuf,
    key_source: MasterKeySource,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedLedger {
    version: u32,
    generation: u64,
    records: Vec<PersistedRecord>,
}

struct TrustLedger {
    generation: u64,
    records: Vec<StoredProjectImportTrust>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedRecord {
    id: String,
    workspace_id: String,
    canonical_workspace: String,
    canonical_target: String,
    label: String,
    target_kind: String,
    decision: String,
    created_at: String,
    updated_at: String,
}

impl EncryptedProjectImportTrustStore {
    pub(crate) fn open(app_data: impl AsRef<Path>) -> Self {
        let directory = app_data.as_ref().join("memory");
        Self {
            path: directory.join(STORE_FILE_NAME),
            lock_path: directory.join(STORE_LOCK_FILE_NAME),
            key_source: MasterKeySource::SystemKeyring,
        }
    }

    #[cfg(test)]
    fn open_with_test_key(app_data: impl AsRef<Path>, key: [u8; 32]) -> Self {
        let mut store = Self::open(app_data);
        store.key_source = MasterKeySource::Fixed(key);
        store
    }

    #[cfg(test)]
    fn open_with_missing_test_key(app_data: impl AsRef<Path>) -> Self {
        let mut store = Self::open(app_data);
        store.key_source = MasterKeySource::Missing;
        store
    }

    fn with_lock<T>(
        &self,
        exclusive: bool,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        static PROCESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _process_guard = PROCESS_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let parent = self
            .lock_path
            .parent()
            .ok_or_else(|| "项目记忆导入信任存储路径无效".to_owned())?;
        fs::create_dir_all(parent).map_err(|_| "无法创建项目记忆导入信任存储目录".to_owned())?;
        validate_store_directory(parent)?;
        if symlink_metadata_optional(&self.lock_path, "无法检查项目记忆导入信任锁")?.is_some()
        {
            reject_reparse_path(&self.lock_path)?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&self.lock_path)
            .map_err(|_| "无法打开项目记忆导入信任锁".to_owned())?;
        reject_reparse_path(&self.lock_path)?;
        make_file_private(&self.lock_path)?;
        if exclusive {
            FileExt::lock_exclusive(&lock)
        } else {
            FileExt::lock_shared(&lock)
        }
        .map_err(|_| "无法锁定项目记忆导入信任存储".to_owned())?;
        let result = operation();
        let unlock_result =
            FileExt::unlock(&lock).map_err(|_| "无法释放项目记忆导入信任存储锁".to_owned());
        match (result, unlock_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    fn read_ledger_locked(&self) -> Result<TrustLedger, String> {
        let Some(metadata) = symlink_metadata_optional(&self.path, "无法检查项目记忆导入信任存储")?
        else {
            return Ok(TrustLedger {
                generation: INITIAL_GENERATION,
                records: Vec::new(),
            });
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || path_is_reparse_point(&self.path)?
            || metadata.len() > MAX_ENCRYPTED_STORE_BYTES
        {
            return Err("项目记忆导入信任存储类型或大小无效".into());
        }
        let encrypted =
            fs::read(&self.path).map_err(|_| "无法读取项目记忆导入信任存储".to_owned())?;
        if encrypted.len() < STORE_MAGIC.len() + STORE_ID_BYTES + NONCE_BYTES + 16
            || !encrypted.starts_with(STORE_MAGIC)
        {
            return Err("项目记忆导入信任存储格式无效".into());
        }
        let store_id_offset = STORE_MAGIC.len();
        let nonce_offset = store_id_offset + STORE_ID_BYTES;
        let store_id: [u8; STORE_ID_BYTES] = encrypted[store_id_offset..nonce_offset]
            .try_into()
            .map_err(|_| "项目记忆导入信任存储身份无效".to_owned())?;
        let key = self.load_master_key(&store_id, false)?.ok_or_else(|| {
            "项目记忆导入信任存储的系统凭据密钥缺失；为安全起见未加载任何外部导入".to_owned()
        })?;
        let ciphertext_offset = nonce_offset + NONCE_BYTES;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key.as_ref()));
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&encrypted[nonce_offset..ciphertext_offset]),
                Payload {
                    msg: &encrypted[ciphertext_offset..],
                    aad: &encrypted[..nonce_offset],
                },
            )
            .map_err(|_| "项目记忆导入信任存储认证失败；为安全起见未加载".to_owned())?;
        let plaintext = Zeroizing::new(plaintext);
        let ledger: PersistedLedger = serde_json::from_slice(&plaintext)
            .map_err(|_| "项目记忆导入信任存储内容无效".to_owned())?;
        if ledger.version != STORE_VERSION || ledger.generation < INITIAL_GENERATION {
            return Err("项目记忆导入信任存储版本或 generation 无效".into());
        }
        let (generation, records) = (ledger.generation, ledger.records);
        if records.len() > MAX_STORED_RECORDS {
            return Err("项目记忆导入信任存储记录数量无效".into());
        }
        let mut ids = BTreeSet::new();
        let mut output = Vec::with_capacity(records.len());
        for record in records {
            if !ids.insert(record.id.clone()) {
                return Err("项目记忆导入信任存储包含重复记录".into());
            }
            output.push(decode_persisted_record(record)?);
        }
        Ok(TrustLedger {
            generation,
            records: output,
        })
    }

    fn write_ledger_locked(&self, ledger: &TrustLedger) -> Result<(), String> {
        if ledger.generation < INITIAL_GENERATION {
            return Err("项目记忆导入信任 generation 无效".into());
        }
        if ledger.records.len() > MAX_STORED_RECORDS {
            return Err(format!(
                "项目记忆导入信任记录超过 {MAX_STORED_RECORDS} 条上限；请先撤销不再需要的记录"
            ));
        }
        let persisted = PersistedLedger {
            version: STORE_VERSION,
            generation: ledger.generation,
            records: ledger
                .records
                .iter()
                .map(encode_persisted_record)
                .collect::<Result<Vec<_>, _>>()?,
        };
        self.write_persisted_locked(&persisted)
    }

    fn write_persisted_locked<T: Serialize>(&self, persisted: &T) -> Result<(), String> {
        let plaintext = Zeroizing::new(
            serde_json::to_vec(persisted).map_err(|_| "无法编码项目记忆导入信任存储".to_owned())?,
        );
        let store_id = self.store_id_for_write()?;
        let key = self
            .load_master_key(&store_id, true)?
            .ok_or_else(|| "无法创建项目记忆导入信任密钥".to_owned())?;
        let nonce_uuid = Uuid::new_v4();
        let nonce = &nonce_uuid.as_bytes()[..NONCE_BYTES];
        let mut aad = Vec::with_capacity(STORE_MAGIC.len() + STORE_ID_BYTES);
        aad.extend_from_slice(STORE_MAGIC);
        aad.extend_from_slice(&store_id);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key.as_ref()));
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| "无法加密项目记忆导入信任存储".to_owned())?;
        let total_len = aad.len() + nonce.len() + ciphertext.len();
        if total_len as u64 > MAX_ENCRYPTED_STORE_BYTES {
            return Err("项目记忆导入信任存储超过安全大小上限".into());
        }
        let mut payload = Zeroizing::new(Vec::with_capacity(total_len));
        payload.extend_from_slice(&aad);
        payload.extend_from_slice(nonce);
        payload.extend_from_slice(&ciphertext);
        atomic_write_private(&self.path, &payload)
    }

    fn store_id_for_write(&self) -> Result<[u8; STORE_ID_BYTES], String> {
        let Some(metadata) = symlink_metadata_optional(&self.path, "无法检查项目记忆导入信任存储")?
        else {
            return Ok(*Uuid::new_v4().as_bytes());
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || path_is_reparse_point(&self.path)?
            || metadata.len() > MAX_ENCRYPTED_STORE_BYTES
        {
            return Err("项目记忆导入信任存储类型或大小无效".into());
        }
        let header =
            fs::read(&self.path).map_err(|_| "无法读取项目记忆导入信任存储身份".to_owned())?;
        if header.len() < STORE_MAGIC.len() + STORE_ID_BYTES || !header.starts_with(STORE_MAGIC) {
            return Err("项目记忆导入信任存储格式无效".into());
        }
        header[STORE_MAGIC.len()..STORE_MAGIC.len() + STORE_ID_BYTES]
            .try_into()
            .map_err(|_| "项目记忆导入信任存储身份无效".to_owned())
    }

    fn load_master_key(
        &self,
        store_id: &[u8; STORE_ID_BYTES],
        create: bool,
    ) -> Result<Option<Zeroizing<[u8; 32]>>, String> {
        match &self.key_source {
            MasterKeySource::SystemKeyring => {
                let identity = format!("master-key:{}", Uuid::from_bytes(*store_id));
                let entry = keyring::Entry::new(KEYRING_SERVICE, &identity)
                    .map_err(|_| "无法访问项目记忆导入信任系统凭据".to_owned())?;
                match entry.get_password() {
                    Ok(encoded) => {
                        let mut encoded = Zeroizing::new(encoded);
                        let decoded = decode_master_key(&encoded)?;
                        encoded.zeroize();
                        Ok(Some(Zeroizing::new(decoded)))
                    }
                    Err(keyring::Error::NoEntry) if !create => Ok(None),
                    Err(keyring::Error::NoEntry) => {
                        let generated = generate_master_key();
                        let encoded = Zeroizing::new(encode_master_key(generated.as_ref()));
                        entry
                            .set_password(&encoded)
                            .map_err(|_| "无法保存项目记忆导入信任系统凭据".to_owned())?;
                        // There is no keyring CAS. The random per-ledger
                        // identity and cross-process ledger lock serialize
                        // legitimate creators; re-reading here guarantees the
                        // bytes used for encryption are the vault's committed
                        // value rather than our tentative value.
                        let final_encoded = Zeroizing::new(
                            entry
                                .get_password()
                                .map_err(|_| "无法复核项目记忆导入信任系统凭据".to_owned())?,
                        );
                        let final_key = decode_master_key(&final_encoded)?;
                        Ok(Some(Zeroizing::new(final_key)))
                    }
                    Err(_) => Err("无法读取项目记忆导入信任系统凭据".into()),
                }
            }
            #[cfg(test)]
            MasterKeySource::Fixed(key) => Ok(Some(Zeroizing::new(*key))),
            #[cfg(test)]
            MasterKeySource::Missing => Ok(None),
        }
    }
}

impl ProjectImportTrustRepository for EncryptedProjectImportTrustStore {
    fn list_records(&self) -> Result<Vec<StoredProjectImportTrust>, String> {
        // An exclusive lock is intentional even for a read: `with_lock` is the
        // only thing serializing this ledger against a concurrent writer, and a
        // reader that observed a half-written generation would hand out a lease
        // no later check could invalidate.
        self.with_lock(true, || Ok(self.read_ledger_locked()?.records))
    }

    fn insert_records(&self, records: &[StoredProjectImportTrust]) -> Result<bool, String> {
        if records.is_empty() {
            return Ok(true);
        }
        self.with_lock(true, || {
            let mut ledger = self.read_ledger_locked()?;
            if records.iter().any(|candidate| {
                !valid_record_identity(candidate)
                    || ledger.records.iter().any(|existing| {
                        existing.id == candidate.id
                            || (existing.workspace_id == candidate.workspace_id
                                && paths_equal(
                                    &existing.canonical_workspace,
                                    &candidate.canonical_workspace,
                                )
                                && paths_equal(
                                    &existing.canonical_target,
                                    &candidate.canonical_target,
                                ))
                    })
            }) {
                return Ok(false);
            }
            ledger.generation = next_generation(ledger.generation)?;
            ledger.records.extend_from_slice(records);
            self.write_ledger_locked(&ledger)?;
            Ok(true)
        })
    }

    fn revoke_record(&self, workspace_id: &str, record_id: &str) -> Result<bool, String> {
        self.with_lock(true, || {
            let mut ledger = self.read_ledger_locked()?;
            let before = ledger.records.len();
            ledger
                .records
                .retain(|record| !(record.workspace_id == workspace_id && record.id == record_id));
            if ledger.records.len() == before {
                return Ok(false);
            }
            ledger.generation = next_generation(ledger.generation)?;
            self.write_ledger_locked(&ledger)?;
            Ok(true)
        })
    }

    fn acquire_lease(
        &self,
        workspace_id: &str,
        workspace_root: &Path,
    ) -> Result<ProjectImportTrustLease, String> {
        self.with_lock(true, || {
            validate_workspace_id(workspace_id)?;
            let canonical_workspace = canonical_existing_directory(workspace_root)?;
            let ledger = self.read_ledger_locked()?;
            Ok(ProjectImportTrustLease {
                workspace_id: workspace_id.to_owned(),
                canonical_workspace,
                generation: ledger.generation,
            })
        })
    }

    fn lease_is_current(
        &self,
        lease: &ProjectImportTrustLease,
        workspace_id: &str,
        workspace_root: &Path,
    ) -> Result<bool, String> {
        self.with_lock(true, || {
            validate_workspace_id(workspace_id)?;
            let canonical_workspace = canonical_existing_directory(workspace_root)?;
            let ledger = self.read_ledger_locked()?;
            Ok(lease.workspace_id == workspace_id
                && paths_equal(&lease.canonical_workspace, &canonical_workspace)
                && lease.generation == ledger.generation)
        })
    }
}

fn next_generation(generation: u64) -> Result<u64, String> {
    generation
        .checked_add(1)
        .ok_or_else(|| "项目记忆导入信任 generation 已耗尽；拒绝修改现有决策".to_owned())
}

fn encode_persisted_record(record: &StoredProjectImportTrust) -> Result<PersistedRecord, String> {
    Ok(PersistedRecord {
        id: record.id.clone(),
        workspace_id: record.workspace_id.clone(),
        canonical_workspace: encode_path(&record.canonical_workspace),
        canonical_target: encode_path(&record.canonical_target),
        label: record.label.clone(),
        target_kind: record.target_kind.as_str().into(),
        decision: record.decision.as_str().into(),
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
    })
}

fn decode_persisted_record(record: PersistedRecord) -> Result<StoredProjectImportTrust, String> {
    if Uuid::parse_str(&record.id).is_err()
        || record.workspace_id.trim().is_empty()
        || record.workspace_id.len() > 1024
        || record.workspace_id.chars().any(char::is_control)
        || record.label.is_empty()
        || record.label.len() > 256
        || record.label.chars().any(char::is_control)
        || record.created_at.len() > 128
        || record.updated_at.len() > 128
    {
        return Err("项目记忆导入信任记录字段无效".into());
    }
    let canonical_workspace = decode_path(&record.canonical_workspace)?;
    let canonical_target = decode_path(&record.canonical_target)?;
    if !canonical_workspace.is_absolute() || !canonical_target.is_absolute() {
        return Err("项目记忆导入信任记录路径身份无效".into());
    }
    Ok(StoredProjectImportTrust {
        id: record.id,
        workspace_id: record.workspace_id,
        canonical_workspace,
        canonical_target,
        label: record.label,
        target_kind: ProjectImportTargetKind::parse(&record.target_kind)?,
        decision: ProjectImportDecision::parse(&record.decision)?,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

fn generate_master_key() -> Zeroizing<[u8; 32]> {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(first.as_bytes());
    key[16..].copy_from_slice(second.as_bytes());
    Zeroizing::new(key)
}

fn encode_master_key(key: &[u8]) -> String {
    STANDARD_NO_PAD.encode(key)
}

fn decode_master_key(encoded: &str) -> Result<[u8; 32], String> {
    let decoded = Zeroizing::new(
        STANDARD_NO_PAD
            .decode(encoded)
            .map_err(|_| "项目记忆导入信任系统凭据格式无效".to_owned())?,
    );
    decoded
        .as_slice()
        .try_into()
        .map_err(|_| "项目记忆导入信任系统凭据长度无效".to_owned())
}

#[cfg(windows)]
fn encode_path(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;
    let mut bytes = Vec::new();
    for value in path.as_os_str().encode_wide() {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    format!("win16:{}", STANDARD_NO_PAD.encode(bytes))
}

#[cfg(windows)]
fn decode_path(value: &str) -> Result<PathBuf, String> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    let encoded = value
        .strip_prefix("win16:")
        .ok_or_else(|| "项目记忆导入信任路径编码无效".to_owned())?;
    let bytes = STANDARD_NO_PAD
        .decode(encoded)
        .map_err(|_| "项目记忆导入信任路径编码无效".to_owned())?;
    if bytes.len() > 16 * 1024 || bytes.len() % 2 != 0 {
        return Err("项目记忆导入信任路径长度无效".into());
    }
    let wide = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err("项目记忆导入信任路径包含无效字符".into());
    }
    Ok(PathBuf::from(OsString::from_wide(&wide)))
}

#[cfg(unix)]
fn encode_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    format!(
        "unix:{}",
        STANDARD_NO_PAD.encode(path.as_os_str().as_bytes())
    )
}

#[cfg(unix)]
fn decode_path(value: &str) -> Result<PathBuf, String> {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let encoded = value
        .strip_prefix("unix:")
        .ok_or_else(|| "项目记忆导入信任路径编码无效".to_owned())?;
    let bytes = STANDARD_NO_PAD
        .decode(encoded)
        .map_err(|_| "项目记忆导入信任路径编码无效".to_owned())?;
    if bytes.len() > 16 * 1024 || bytes.contains(&0) {
        return Err("项目记忆导入信任路径长度或内容无效".into());
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn symlink_metadata_optional(
    path: &Path,
    error_message: &'static str,
) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(error_message.to_owned()),
    }
}

fn validate_store_directory(path: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "无法检查项目记忆导入信任存储目录".to_owned())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || path_is_reparse_point(path)? {
        return Err("项目记忆导入信任存储目录不安全".into());
    }
    let app_data = path
        .parent()
        .ok_or_else(|| "项目记忆导入信任存储目录缺少应用数据根".to_owned())?;
    let canonical_app_data =
        fs::canonicalize(app_data).map_err(|_| "无法校验项目记忆导入信任应用数据根".to_owned())?;
    let canonical_memory =
        fs::canonicalize(path).map_err(|_| "无法校验项目记忆导入信任存储目录".to_owned())?;
    let expected = canonical_app_data.join(
        path.file_name()
            .ok_or_else(|| "项目记忆导入信任存储目录名称无效".to_owned())?,
    );
    if !paths_equal(&canonical_memory, &expected) {
        return Err("项目记忆导入信任存储目录越过了可信应用数据边界".into());
    }
    Ok(())
}

fn reject_reparse_path(path: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "无法检查项目记忆导入信任存储条目".to_owned())?;
    if metadata.file_type().is_symlink() || path_is_reparse_point(path)? {
        Err("项目记忆导入信任存储条目是链接或重解析点".into())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn path_is_reparse_point(path: &Path) -> Result<bool, String> {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .map_err(|_| "无法检查项目记忆导入信任重解析点".to_owned())
}

#[cfg(not(windows))]
fn path_is_reparse_point(_path: &Path) -> Result<bool, String> {
    Ok(false)
}

fn atomic_write_private(path: &Path, payload: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "项目记忆导入信任存储路径无效".to_owned())?;
    let temporary = parent.join(format!(".project-import-trust.{}.tmp", Uuid::new_v4()));
    atomic_write_private_with_temporary(path, payload, &temporary)
}

fn atomic_write_private_with_temporary(
    path: &Path,
    payload: &[u8],
    temporary: &Path,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "项目记忆导入信任存储路径无效".to_owned())?;
    if temporary.parent() != Some(parent) {
        return Err("项目记忆导入信任临时文件越过了存储目录".into());
    }
    validate_store_directory(parent)?;
    if symlink_metadata_optional(path, "无法检查项目记忆导入信任存储条目")?.is_some()
    {
        reject_reparse_path(path)?;
    }
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(temporary)
            .map_err(|_| "无法创建项目记忆导入信任临时文件".to_owned())?;
        reject_reparse_path(temporary)?;
        file.write_all(payload)
            .map_err(|_| "无法写入项目记忆导入信任临时文件".to_owned())?;
        file.sync_all()
            .map_err(|_| "无法刷新项目记忆导入信任临时文件".to_owned())?;
        make_file_private(temporary)?;
        drop(file);
        replace_file(temporary, path)?;
        make_file_private(path)?;
        sync_directory(parent)?;
        Ok(())
    })();
    if symlink_metadata_optional(temporary, "无法检查项目记忆导入信任临时文件")
        .ok()
        .flatten()
        .is_some()
    {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let mut source_wide = source.as_os_str().encode_wide().collect::<Vec<_>>();
    source_wide.push(0);
    let mut target_wide = target.as_os_str().encode_wide().collect::<Vec<_>>();
    target_wide.push(0);
    let success = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if success == 0 {
        Err("无法原子替换项目记忆导入信任存储".into())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> Result<(), String> {
    fs::rename(source, target).map_err(|_| "无法原子替换项目记忆导入信任存储".to_owned())
}

#[cfg(unix)]
fn make_file_private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| "无法限制项目记忆导入信任存储权限".to_owned())
}

#[cfg(not(unix))]
fn make_file_private(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "无法刷新项目记忆导入信任存储目录".to_owned())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Resolves persistent decisions, asks only about genuinely new canonical
/// targets, persists the answer, and rediscovers from scratch.
///
/// The caller supplies a native-dialog callback. A `false` answer means
/// persistent deny and does not cancel the model run.
pub(crate) fn resolve_project_memory_imports<R, F>(
    repository: &R,
    workspace_id: &str,
    options: &ProjectMemoryOptions,
    mut approve: F,
) -> Result<ProjectImportResolution, String>
where
    R: ProjectImportTrustRepository,
    F: FnMut(&[ProjectImportCandidate]) -> Result<bool, String>,
{
    validate_workspace_id(workspace_id)?;
    let canonical_workspace = canonical_existing_directory(&options.workspace_root)?;
    let mut initial_options = options.clone();
    initial_options.workspace_root = canonical_workspace.clone();
    initial_options.trusted_import_paths.clear();
    let initial_report = project_memory::discover_project_memory(&initial_options);
    if !initial_report
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == ProjectMemoryDiagnosticKind::ImportRequiresTrust)
    {
        return Ok(ProjectImportResolution {
            report: initial_report,
        });
    }
    let records = validated_current_records(
        repository.list_records()?,
        workspace_id,
        &canonical_workspace,
    );
    let mut trusted_paths = records
        .iter()
        .filter(|record| record.decision == ProjectImportDecision::Allowed)
        .map(|record| record.canonical_target.clone())
        .collect::<BTreeSet<_>>();
    let mut denied_paths = records
        .iter()
        .filter(|record| record.decision == ProjectImportDecision::Denied)
        .map(|record| record.canonical_target.clone())
        .collect::<Vec<_>>();

    for _ in 0..MAX_RESOLUTION_ROUNDS {
        let mut resolved_options = options.clone();
        resolved_options.workspace_root = canonical_workspace.clone();
        resolved_options.trusted_import_paths = trusted_paths.clone();
        let report = project_memory::discover_project_memory(&resolved_options);
        let mut pending = pending_targets(&report)?;
        pending.retain(|candidate| {
            !path_is_covered(candidate, &trusted_paths)
                && !denied_paths
                    .iter()
                    .any(|denied| paths_equal(denied, &candidate.canonical_path))
        });
        if pending.is_empty() {
            return Ok(ProjectImportResolution { report });
        }

        pending.truncate(MAX_APPROVAL_TARGETS);
        disambiguate_labels(&mut pending);
        let public = pending
            .iter()
            .map(|candidate| ProjectImportCandidate {
                label: candidate.label.clone(),
                approval_display: approval_target_display(
                    &canonical_workspace,
                    &candidate.canonical_path,
                ),
                target_kind: candidate.target_kind,
            })
            .collect::<Vec<_>>();
        let decision = if approve(&public)? {
            ProjectImportDecision::Allowed
        } else {
            ProjectImportDecision::Denied
        };

        // Close the discover→dialog time-of-check gap. A retargeted symlink or
        // junction must be rediscovered and approved as its new canonical
        // identity, never inherit the stale click.
        for candidate in &pending {
            revalidate_pending_target(candidate)?;
        }

        let timestamp = Utc::now().to_rfc3339();
        let records = pending
            .iter()
            .map(|candidate| StoredProjectImportTrust {
                id: Uuid::new_v4().to_string(),
                workspace_id: workspace_id.to_owned(),
                canonical_workspace: canonical_workspace.clone(),
                canonical_target: candidate.canonical_path.clone(),
                label: candidate.label.clone(),
                target_kind: candidate.target_kind,
                decision,
                created_at: timestamp.clone(),
                updated_at: timestamp.clone(),
            })
            .collect::<Vec<_>>();
        if !repository.insert_records(&records)? {
            let refreshed = validated_current_records(
                repository.list_records()?,
                workspace_id,
                &canonical_workspace,
            );
            trusted_paths = refreshed
                .iter()
                .filter(|record| record.decision == ProjectImportDecision::Allowed)
                .map(|record| record.canonical_target.clone())
                .collect();
            denied_paths = refreshed
                .iter()
                .filter(|record| record.decision == ProjectImportDecision::Denied)
                .map(|record| record.canonical_target.clone())
                .collect();
            continue;
        }
        match decision {
            ProjectImportDecision::Allowed => {
                trusted_paths.extend(records.into_iter().map(|record| record.canonical_target));
            }
            ProjectImportDecision::Denied => {
                denied_paths.extend(records.into_iter().map(|record| record.canonical_target))
            }
        }
    }

    Err("外部项目记忆导入链在单轮中产生了过多新的信任边界；本轮已停止加载".into())
}

pub(crate) fn list_project_import_trust<R: ProjectImportTrustRepository>(
    repository: &R,
    workspace_id: &str,
    workspace_root: Option<&Path>,
) -> Result<Vec<ProjectImportTrustSummary>, String> {
    validate_workspace_id(workspace_id)?;
    let canonical_workspace = workspace_root
        .map(canonical_existing_directory)
        .transpose()?;
    let mut records = repository
        .list_records()?
        .into_iter()
        .filter(|record| record.workspace_id == workspace_id)
        .map(|record| {
            let active = canonical_workspace.as_ref().is_some_and(|workspace| {
                paths_equal(workspace, &record.canonical_workspace)
                    && valid_record_identity(&record)
            });
            record.summary(active)
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.label
            .cmp(&right.label)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(records)
}

pub(crate) fn revoke_project_import_trust<R: ProjectImportTrustRepository>(
    repository: &R,
    workspace_id: &str,
    record_id: &str,
) -> Result<(), String> {
    validate_workspace_id(workspace_id)?;
    if Uuid::parse_str(record_id).is_err() {
        return Err("项目记忆导入信任记录 ID 无效".into());
    }
    if repository.revoke_record(workspace_id, record_id)? {
        Ok(())
    } else {
        Err("找不到指定的项目记忆导入信任记录".into())
    }
}

/// Captures the authenticated trust-ledger revision for the current workspace.
/// The opaque result is backend-only and contains no model-provided identity.
pub(crate) fn acquire_project_import_trust_lease<R: ProjectImportTrustRepository>(
    repository: &R,
    workspace_id: &str,
    workspace_root: &Path,
) -> Result<ProjectImportTrustLease, String> {
    repository.acquire_lease(workspace_id, workspace_root)
}

/// Returns `false` for a changed generation, workspace ID, or canonical root.
/// Authentication, key, and storage failures are returned as errors so callers
/// cannot accidentally treat an unreadable ledger as trusted.
pub(crate) fn project_import_trust_lease_is_current<R: ProjectImportTrustRepository>(
    repository: &R,
    lease: &ProjectImportTrustLease,
    workspace_id: &str,
    workspace_root: &Path,
) -> Result<bool, String> {
    repository.lease_is_current(lease, workspace_id, workspace_root)
}

/// Returns only currently valid allowed canonical identities. Invalid,
/// unavailable, moved, or retargeted records fail closed.
pub(crate) fn trusted_paths_for_workspace<R: ProjectImportTrustRepository>(
    repository: &R,
    workspace_id: &str,
    workspace_root: &Path,
) -> Result<BTreeSet<PathBuf>, String> {
    validate_workspace_id(workspace_id)?;
    let canonical_workspace = canonical_existing_directory(workspace_root)?;
    Ok(validated_current_records(
        repository.list_records()?,
        workspace_id,
        &canonical_workspace,
    )
    .into_iter()
    .filter(|record| record.decision == ProjectImportDecision::Allowed)
    .map(|record| record.canonical_target)
    .collect())
}

fn validated_current_records(
    records: Vec<StoredProjectImportTrust>,
    workspace_id: &str,
    canonical_workspace: &Path,
) -> Vec<StoredProjectImportTrust> {
    records
        .into_iter()
        .filter(|record| {
            record.workspace_id == workspace_id
                && paths_equal(&record.canonical_workspace, canonical_workspace)
                && valid_record_identity(record)
        })
        .collect()
}

fn valid_record_identity(record: &StoredProjectImportTrust) -> bool {
    if Uuid::parse_str(&record.id).is_err()
        || record.label.is_empty()
        || record.label.len() > 256
        || record.label.chars().any(char::is_control)
    {
        return false;
    }
    let Ok(canonical) = fs::canonicalize(&record.canonical_target) else {
        return false;
    };
    if !paths_equal(&canonical, &record.canonical_target) {
        return false;
    }
    fs::metadata(&canonical).is_ok_and(|metadata| match record.target_kind {
        ProjectImportTargetKind::File => metadata.is_file(),
        ProjectImportTargetKind::Directory => metadata.is_dir(),
    })
}

fn pending_targets(report: &ProjectMemoryReport) -> Result<Vec<PendingTarget>, String> {
    let mut paths = report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.kind == ProjectMemoryDiagnosticKind::ImportRequiresTrust)
        .filter_map(|diagnostic| diagnostic.path.as_ref())
        .cloned()
        .collect::<Vec<_>>();
    paths.sort_by(|left, right| path_sort_key(left).cmp(&path_sort_key(right)));
    paths.dedup_by(|left, right| paths_equal(left, right));

    let mut pending = Vec::with_capacity(paths.len());
    for path in paths {
        let canonical = fs::canonicalize(&path)
            .map_err(|_| "外部项目记忆目标在确认前已不可访问；请检查项目后重试".to_owned())?;
        if !paths_equal(&canonical, &path) {
            return Err("外部项目记忆目标在发现期间发生变化；请重试".into());
        }
        let metadata = fs::metadata(&canonical)
            .map_err(|_| "无法安全检查外部项目记忆目标；本轮未加载".to_owned())?;
        let target_kind = if metadata.is_file() {
            ProjectImportTargetKind::File
        } else if metadata.is_dir() {
            ProjectImportTargetKind::Directory
        } else {
            return Err("外部项目记忆目标不是普通文件或目录；本轮未加载".into());
        };
        pending.push(PendingTarget {
            label: safe_target_label(&canonical, target_kind),
            canonical_path: canonical,
            target_kind,
        });
    }
    Ok(pending)
}

fn approval_target_display(workspace: &Path, target: &Path) -> String {
    let mut segments = relative_segments(workspace, target).unwrap_or_else(|| {
        let mut output = vec!["<volume>".to_owned()];
        output.extend(normal_path_segments(target));
        output
    });
    redact_user_segments(&mut segments);
    let mut display = segments.join("/");
    if display.is_empty() {
        display = ".".into();
    }
    if display.chars().count() > 512 {
        let prefix = display.chars().take(240).collect::<String>();
        let suffix = display
            .chars()
            .rev()
            .take(240)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        display = format!("{prefix}/…/{suffix}");
    }
    display
}

fn relative_segments(from: &Path, to: &Path) -> Option<Vec<String>> {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let mut shared = 0usize;
    while shared < from.len() && shared < to.len() && components_equal(from[shared], to[shared]) {
        shared += 1;
    }
    if shared == 0
        || from
            .first()
            .is_some_and(|component| matches!(component, Component::Prefix(_)))
            && shared < 2
    {
        return None;
    }
    let mut output = from[shared..]
        .iter()
        .filter(|component| matches!(component, Component::Normal(_)))
        .map(|_| "..".to_owned())
        .collect::<Vec<_>>();
    output.extend(to[shared..].iter().filter_map(|component| match component {
        Component::Normal(value) => Some(sanitize_display_component(&value.to_string_lossy())),
        Component::ParentDir => Some("..".into()),
        Component::CurDir => Some(".".into()),
        Component::RootDir | Component::Prefix(_) => None,
    }));
    Some(output)
}

fn normal_path_segments(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(sanitize_display_component(&value.to_string_lossy())),
            _ => None,
        })
        .collect()
}

fn components_equal(left: Component<'_>, right: Component<'_>) -> bool {
    if cfg!(windows) {
        left.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
    } else {
        left == right
    }
}

fn sanitize_display_component(value: &str) -> String {
    let mut output = value
        .chars()
        .filter(|character| !character.is_control())
        .take(96)
        .collect::<String>();
    if output.is_empty() {
        output = "<unnamed>".into();
    }
    output
}

fn redact_user_segments(segments: &mut [String]) {
    for index in 1..segments.len() {
        if segments[index - 1].eq_ignore_ascii_case("users")
            || segments[index - 1].eq_ignore_ascii_case("home")
        {
            segments[index] = "<user>".into();
        }
    }
}

fn path_is_covered(candidate: &PendingTarget, trusted_paths: &BTreeSet<PathBuf>) -> bool {
    trusted_paths.iter().any(|trusted| {
        paths_equal(trusted, &candidate.canonical_path)
            || fs::metadata(trusted).is_ok_and(|metadata| {
                metadata.is_dir() && path_is_within(&candidate.canonical_path, trusted)
            })
    })
}

fn revalidate_pending_target(candidate: &PendingTarget) -> Result<(), String> {
    let canonical = fs::canonicalize(&candidate.canonical_path)
        .map_err(|_| "外部项目记忆目标在确认期间已不可访问；未保存授权".to_owned())?;
    if !paths_equal(&canonical, &candidate.canonical_path) {
        return Err("外部项目记忆目标在确认期间发生变化；未保存授权，请重试".into());
    }
    let metadata = fs::metadata(&canonical)
        .map_err(|_| "无法在授权前复核外部项目记忆目标；未保存授权".to_owned())?;
    let valid_kind = match candidate.target_kind {
        ProjectImportTargetKind::File => metadata.is_file(),
        ProjectImportTargetKind::Directory => metadata.is_dir(),
    };
    if !valid_kind {
        return Err("外部项目记忆目标类型在确认期间发生变化；未保存授权".into());
    }
    Ok(())
}

fn safe_target_label(path: &Path, target_kind: ProjectImportTargetKind) -> String {
    let fallback = match target_kind {
        ProjectImportTargetKind::File => "file",
        ProjectImportTargetKind::Directory => "directory",
    };
    let mut leaf = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.to_owned());
    leaf.retain(|character| !character.is_control());
    if leaf.trim().is_empty() {
        leaf = fallback.to_owned();
    }
    leaf = leaf.chars().take(96).collect();
    match target_kind {
        ProjectImportTargetKind::File => format!("external:{leaf}"),
        ProjectImportTargetKind::Directory => format!("external:{leaf}/"),
    }
}

fn disambiguate_labels(pending: &mut [PendingTarget]) {
    let mut totals = HashMap::<String, usize>::new();
    for candidate in pending.iter() {
        *totals.entry(candidate.label.clone()).or_default() += 1;
    }
    let mut seen = HashMap::<String, usize>::new();
    for candidate in pending {
        if totals.get(&candidate.label).copied().unwrap_or_default() <= 1 {
            continue;
        }
        let sequence = seen.entry(candidate.label.clone()).or_default();
        *sequence += 1;
        candidate.label = format!("{} ({})", candidate.label, sequence);
    }
}

fn validate_workspace_id(workspace_id: &str) -> Result<(), String> {
    if workspace_id.trim().is_empty()
        || workspace_id.len() > 1024
        || workspace_id.chars().any(char::is_control)
    {
        Err("工作区 ID 无效".into())
    } else {
        Ok(())
    }
}

fn canonical_existing_directory(path: &Path) -> Result<PathBuf, String> {
    let canonical =
        fs::canonicalize(path).map_err(|_| "项目记忆工作区不存在或无法访问".to_owned())?;
    if canonical.is_dir() {
        Ok(canonical)
    } else {
        Err("项目记忆工作区不是目录".into())
    }
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    if cfg!(windows) {
        let path = path_sort_key(path);
        let root = path_sort_key(root);
        path == root
            || path
                .strip_prefix(&root)
                .is_some_and(|suffix| suffix.starts_with('\\') || suffix.starts_with('/'))
    } else {
        path.starts_with(root)
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        path_sort_key(left) == path_sort_key(right)
    } else {
        left == right
    }
}

fn path_sort_key(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct MemoryRepositoryState {
        generation: u64,
        records: Vec<StoredProjectImportTrust>,
    }

    #[derive(Clone)]
    struct MemoryRepository {
        state: Arc<Mutex<MemoryRepositoryState>>,
        inserts: Arc<Mutex<usize>>,
    }

    impl Default for MemoryRepository {
        fn default() -> Self {
            Self {
                state: Arc::new(Mutex::new(MemoryRepositoryState {
                    generation: INITIAL_GENERATION,
                    records: Vec::new(),
                })),
                inserts: Arc::new(Mutex::new(0)),
            }
        }
    }

    impl ProjectImportTrustRepository for MemoryRepository {
        fn list_records(&self) -> Result<Vec<StoredProjectImportTrust>, String> {
            Ok(self.state.lock().unwrap().records.clone())
        }

        fn insert_records(&self, records: &[StoredProjectImportTrust]) -> Result<bool, String> {
            if records.is_empty() {
                return Ok(true);
            }
            let mut state = self.state.lock().unwrap();
            if records.iter().any(|candidate| {
                state.records.iter().any(|existing| {
                    existing.id == candidate.id
                        || (existing.workspace_id == candidate.workspace_id
                            && paths_equal(
                                &existing.canonical_workspace,
                                &candidate.canonical_workspace,
                            )
                            && paths_equal(&existing.canonical_target, &candidate.canonical_target))
                })
            }) {
                return Ok(false);
            }
            state.generation = next_generation(state.generation)?;
            state.records.extend_from_slice(records);
            *self.inserts.lock().unwrap() += 1;
            Ok(true)
        }

        fn revoke_record(&self, workspace_id: &str, record_id: &str) -> Result<bool, String> {
            let mut state = self.state.lock().unwrap();
            let before = state.records.len();
            state
                .records
                .retain(|record| !(record.workspace_id == workspace_id && record.id == record_id));
            if state.records.len() == before {
                return Ok(false);
            }
            state.generation = next_generation(state.generation)?;
            Ok(true)
        }

        fn acquire_lease(
            &self,
            workspace_id: &str,
            workspace_root: &Path,
        ) -> Result<ProjectImportTrustLease, String> {
            validate_workspace_id(workspace_id)?;
            let canonical_workspace = canonical_existing_directory(workspace_root)?;
            let state = self.state.lock().unwrap();
            Ok(ProjectImportTrustLease {
                workspace_id: workspace_id.to_owned(),
                canonical_workspace,
                generation: state.generation,
            })
        }

        fn lease_is_current(
            &self,
            lease: &ProjectImportTrustLease,
            workspace_id: &str,
            workspace_root: &Path,
        ) -> Result<bool, String> {
            validate_workspace_id(workspace_id)?;
            let canonical_workspace = canonical_existing_directory(workspace_root)?;
            let state = self.state.lock().unwrap();
            Ok(lease.workspace_id == workspace_id
                && paths_equal(&lease.canonical_workspace, &canonical_workspace)
                && lease.generation == state.generation)
        }
    }

    fn options(workspace: &Path) -> ProjectMemoryOptions {
        let mut options = ProjectMemoryOptions::new(workspace);
        options.ancestor_floor = Some(workspace.to_path_buf());
        options
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn make_store_record(
        workspace: &Path,
        target: &Path,
        workspace_id: &str,
        decision: ProjectImportDecision,
    ) -> StoredProjectImportTrust {
        let timestamp = Utc::now().to_rfc3339();
        StoredProjectImportTrust {
            id: Uuid::new_v4().to_string(),
            workspace_id: workspace_id.into(),
            canonical_workspace: fs::canonicalize(&workspace).unwrap(),
            canonical_target: fs::canonicalize(&target).unwrap(),
            label: format!(
                "external:{}",
                target
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("target")
            ),
            target_kind: ProjectImportTargetKind::File,
            decision,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        }
    }

    #[cfg(unix)]
    fn symlink_file(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).is_ok()
    }

    #[cfg(windows)]
    fn symlink_file(target: &Path, link: &Path) -> bool {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }

    #[cfg(unix)]
    fn symlink_directory(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).is_ok()
    }

    #[cfg(windows)]
    fn symlink_directory(target: &Path, link: &Path) -> bool {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
    }

    #[test]
    fn first_external_file_prompts_once_then_persistent_allow_loads_without_prompt() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let external = root.path().join("shared.md");
        write(&workspace.join("MEWORK.md"), "@../shared.md\nworkspace");
        write(&external, "shared instructions");
        let repository = MemoryRepository::default();
        let mut prompts = 0usize;

        let first = resolve_project_memory_imports(
            &repository,
            "workspace-1",
            &options(&workspace),
            |candidates| {
                prompts += 1;
                assert_eq!(
                    candidates,
                    &[ProjectImportCandidate {
                        label: "external:shared.md".into(),
                        approval_display: "../shared.md".into(),
                        target_kind: ProjectImportTargetKind::File,
                    }]
                );
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(prompts, 1);
        assert!(first
            .report
            .included_sources()
            .any(|source| { source.content.as_deref() == Some("shared instructions") }));

        let second = resolve_project_memory_imports(
            &repository,
            "workspace-1",
            &options(&workspace),
            |_| panic!("persisted allow must not prompt again"),
        )
        .unwrap();
        assert!(second
            .report
            .included_sources()
            .any(|source| { source.content.as_deref() == Some("shared instructions") }));
        assert_eq!(*repository.inserts.lock().unwrap(), 1);
    }

    #[test]
    fn denial_is_persistent_and_never_reads_external_content() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let external = root.path().join("private.md");
        write(&workspace.join("MEWORK.md"), "@../private.md\nworkspace");
        write(&external, "must never load");
        let repository = MemoryRepository::default();

        let first = resolve_project_memory_imports(
            &repository,
            "workspace-1",
            &options(&workspace),
            |_| Ok(false),
        )
        .unwrap();
        assert!(first
            .report
            .included_sources()
            .all(|source| source.content.as_deref() != Some("must never load")));

        let second = resolve_project_memory_imports(
            &repository,
            "workspace-1",
            &options(&workspace),
            |_| panic!("persisted denial must not prompt again"),
        )
        .unwrap();
        assert!(second
            .report
            .included_sources()
            .all(|source| source.content.as_deref() != Some("must never load")));
        let summaries =
            list_project_import_trust(&repository, "workspace-1", Some(&workspace)).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].decision, ProjectImportDecision::Denied);
    }

    #[test]
    fn revocation_removes_exact_record_and_causes_a_new_prompt() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        write(&workspace.join("MEWORK.md"), "@../shared.md\n");
        write(&root.path().join("shared.md"), "shared");
        let repository = MemoryRepository::default();
        resolve_project_memory_imports(&repository, "workspace-1", &options(&workspace), |_| {
            Ok(true)
        })
        .unwrap();
        let record = list_project_import_trust(&repository, "workspace-1", Some(&workspace))
            .unwrap()[0]
            .clone();
        revoke_project_import_trust(&repository, "workspace-1", &record.id).unwrap();

        let mut prompted = false;
        resolve_project_memory_imports(&repository, "workspace-1", &options(&workspace), |_| {
            prompted = true;
            Ok(false)
        })
        .unwrap();
        assert!(prompted);
    }

    #[test]
    fn workspace_id_alone_cannot_reuse_decisions_after_root_changes() {
        let root = tempfile::tempdir().unwrap();
        let first_workspace = root.path().join("first");
        let second_workspace = root.path().join("second");
        write(&first_workspace.join("MEWORK.md"), "@../one.md\n");
        write(&second_workspace.join("MEWORK.md"), "@../two.md\n");
        write(&root.path().join("one.md"), "one");
        write(&root.path().join("two.md"), "two");
        let repository = MemoryRepository::default();
        resolve_project_memory_imports(
            &repository,
            "same-workspace-id",
            &options(&first_workspace),
            |_| Ok(true),
        )
        .unwrap();

        let mut prompted = false;
        let second = resolve_project_memory_imports(
            &repository,
            "same-workspace-id",
            &options(&second_workspace),
            |_| {
                prompted = true;
                Ok(false)
            },
        )
        .unwrap();
        assert!(prompted);
        assert!(second
            .report
            .included_sources()
            .all(|source| source.content.as_deref() != Some("two")));
    }

    #[test]
    fn linked_rules_directory_is_approved_as_a_directory_and_covers_descendants() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let rules = root.path().join("external-rules");
        fs::create_dir_all(workspace.join(".mework")).unwrap();
        write(&rules.join("rust.md"), "rust rules");
        if !symlink_directory(&rules, &workspace.join(".mework/rules")) {
            return;
        }
        let repository = MemoryRepository::default();
        let result = resolve_project_memory_imports(
            &repository,
            "workspace-1",
            &options(&workspace),
            |candidates| {
                assert_eq!(candidates.len(), 1);
                assert_eq!(
                    candidates[0].target_kind,
                    ProjectImportTargetKind::Directory
                );
                Ok(true)
            },
        )
        .unwrap();
        assert!(result
            .report
            .included_sources()
            .any(|source| source.content.as_deref() == Some("rust rules")));
    }

    #[test]
    fn symlink_file_authorization_is_bound_to_canonical_target() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let first = root.path().join("first.md");
        let link = workspace.join("linked.md");
        fs::create_dir_all(&workspace).unwrap();
        write(&first, "first");
        if !symlink_file(&first, &link) {
            return;
        }
        write(&workspace.join("MEWORK.md"), "@linked.md\n");
        let repository = MemoryRepository::default();
        resolve_project_memory_imports(&repository, "workspace-1", &options(&workspace), |_| {
            Ok(true)
        })
        .unwrap();

        let stored = repository.state.lock().unwrap();
        assert_eq!(stored.records.len(), 1);
        assert!(paths_equal(
            &stored.records[0].canonical_target,
            &fs::canonicalize(first).unwrap()
        ));
    }

    #[test]
    fn renderer_summaries_contain_no_canonical_paths_or_content() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let external = root.path().join("private-name.md");
        write(&workspace.join("MEWORK.md"), "@../private-name.md\n");
        write(&external, "TOP_SECRET_CONTENT");
        let repository = MemoryRepository::default();
        resolve_project_memory_imports(&repository, "workspace-1", &options(&workspace), |_| {
            Ok(true)
        })
        .unwrap();

        let summaries =
            list_project_import_trust(&repository, "workspace-1", Some(&workspace)).unwrap();
        let serialized = serde_json::to_string(&summaries).unwrap();
        assert!(!serialized.contains(&root.path().to_string_lossy().to_string()));
        assert!(!serialized.contains("TOP_SECRET_CONTENT"));
        assert!(serialized.contains("external:private-name.md"));
    }

    #[test]
    fn approval_display_distinguishes_same_named_targets_without_an_absolute_user_path() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let first = root.path().join("shared-a").join("rules.md");
        let second = root.path().join("shared-b").join("rules.md");
        fs::create_dir_all(&workspace).unwrap();
        write(&first, "first");
        write(&second, "second");

        let first_display = approval_target_display(&workspace, &first);
        let second_display = approval_target_display(&workspace, &second);
        assert_eq!(first_display, "../shared-a/rules.md");
        assert_eq!(second_display, "../shared-b/rules.md");
        assert_ne!(first_display, second_display);
        assert!(!first_display.contains(&root.path().to_string_lossy().to_string()));
        assert!(!second_display.contains(&root.path().to_string_lossy().to_string()));
    }

    #[test]
    fn encrypted_ledger_generation_starts_at_one_and_persists_real_changes() {
        let app_data = tempfile::tempdir().unwrap();
        let workspace = app_data.path().join("workspace");
        let allowed_target = app_data.path().join("allowed.md");
        let denied_target = app_data.path().join("denied.md");
        fs::create_dir_all(&workspace).unwrap();
        write(&allowed_target, "allowed");
        write(&denied_target, "denied");
        let store =
            EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [0x31; 32]);

        let initial =
            acquire_project_import_trust_lease(&store, "workspace-1", &workspace).unwrap();
        assert_eq!(initial.generation, INITIAL_GENERATION);

        let allowed = make_store_record(
            &workspace,
            &allowed_target,
            "workspace-1",
            ProjectImportDecision::Allowed,
        );
        assert!(store.insert_records(&[allowed.clone()]).unwrap());
        let after_allow =
            acquire_project_import_trust_lease(&store, "workspace-1", &workspace).unwrap();
        assert_eq!(after_allow.generation, INITIAL_GENERATION + 1);

        let denied = make_store_record(
            &workspace,
            &denied_target,
            "workspace-1",
            ProjectImportDecision::Denied,
        );
        assert!(store.insert_records(&[denied]).unwrap());
        let after_deny =
            acquire_project_import_trust_lease(&store, "workspace-1", &workspace).unwrap();
        assert_eq!(after_deny.generation, INITIAL_GENERATION + 2);

        assert!(store.revoke_record("workspace-1", &allowed.id).unwrap());
        let after_revoke =
            acquire_project_import_trust_lease(&store, "workspace-1", &workspace).unwrap();
        assert_eq!(after_revoke.generation, INITIAL_GENERATION + 3);

        let reopened =
            EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [0x31; 32]);
        let reopened_lease =
            acquire_project_import_trust_lease(&reopened, "workspace-1", &workspace).unwrap();
        assert_eq!(reopened_lease.generation, after_revoke.generation);
        assert!(project_import_trust_lease_is_current(
            &reopened,
            &after_revoke,
            "workspace-1",
            &workspace
        )
        .unwrap());
    }

    #[test]
    fn ledger_no_ops_do_not_advance_generation() {
        let app_data = tempfile::tempdir().unwrap();
        let workspace = app_data.path().join("workspace");
        let target = app_data.path().join("target.md");
        fs::create_dir_all(&workspace).unwrap();
        write(&target, "target");
        let store =
            EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [0x32; 32]);
        let record = make_store_record(
            &workspace,
            &target,
            "workspace-1",
            ProjectImportDecision::Allowed,
        );
        assert!(store.insert_records(&[record.clone()]).unwrap());
        let stable = acquire_project_import_trust_lease(&store, "workspace-1", &workspace).unwrap();

        assert!(store.insert_records(&[]).unwrap());
        let conflicting = StoredProjectImportTrust {
            id: Uuid::new_v4().to_string(),
            decision: ProjectImportDecision::Denied,
            ..record
        };
        assert!(!store.insert_records(&[conflicting]).unwrap());
        assert!(!store
            .revoke_record("workspace-1", &Uuid::new_v4().to_string())
            .unwrap());

        let after = acquire_project_import_trust_lease(&store, "workspace-1", &workspace).unwrap();
        assert_eq!(after.generation, stable.generation);
        assert!(
            project_import_trust_lease_is_current(&store, &stable, "workspace-1", &workspace)
                .unwrap()
        );
    }

    #[test]
    fn lease_rejects_workspace_id_root_and_generation_mismatches() {
        let app_data = tempfile::tempdir().unwrap();
        let workspace = app_data.path().join("workspace");
        let moved_workspace = app_data.path().join("moved-workspace");
        let target = app_data.path().join("target.md");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&moved_workspace).unwrap();
        write(&target, "target");
        let store =
            EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [0x33; 32]);
        let lease = acquire_project_import_trust_lease(&store, "workspace-1", &workspace).unwrap();

        assert!(
            !project_import_trust_lease_is_current(&store, &lease, "workspace-2", &workspace)
                .unwrap()
        );
        assert!(!project_import_trust_lease_is_current(
            &store,
            &lease,
            "workspace-1",
            &moved_workspace
        )
        .unwrap());

        let record = make_store_record(
            &workspace,
            &target,
            "workspace-1",
            ProjectImportDecision::Allowed,
        );
        assert!(store.insert_records(&[record]).unwrap());
        assert!(
            !project_import_trust_lease_is_current(&store, &lease, "workspace-1", &workspace)
                .unwrap()
        );
    }

    /// Unwraps a refused lease.
    ///
    /// `expect_err` cannot be used here: it needs `Debug` on the success type,
    /// and [`ProjectImportTrustLease`] withholds it on purpose so canonical
    /// paths cannot reach a log or a panic message.
    fn expect_lease_error(
        result: Result<ProjectImportTrustLease, String>,
        reason: &str,
    ) -> String {
        match result {
            Ok(_) => panic!("{reason}"),
            Err(error) => error,
        }
    }

    /// No ledger is migrated in place. A ledger stamped with any other version,
    /// and one shaped like an older layout, are both refused and left on disk
    /// untouched, so a launch that cannot read the ledger trusts no external
    /// import rather than silently reconstructing one. The version is a literal
    /// so that raising it forces this decision to be made again.
    #[test]
    fn a_ledger_from_another_version_is_rejected_rather_than_migrated() {
        assert_eq!(STORE_VERSION, 1, "抬版本时重新判断要不要就地迁移");
        let app_data = tempfile::tempdir().unwrap();
        let workspace = app_data.path().join("workspace");
        let target = app_data.path().join("other-version.md");
        fs::create_dir_all(&workspace).unwrap();
        write(&target, "other version");
        let store =
            EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [0x34; 32]);
        let record = make_store_record(
            &workspace,
            &target,
            "workspace-1",
            ProjectImportDecision::Allowed,
        );

        for other in [STORE_VERSION + 1, STORE_VERSION.wrapping_sub(1)] {
            let foreign = PersistedLedger {
                version: other,
                generation: INITIAL_GENERATION,
                records: vec![encode_persisted_record(&record).unwrap()],
            };
            store
                .with_lock(true, || store.write_persisted_locked(&foreign))
                .unwrap();
            let written = fs::read(&store.path).unwrap();

            let error = expect_lease_error(
                acquire_project_import_trust_lease(&store, "workspace-1", &workspace),
                "另一个版本的账本必须被拒绝",
            );
            assert!(error.contains("版本或 generation 无效"), "{error}");
            assert_eq!(
                fs::read(&store.path).unwrap(),
                written,
                "被拒的账本必须原样留在盘上"
            );
        }
    }

    /// A ledger missing a field this version requires is content, not a shape to
    /// accommodate: the older layout carried no `generation`, and accepting it
    /// would be the in-place migration the version above rules out.
    #[test]
    fn a_ledger_missing_a_required_field_is_rejected() {
        #[derive(Serialize)]
        struct WithoutGeneration {
            version: u32,
            records: Vec<PersistedRecord>,
        }

        let app_data = tempfile::tempdir().unwrap();
        let workspace = app_data.path().join("workspace");
        let target = app_data.path().join("shapeless.md");
        fs::create_dir_all(&workspace).unwrap();
        write(&target, "shapeless");
        let store =
            EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [0x34; 32]);
        let record = make_store_record(
            &workspace,
            &target,
            "workspace-1",
            ProjectImportDecision::Allowed,
        );
        let shapeless = WithoutGeneration {
            version: STORE_VERSION,
            records: vec![encode_persisted_record(&record).unwrap()],
        };
        store
            .with_lock(true, || store.write_persisted_locked(&shapeless))
            .unwrap();

        let error = expect_lease_error(
            acquire_project_import_trust_lease(&store, "workspace-1", &workspace),
            "缺少必需字段的账本必须被拒绝",
        );
        assert!(error.contains("存储内容无效"), "{error}");
    }

    #[test]
    fn exhausted_generation_rejects_mutation_without_overwriting_ledger() {
        let app_data = tempfile::tempdir().unwrap();
        let workspace = app_data.path().join("workspace");
        let target = app_data.path().join("target.md");
        fs::create_dir_all(&workspace).unwrap();
        write(&target, "target");
        let store =
            EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [0x35; 32]);
        store
            .with_lock(true, || {
                store.write_ledger_locked(&TrustLedger {
                    generation: u64::MAX,
                    records: Vec::new(),
                })
            })
            .unwrap();
        let original = fs::read(&store.path).unwrap();
        let record = make_store_record(
            &workspace,
            &target,
            "workspace-1",
            ProjectImportDecision::Allowed,
        );

        assert!(store
            .insert_records(&[record])
            .unwrap_err()
            .contains("已耗尽"));
        assert_eq!(fs::read(&store.path).unwrap(), original);
        assert!(store.list_records().unwrap().is_empty());
    }

    #[test]
    fn encrypted_store_persists_without_plaintext_paths_labels_or_workspace_ids() {
        let app_data = tempfile::tempdir().unwrap();
        let workspace = app_data.path().join("outside-workspace");
        let target = app_data.path().join("outside-target").join("private.md");
        fs::create_dir_all(&workspace).unwrap();
        write(&target, "content is never stored by trust");
        let store =
            EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [0x41; 32]);
        let timestamp = Utc::now().to_rfc3339();
        let record = StoredProjectImportTrust {
            id: Uuid::new_v4().to_string(),
            workspace_id: "workspace-private-id".into(),
            canonical_workspace: fs::canonicalize(&workspace).unwrap(),
            canonical_target: fs::canonicalize(&target).unwrap(),
            label: "external:private.md".into(),
            target_kind: ProjectImportTargetKind::File,
            decision: ProjectImportDecision::Allowed,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        };
        assert!(store.insert_records(&[record.clone()]).unwrap());

        let bytes = fs::read(&store.path).unwrap();
        let lossy = String::from_utf8_lossy(&bytes);
        assert!(!lossy.contains("workspace-private-id"));
        assert!(!lossy.contains("external:private.md"));
        assert!(!lossy.contains(&workspace.to_string_lossy().to_string()));
        assert!(!lossy.contains(&target.to_string_lossy().to_string()));

        let reopened =
            EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [0x41; 32]);
        let loaded = reopened.list_records().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].summary(true), record.summary(true));
        assert!(paths_equal(
            &loaded[0].canonical_target,
            &record.canonical_target
        ));
    }

    #[test]
    fn wrong_key_and_ciphertext_tampering_fail_closed() {
        let app_data = tempfile::tempdir().unwrap();
        let workspace = app_data.path().join("workspace");
        let target = app_data.path().join("target.md");
        fs::create_dir_all(&workspace).unwrap();
        write(&target, "target");
        let store = EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [7; 32]);
        let timestamp = Utc::now().to_rfc3339();
        let record = StoredProjectImportTrust {
            id: Uuid::new_v4().to_string(),
            workspace_id: "workspace-1".into(),
            canonical_workspace: fs::canonicalize(&workspace).unwrap(),
            canonical_target: fs::canonicalize(&target).unwrap(),
            label: "external:target.md".into(),
            target_kind: ProjectImportTargetKind::File,
            decision: ProjectImportDecision::Denied,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        };
        assert!(store.insert_records(&[record]).unwrap());
        let lease = acquire_project_import_trust_lease(&store, "workspace-1", &workspace).unwrap();

        let wrong = EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [8; 32]);
        assert!(wrong
            .list_records()
            .err()
            .expect("wrong key must fail closed")
            .contains("认证失败"));
        assert!(
            project_import_trust_lease_is_current(&wrong, &lease, "workspace-1", &workspace)
                .err()
                .expect("wrong key must not validate a lease")
                .contains("认证失败")
        );

        let mut bytes = fs::read(&store.path).unwrap();
        let last = bytes.last_mut().unwrap();
        *last ^= 0x80;
        fs::write(&store.path, bytes).unwrap();
        assert!(store
            .list_records()
            .err()
            .expect("tampered ledger must fail closed")
            .contains("认证失败"));
        assert!(
            project_import_trust_lease_is_current(&store, &lease, "workspace-1", &workspace)
                .err()
                .expect("tampered ledger must not validate a lease")
                .contains("认证失败")
        );
    }

    #[test]
    fn missing_vault_key_fails_closed_without_replacing_the_store() {
        let app_data = tempfile::tempdir().unwrap();
        let workspace = app_data.path().join("workspace");
        let target = app_data.path().join("target.md");
        fs::create_dir_all(&workspace).unwrap();
        write(&target, "target");
        let store = EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [5; 32]);
        let timestamp = Utc::now().to_rfc3339();
        let record = StoredProjectImportTrust {
            id: Uuid::new_v4().to_string(),
            workspace_id: "workspace-1".into(),
            canonical_workspace: fs::canonicalize(&workspace).unwrap(),
            canonical_target: fs::canonicalize(&target).unwrap(),
            label: "external:target.md".into(),
            target_kind: ProjectImportTargetKind::File,
            decision: ProjectImportDecision::Allowed,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        };
        assert!(store.insert_records(&[record]).unwrap());
        let lease = acquire_project_import_trust_lease(&store, "workspace-1", &workspace).unwrap();
        let original = fs::read(&store.path).unwrap();

        let missing = EncryptedProjectImportTrustStore::open_with_missing_test_key(app_data.path());
        assert!(missing
            .list_records()
            .err()
            .expect("missing key must fail closed")
            .contains("密钥缺失"));
        assert!(
            project_import_trust_lease_is_current(&missing, &lease, "workspace-1", &workspace)
                .err()
                .expect("missing key must not validate a lease")
                .contains("密钥缺失")
        );
        assert_eq!(fs::read(&store.path).unwrap(), original);
    }

    #[test]
    fn repository_collision_never_overwrites_an_existing_decision() {
        let app_data = tempfile::tempdir().unwrap();
        let workspace = app_data.path().join("workspace");
        let target = app_data.path().join("target.md");
        fs::create_dir_all(&workspace).unwrap();
        write(&target, "target");
        let workspace = fs::canonicalize(workspace).unwrap();
        let target = fs::canonicalize(target).unwrap();
        let store = EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [3; 32]);
        let timestamp = Utc::now().to_rfc3339();
        let allowed = StoredProjectImportTrust {
            id: Uuid::new_v4().to_string(),
            workspace_id: "workspace-1".into(),
            canonical_workspace: workspace.clone(),
            canonical_target: target.clone(),
            label: "external:target.md".into(),
            target_kind: ProjectImportTargetKind::File,
            decision: ProjectImportDecision::Allowed,
            created_at: timestamp.clone(),
            updated_at: timestamp.clone(),
        };
        let denied = StoredProjectImportTrust {
            id: Uuid::new_v4().to_string(),
            decision: ProjectImportDecision::Denied,
            ..allowed.clone()
        };
        assert!(store.insert_records(&[allowed]).unwrap());
        assert!(!store.insert_records(&[denied]).unwrap());
        let records = store.list_records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].decision, ProjectImportDecision::Allowed);
    }

    #[test]
    fn concurrent_writers_share_one_locked_ledger_without_lost_updates() {
        let app_data = tempfile::tempdir().unwrap();
        let workspace = app_data.path().join("workspace");
        let first_target = app_data.path().join("first.md");
        let second_target = app_data.path().join("second.md");
        fs::create_dir_all(&workspace).unwrap();
        write(&first_target, "first");
        write(&second_target, "second");
        let workspace = fs::canonicalize(workspace).unwrap();
        let timestamp = Utc::now().to_rfc3339();
        let make_record = |target: PathBuf, label: &str| StoredProjectImportTrust {
            id: Uuid::new_v4().to_string(),
            workspace_id: "workspace-1".into(),
            canonical_workspace: workspace.clone(),
            canonical_target: fs::canonicalize(target).unwrap(),
            label: label.into(),
            target_kind: ProjectImportTargetKind::File,
            decision: ProjectImportDecision::Allowed,
            created_at: timestamp.clone(),
            updated_at: timestamp.clone(),
        };
        let first = make_record(first_target, "external:first.md");
        let second = make_record(second_target, "external:second.md");
        let store = EncryptedProjectImportTrustStore::open_with_test_key(app_data.path(), [9; 32]);
        let initial =
            acquire_project_import_trust_lease(&store, "workspace-1", &workspace).unwrap();
        let left = store.clone();
        let right = store.clone();
        let left_thread = std::thread::spawn(move || left.insert_records(&[first]).unwrap());
        let right_thread = std::thread::spawn(move || right.insert_records(&[second]).unwrap());
        assert!(left_thread.join().unwrap());
        assert!(right_thread.join().unwrap());
        assert_eq!(store.list_records().unwrap().len(), 2);
        let after = acquire_project_import_trust_lease(&store, "workspace-1", &workspace).unwrap();
        assert_eq!(after.generation, INITIAL_GENERATION + 2);
        assert!(!project_import_trust_lease_is_current(
            &store,
            &initial,
            "workspace-1",
            &workspace
        )
        .unwrap());
    }

    #[test]
    fn independent_ledgers_have_random_keyring_identities() {
        let first_data = tempfile::tempdir().unwrap();
        let second_data = tempfile::tempdir().unwrap();
        let create_record = |app_data: &Path, suffix: &str| {
            let workspace = app_data.join("workspace");
            let target = app_data.join(format!("{suffix}.md"));
            fs::create_dir_all(&workspace).unwrap();
            write(&target, suffix);
            let timestamp = Utc::now().to_rfc3339();
            StoredProjectImportTrust {
                id: Uuid::new_v4().to_string(),
                workspace_id: "workspace-1".into(),
                canonical_workspace: fs::canonicalize(workspace).unwrap(),
                canonical_target: fs::canonicalize(target).unwrap(),
                label: format!("external:{suffix}.md"),
                target_kind: ProjectImportTargetKind::File,
                decision: ProjectImportDecision::Allowed,
                created_at: timestamp.clone(),
                updated_at: timestamp,
            }
        };
        let first =
            EncryptedProjectImportTrustStore::open_with_test_key(first_data.path(), [2; 32]);
        let second =
            EncryptedProjectImportTrustStore::open_with_test_key(second_data.path(), [2; 32]);
        assert!(first
            .insert_records(&[create_record(first_data.path(), "one")])
            .unwrap());
        assert!(second
            .insert_records(&[create_record(second_data.path(), "two")])
            .unwrap());
        let first_bytes = fs::read(&first.path).unwrap();
        let second_bytes = fs::read(&second.path).unwrap();
        let range = STORE_MAGIC.len()..STORE_MAGIC.len() + STORE_ID_BYTES;
        assert_ne!(&first_bytes[range.clone()], &second_bytes[range]);
    }

    #[test]
    fn dangling_lock_and_store_symlinks_are_never_treated_as_absent() {
        let lock_root = tempfile::tempdir().unwrap();
        let lock_memory = lock_root.path().join("memory");
        fs::create_dir_all(&lock_memory).unwrap();
        let lock_store =
            EncryptedProjectImportTrustStore::open_with_test_key(lock_root.path(), [0x51; 32]);
        let outside_lock_target = lock_root.path().join("outside").join("missing-lock");
        if !symlink_file(&outside_lock_target, &lock_store.lock_path) {
            return;
        }
        assert!(lock_store
            .list_records()
            .err()
            .expect("dangling lock symlink must fail closed")
            .contains("链接"));
        assert!(!outside_lock_target.exists());

        let store_root = tempfile::tempdir().unwrap();
        let store_memory = store_root.path().join("memory");
        fs::create_dir_all(&store_memory).unwrap();
        let linked_store =
            EncryptedProjectImportTrustStore::open_with_test_key(store_root.path(), [0x52; 32]);
        let outside_store_target = store_root.path().join("outside").join("missing-ledger");
        if !symlink_file(&outside_store_target, &linked_store.path) {
            return;
        }
        assert!(linked_store
            .list_records()
            .err()
            .expect("dangling ledger symlink must fail closed")
            .contains("无效"));
        assert!(!outside_store_target.exists());

        let temporary_root = tempfile::tempdir().unwrap();
        let temporary_memory = temporary_root.path().join("memory");
        fs::create_dir_all(&temporary_memory).unwrap();
        let ledger_path = temporary_memory.join(STORE_FILE_NAME);
        let temporary_path = temporary_memory.join(".project-import-trust.test.tmp");
        let outside_temporary_target = temporary_root.path().join("outside").join("missing-temp");
        if !symlink_file(&outside_temporary_target, &temporary_path) {
            return;
        }
        assert!(
            atomic_write_private_with_temporary(&ledger_path, b"encrypted", &temporary_path)
                .unwrap_err()
                .contains("无法创建")
        );
        assert!(!outside_temporary_target.exists());
        assert!(!ledger_path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_memory_directory_reparse_point_is_rejected() {
        use std::process::Command;
        let root = tempfile::tempdir().unwrap();
        let app_data = root.path().join("app-data");
        let outside = root.path().join("outside");
        fs::create_dir_all(&app_data).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let memory = app_data.join("memory");
        let status = Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                memory.to_string_lossy().as_ref(),
                outside.to_string_lossy().as_ref(),
            ])
            .status()
            .unwrap();
        if !status.success() {
            return;
        }
        let store = EncryptedProjectImportTrustStore::open_with_test_key(&app_data, [1; 32]);
        assert!(store
            .list_records()
            .err()
            .expect("reparse-point store must fail closed")
            .contains("不安全"));
    }
}
