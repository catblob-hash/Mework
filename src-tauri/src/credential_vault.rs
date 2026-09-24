//! macOS credential store: one keychain item, one prompt.
//!
//! The `keyring` crate's native macOS backend stores every entry as its own
//! login-keychain item, and each item's access list names the code signature of
//! the binary that created it. A development build is ad-hoc signed, as is any
//! release not signed with a stable Developer ID, so after every rebuild or
//! update the running binary is a stranger to all of them. Every read, update
//! and delete of every item then raises its own "Mework wants to use your
//! confidential information" dialog, and its "Allow" button covers that single
//! access only. The provider page reads two items per provider (binding and
//! secret), and the Codex panel re-reads them every two seconds while a sign-in
//! is pending, so the dialogs arrive faster than they can be answered. Only
//! "Deny" makes them stop.
//!
//! This backend keeps one random master key in the keychain ("Mework Safe
//! Storage") and every `keyring` entry in a single file sealed under it — the
//! arrangement Chromium and Electron's `safeStorage` use on macOS, and the one
//! `codex_oauth` already uses for its token file. The master key is read at most
//! once per process and then kept in memory, so a new binary costs one dialog,
//! and answering it with "Always Allow" covers every credential Mework holds.
//!
//! A refused dialog is remembered for the rest of the process. Reads that
//! merely display state then fail at once instead of raising the dialog again;
//! an action that genuinely needs a credential (saving, deleting or revealing a
//! key, starting a run) calls [`retry_after_refusal`] first and asks again.
//!
//! The file is shared by every Mework identifier on the machine, as the
//! keychain service was, and every write is a locked read-modify-replace, so
//! the production app and a browser-dev instance never lose each other's
//! entries.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
use keyring::credential::{
    Credential, CredentialApi, CredentialBuilderApi, CredentialPersistence,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

/// The keychain item holding the master key. The service name is what the
/// macOS dialog and Keychain Access show, so it names the product rather than
/// an internal identifier.
const MASTER_KEY_SERVICE: &str = "Mework Safe Storage";
const MASTER_KEY_ACCOUNT: &str = "Mework";
const MAGIC: &[u8] = b"MEWORK-CREDENTIAL-VAULT-v1\0";
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const MAX_VAULT_BYTES: u64 = 4 * 1024 * 1024;

/// Routes every `keyring::Entry` in this process through the vault. Must run
/// before the first entry is created; `keyring` binds an entry to whichever
/// builder was the default when the entry was built.
pub(crate) fn install() {
    keyring::set_default_credential_builder(Box::new(VaultCredentialBuilder));
}

/// Lets the next credential access raise the keychain dialog again after the
/// user refused it. Called by actions the user started and that cannot work
/// without a credential, never by reads that only render status.
pub(crate) fn retry_after_refusal() {
    let mut state = lock_master_key();
    if matches!(*state, MasterKeyState::Refused) {
        *state = MasterKeyState::Unknown;
    }
}

enum MasterKeyState {
    Unknown,
    Ready(Zeroizing<[u8; 32]>),
    Refused,
}

static MASTER_KEY: Mutex<MasterKeyState> = Mutex::new(MasterKeyState::Unknown);

fn lock_master_key() -> std::sync::MutexGuard<'static, MasterKeyState> {
    MASTER_KEY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The master key, from memory after the first successful read. The mutex is
/// held across the keychain call on purpose: concurrent first reads wait for
/// the one dialog instead of stacking a dialog each.
fn master_key(vault: &Vault) -> keyring::Result<Zeroizing<[u8; 32]>> {
    let mut state = lock_master_key();
    match &*state {
        MasterKeyState::Ready(key) => return Ok(key.clone()),
        MasterKeyState::Refused => return Err(refused()),
        MasterKeyState::Unknown => {}
    }
    match load_or_create_master_key(vault) {
        Ok(key) => {
            *state = MasterKeyState::Ready(key.clone());
            Ok(key)
        }
        Err(error) => {
            log_keychain_failure(&error);
            *state = MasterKeyState::Refused;
            Err(refused())
        }
    }
}

fn refused() -> keyring::Error {
    keyring::Error::NoStorageAccess(
        format!(
            "macOS 钥匙串没有允许 Mework 读取「{MASTER_KEY_SERVICE}」。\
             请重新执行刚才的操作，在系统弹窗中输入登录密码后选择「始终允许」"
        )
        .into(),
    )
}

fn log_keychain_failure(error: &keyring::Error) {
    // Never `BadEncoding`'s payload: it is the stored bytes themselves.
    let reason = match error {
        keyring::Error::PlatformFailure(reason) | keyring::Error::NoStorageAccess(reason) => {
            reason.to_string()
        }
        keyring::Error::BadEncoding(_) => "master key is not valid UTF-8".into(),
        other => format!("{other:?}"),
    };
    eprintln!("[credential-vault] keychain master key unavailable: {reason}");
}

fn master_key_entry() -> keyring::Result<keyring::macos::MacCredential> {
    keyring::macos::MacCredential::new_with_target(None, MASTER_KEY_SERVICE, MASTER_KEY_ACCOUNT)
}

fn read_master_key(
    entry: &keyring::macos::MacCredential,
) -> keyring::Result<Zeroizing<[u8; 32]>> {
    let encoded = Zeroizing::new(entry.get_password()?);
    decode_master_key(&encoded).ok_or_else(|| {
        keyring::Error::PlatformFailure(format!("「{MASTER_KEY_SERVICE}」的内容无效").into())
    })
}

fn load_or_create_master_key(vault: &Vault) -> keyring::Result<Zeroizing<[u8; 32]>> {
    let entry = master_key_entry()?;
    match read_master_key(&entry) {
        Err(keyring::Error::NoEntry) => {}
        result => return result,
    }
    // Creation is serialized with every other process through the vault lock,
    // and the item is looked up again under it: another Mework process may have
    // created it between the miss above and taking the lock.
    let _lock = vault.lock()?;
    match read_master_key(&entry) {
        Err(keyring::Error::NoEntry) => {}
        result => return result,
    }
    let key = generate_key();
    let encoded = Zeroizing::new(STANDARD.encode(key.as_ref()));
    entry.set_password(&encoded)?;
    // A vault left behind was sealed under a key that no longer exists (the
    // item was deleted in Keychain Access, or by `reset:data --keys`), so
    // nothing in it can ever be opened again. Starting empty lets the user
    // re-enter their keys instead of failing every read forever.
    vault.discard()?;
    Ok(key)
}

fn decode_master_key(encoded: &str) -> Option<Zeroizing<[u8; 32]>> {
    let decoded = Zeroizing::new(STANDARD.decode(encoded.trim()).ok()?);
    let key: [u8; 32] = decoded.as_slice().try_into().ok()?;
    Some(Zeroizing::new(key))
}

fn generate_key() -> Zeroizing<[u8; 32]> {
    let mut key = Zeroizing::new([0u8; 32]);
    key[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    key[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    key
}

/// The sealed file and its lock. Paths only; every operation re-reads the file,
/// so a write by another process is visible to the next read here.
struct Vault {
    path: PathBuf,
    lock_path: PathBuf,
}

fn shared_vault() -> keyring::Result<&'static Vault> {
    static VAULT: OnceLock<Option<Vault>> = OnceLock::new();
    VAULT
        .get_or_init(|| {
            let directory = dirs::home_dir()?.join(".mework").join("credential-vault");
            Some(Vault::at(&directory))
        })
        .as_ref()
        .ok_or_else(|| store_failure("无法确定用户主目录，凭据库不可用"))
}

fn store_failure(message: &str) -> keyring::Error {
    keyring::Error::PlatformFailure(message.to_owned().into())
}

/// Plaintext of the vault. Secrets are base64 so the JSON stays text; every
/// one is wiped when the contents are dropped.
#[derive(Serialize, Deserialize)]
struct VaultContents {
    version: u32,
    /// service -> user -> base64(secret)
    entries: BTreeMap<String, BTreeMap<String, String>>,
}

impl Drop for VaultContents {
    fn drop(&mut self) {
        for users in self.entries.values_mut() {
            for secret in users.values_mut() {
                secret.zeroize();
            }
        }
    }
}

impl Vault {
    fn at(directory: &Path) -> Self {
        Self {
            path: directory.join("vault.v1.bin"),
            lock_path: directory.join(".lock"),
        }
    }

    fn directory(&self) -> keyring::Result<&Path> {
        self.path
            .parent()
            .ok_or_else(|| store_failure("凭据库路径无效"))
    }

    fn ensure_directory(&self) -> keyring::Result<()> {
        let directory = self.directory()?;
        fs::create_dir_all(directory).map_err(|_| store_failure("无法创建凭据库目录"))?;
        let metadata =
            fs::symlink_metadata(directory).map_err(|_| store_failure("无法检查凭据库目录"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(store_failure("凭据库目录不安全"));
        }
        set_mode(directory, 0o700).map_err(|_| store_failure("无法保护凭据库目录"))
    }

    /// Exclusive cross-process lock for a read-modify-replace. Always the
    /// innermost lock: nothing else is acquired while it is held.
    fn lock(&self) -> keyring::Result<fs::File> {
        self.ensure_directory()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock_path)
            .map_err(|_| store_failure("无法打开凭据库锁文件"))?;
        let metadata = fs::symlink_metadata(&self.lock_path)
            .map_err(|_| store_failure("无法检查凭据库锁文件"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(store_failure("凭据库锁文件不安全"));
        }
        fs2::FileExt::lock_exclusive(&file).map_err(|_| store_failure("无法锁定凭据库"))?;
        Ok(file)
    }

    fn read(&self, key: &[u8; 32]) -> keyring::Result<VaultContents> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(VaultContents::new())
            }
            Err(_) => return Err(store_failure("无法检查凭据库文件")),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(store_failure("凭据库文件不安全"));
        }
        if metadata.len() > MAX_VAULT_BYTES {
            return Err(store_failure("凭据库文件超过大小上限"));
        }
        let sealed = fs::read(&self.path).map_err(|_| store_failure("无法读取凭据库文件"))?;
        open(&sealed, key)
    }

    fn write(&self, key: &[u8; 32], contents: &VaultContents) -> keyring::Result<()> {
        let sealed = seal(contents, key)?;
        if sealed.len() as u64 > MAX_VAULT_BYTES {
            return Err(store_failure("凭据库超过大小上限"));
        }
        self.ensure_directory()?;
        let directory = self.directory()?;
        let temporary = directory.join(format!(".vault.{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = open_private_new(&temporary)
                .map_err(|_| store_failure("无法创建凭据库临时文件"))?;
            file.write_all(&sealed)
                .map_err(|_| store_failure("无法写入凭据库临时文件"))?;
            file.sync_all()
                .map_err(|_| store_failure("无法刷新凭据库临时文件"))?;
            drop(file);
            fs::rename(&temporary, &self.path).map_err(|_| store_failure("无法替换凭据库文件"))
        })();
        let _ = fs::remove_file(&temporary);
        result
    }

    /// Removes a vault that can no longer be opened. Caller holds the lock.
    fn discard(&self) -> keyring::Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(store_failure("无法移除失效的凭据库文件")),
        }
    }

    fn get(&self, key: &[u8; 32], service: &str, user: &str) -> keyring::Result<Vec<u8>> {
        let contents = self.read(key)?;
        let encoded = contents
            .entries
            .get(service)
            .and_then(|users| users.get(user))
            .ok_or(keyring::Error::NoEntry)?;
        STANDARD
            .decode(encoded)
            .map_err(|_| store_failure("凭据库中的条目格式无效"))
    }

    fn set(&self, key: &[u8; 32], service: &str, user: &str, secret: &[u8]) -> keyring::Result<()> {
        let _lock = self.lock()?;
        let mut contents = self.read(key)?;
        contents
            .entries
            .entry(service.to_owned())
            .or_default()
            .insert(user.to_owned(), STANDARD.encode(secret));
        self.write(key, &contents)
    }

    fn delete(&self, key: &[u8; 32], service: &str, user: &str) -> keyring::Result<()> {
        let _lock = self.lock()?;
        let mut contents = self.read(key)?;
        let users = contents
            .entries
            .get_mut(service)
            .ok_or(keyring::Error::NoEntry)?;
        let mut removed = users.remove(user).ok_or(keyring::Error::NoEntry)?;
        removed.zeroize();
        if users.is_empty() {
            contents.entries.remove(service);
        }
        self.write(key, &contents)
    }
}

impl VaultContents {
    fn new() -> Self {
        Self {
            version: 1,
            entries: BTreeMap::new(),
        }
    }
}

fn seal(contents: &VaultContents, key: &[u8; 32]) -> keyring::Result<Zeroizing<Vec<u8>>> {
    let plaintext = Zeroizing::new(
        serde_json::to_vec(contents).map_err(|_| store_failure("无法编码凭据库"))?,
    );
    let nonce_source = Uuid::new_v4();
    let nonce = &nonce_source.as_bytes()[..NONCE_BYTES];
    let ciphertext = ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: &plaintext,
                aad: MAGIC,
            },
        )
        .map_err(|_| store_failure("无法加密凭据库"))?;
    let mut sealed = Zeroizing::new(Vec::with_capacity(
        MAGIC.len() + NONCE_BYTES + ciphertext.len(),
    ));
    sealed.extend_from_slice(MAGIC);
    sealed.extend_from_slice(nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
}

fn open(sealed: &[u8], key: &[u8; 32]) -> keyring::Result<VaultContents> {
    if sealed.len() < MAGIC.len() + NONCE_BYTES + TAG_BYTES || !sealed.starts_with(MAGIC) {
        return Err(store_failure("凭据库文件格式无效"));
    }
    let nonce_end = MAGIC.len() + NONCE_BYTES;
    let plaintext = Zeroizing::new(
        ChaCha20Poly1305::new(Key::from_slice(key))
            .decrypt(
                Nonce::from_slice(&sealed[MAGIC.len()..nonce_end]),
                Payload {
                    msg: &sealed[nonce_end..],
                    aad: MAGIC,
                },
            )
            .map_err(|_| store_failure("凭据库认证失败"))?,
    );
    let contents: VaultContents =
        serde_json::from_slice(&plaintext).map_err(|_| store_failure("凭据库内容无效"))?;
    if contents.version != 1 {
        return Err(store_failure("凭据库版本不受支持"));
    }
    Ok(contents)
}

fn open_private_new(path: &Path) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

struct VaultCredentialBuilder;

impl CredentialBuilderApi for VaultCredentialBuilder {
    fn build(
        &self,
        _target: Option<&str>,
        service: &str,
        user: &str,
    ) -> keyring::Result<Box<Credential>> {
        // Same rule as the native backend: an empty attribute is a wildcard in
        // Keychain Services, and entries must stay addressable one at a time.
        if service.is_empty() || user.is_empty() {
            return Err(keyring::Error::Invalid(
                if service.is_empty() { "service" } else { "user" }.into(),
                "cannot be empty".into(),
            ));
        }
        Ok(Box::new(VaultCredential {
            service: service.to_owned(),
            user: user.to_owned(),
        }))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn persistence(&self) -> CredentialPersistence {
        CredentialPersistence::UntilDelete
    }
}

#[derive(Debug)]
struct VaultCredential {
    service: String,
    user: String,
}

impl CredentialApi for VaultCredential {
    fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
        // Writes only ever come from something the user did.
        retry_after_refusal();
        let vault = shared_vault()?;
        let key = master_key(vault)?;
        vault.set(&key, &self.service, &self.user, secret)
    }

    fn get_secret(&self) -> keyring::Result<Vec<u8>> {
        let vault = shared_vault()?;
        let key = master_key(vault)?;
        vault.get(&key, &self.service, &self.user)
    }

    fn delete_credential(&self) -> keyring::Result<()> {
        retry_after_refusal();
        let vault = shared_vault()?;
        let key = master_key(vault)?;
        vault.delete(&key, &self.service, &self.user)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    fn vault() -> (tempfile::TempDir, Vault) {
        let root = tempfile::tempdir().unwrap();
        let vault = Vault::at(&root.path().join("credential-vault"));
        (root, vault)
    }

    #[test]
    fn entries_round_trip_and_stay_separate() {
        let (_root, vault) = vault();
        assert!(matches!(
            vault.get(&KEY, "com.mework.api", "a"),
            Err(keyring::Error::NoEntry)
        ));
        vault.set(&KEY, "com.mework.api", "a", b"secret-a").unwrap();
        vault.set(&KEY, "com.mework.api", "b", b"secret-b").unwrap();
        vault.set(&KEY, "other", "a", b"secret-c").unwrap();
        assert_eq!(vault.get(&KEY, "com.mework.api", "a").unwrap(), b"secret-a");
        assert_eq!(vault.get(&KEY, "com.mework.api", "b").unwrap(), b"secret-b");
        assert_eq!(vault.get(&KEY, "other", "a").unwrap(), b"secret-c");

        vault.set(&KEY, "com.mework.api", "a", b"replaced").unwrap();
        assert_eq!(vault.get(&KEY, "com.mework.api", "a").unwrap(), b"replaced");

        vault.delete(&KEY, "com.mework.api", "a").unwrap();
        assert!(matches!(
            vault.get(&KEY, "com.mework.api", "a"),
            Err(keyring::Error::NoEntry)
        ));
        assert!(matches!(
            vault.delete(&KEY, "com.mework.api", "a"),
            Err(keyring::Error::NoEntry)
        ));
        assert_eq!(vault.get(&KEY, "com.mework.api", "b").unwrap(), b"secret-b");
    }

    #[test]
    fn the_file_is_sealed_and_private() {
        let (_root, vault) = vault();
        vault.set(&KEY, "svc", "user", b"plaintext-marker").unwrap();
        let sealed = fs::read(&vault.path).unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert!(!sealed
            .windows(b"plaintext-marker".len())
            .any(|window| window == b"plaintext-marker"));
        assert!(!sealed.windows(b"svc".len()).any(|window| window == b"svc"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_mode = fs::metadata(&vault.path).unwrap().permissions().mode() & 0o777;
            let directory_mode =
                fs::metadata(vault.directory().unwrap()).unwrap().permissions().mode() & 0o777;
            assert_eq!(file_mode, 0o600);
            assert_eq!(directory_mode, 0o700);
        }
    }

    #[test]
    fn a_different_key_or_a_tampered_file_cannot_be_read() {
        let (_root, vault) = vault();
        vault.set(&KEY, "svc", "user", b"secret").unwrap();
        assert!(vault.get(&[8; 32], "svc", "user").is_err());

        let mut sealed = fs::read(&vault.path).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        fs::write(&vault.path, &sealed).unwrap();
        assert!(matches!(
            vault.get(&KEY, "svc", "user"),
            Err(keyring::Error::PlatformFailure(_))
        ));
    }

    #[test]
    fn discarding_starts_an_empty_vault() {
        let (_root, vault) = vault();
        vault.set(&KEY, "svc", "user", b"secret").unwrap();
        vault.discard().unwrap();
        vault.discard().unwrap();
        assert!(matches!(
            vault.get(&[9; 32], "svc", "user"),
            Err(keyring::Error::NoEntry)
        ));
    }

    #[test]
    fn concurrent_writers_keep_every_entry() {
        let (_root, vault) = vault();
        let vault = std::sync::Arc::new(vault);
        let writers: Vec<_> = (0..8)
            .map(|index| {
                let vault = std::sync::Arc::clone(&vault);
                std::thread::spawn(move || {
                    vault
                        .set(&KEY, "svc", &format!("user-{index}"), b"secret")
                        .unwrap();
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        for index in 0..8 {
            assert_eq!(
                vault.get(&KEY, "svc", &format!("user-{index}")).unwrap(),
                b"secret"
            );
        }
    }

    #[test]
    fn a_master_key_round_trips_through_its_text_form() {
        let key = generate_key();
        let encoded = STANDARD.encode(key.as_ref());
        assert_eq!(decode_master_key(&encoded).unwrap().as_ref(), key.as_ref());
        assert!(decode_master_key("not base64").is_none());
        assert!(decode_master_key(&STANDARD.encode([0u8; 16])).is_none());
    }

    #[test]
    fn a_refusal_is_remembered_until_retried() {
        *lock_master_key() = MasterKeyState::Refused;
        let (_root, vault) = vault();
        assert!(matches!(
            master_key(&vault),
            Err(keyring::Error::NoStorageAccess(_))
        ));
        retry_after_refusal();
        assert!(matches!(*lock_master_key(), MasterKeyState::Unknown));
    }
}
