use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

pub const CODEX_DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
/// `client_version` sent to `GET /models`; the backend hides models whose `minimal_client_version` is newer.
pub const CODEX_MODELS_CLIENT_VERSION: &str = "0.153.4";
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

const KEY_PREFIX: &str = "codex-oauth:v1:";
const MAGIC: &[u8] = b"MEWORK_CODEX_OAUTH\x00\x01";
const NONCE_BYTES: usize = 12;
const MAX_TOKEN_FILE_BYTES: u64 = 1024 * 1024;
const EXPIRY_SKEW_SECONDS: u64 = 300;
const UNKNOWN_EXPIRY_REFRESH_AGE_SECONDS: i64 = 8 * 24 * 60 * 60;
/// No second refresh within this window of a successful one, whatever the new token's `exp` says.
const RECENT_REFRESH_FLOOR_SECONDS: i64 = 60;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexAccount {
    pub account_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexOauthStatus {
    pub signed_in: bool,
    pub signing_in: bool,
    pub account: Option<CodexAccount>,
}

/// `Debug` is deliberately absent: the token must not reach logs by accident.
pub struct CodexCredentials {
    pub access_token: Zeroizing<String>,
    pub account_id: String,
}

#[derive(Clone, Debug)]
pub struct CodexOauthConfig {
    pub issuer: String,
    pub client_id: String,
    pub scope: String,
    pub callback_ports: Vec<u16>,
    pub sign_in_timeout: Duration,
    pub token_http_timeout: Duration,
}

impl Default for CodexOauthConfig {
    fn default() -> Self {
        Self {
            issuer: "https://auth.openai.com".to_owned(),
            client_id: CODEX_CLIENT_ID.to_owned(),
            scope: "openid profile email offline_access".to_owned(),
            callback_ports: vec![1455, 1457],
            sign_in_timeout: Duration::from_secs(10 * 60),
            token_http_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Default)]
struct ProviderRuntime {
    active_sign_in: Option<Arc<AtomicBool>>,
    cached_expiry: Option<u64>,
    force_refresh: bool,
    refresh_lock: Arc<Mutex<()>>,
}

pub struct CodexOauthHost {
    store_root: PathBuf,
    config: CodexOauthConfig,
    runtimes: Mutex<HashMap<String, ProviderRuntime>>,
}

impl CodexOauthHost {
    pub fn new(store_root: PathBuf, config: CodexOauthConfig) -> Self {
        Self {
            store_root,
            config,
            runtimes: Mutex::new(HashMap::new()),
        }
    }

    /// Blocks until the browser callback completes, is cancelled, or times out.
    pub fn sign_in(
        &self,
        provider_id: &str,
        open_browser: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<CodexOauthStatus, String> {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut runtimes = lock(&self.runtimes)?;
            let runtime = runtimes.entry(provider_id.to_owned()).or_default();
            if runtime.active_sign_in.is_some() {
                return Err("Codex 登录正在进行中".into());
            }
            runtime.active_sign_in = Some(Arc::clone(&cancel));
        }
        let _guard = SignInGuard {
            host: self,
            provider_id: provider_id.to_owned(),
        };

        let (listener_v4, callback_port) = self.bind_callback_listener()?;
        listener_v4
            .set_nonblocking(true)
            .map_err(|_| "无法配置 Codex 登录回调监听器".to_owned())?;
        let listener_v6 = TcpListener::bind(("::1", callback_port))
            .ok()
            .and_then(|listener| listener.set_nonblocking(true).ok().map(|_| listener));

        let verifier = pkce_verifier();
        let challenge = pkce_challenge(&verifier);
        let state = random_urlsafe(32);
        let redirect_uri = format!("http://localhost:{callback_port}/auth/callback");
        let authorize_url = self.authorize_url(&redirect_uri, &challenge, &state)?;
        open_browser(&authorize_url)?;

        let deadline = Instant::now() + self.config.sign_in_timeout;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err("Codex 登录已取消".into());
            }
            if Instant::now() >= deadline {
                return Err("Codex 登录超时（10 分钟内未收到浏览器回调）".into());
            }

            for listener in std::iter::once(&listener_v4).chain(listener_v6.iter()) {
                if let Some(action) = poll_callback(listener, &state)? {
                    match action {
                        CallbackAction::Continue => {}
                        CallbackAction::Fail(error) => return Err(error),
                        CallbackAction::Code(code, mut stream) => {
                            let exchanged = self.exchange_code(&code, &redirect_uri, &verifier);
                            match exchanged {
                                Ok(tokens) => {
                                    let account = account_from_tokens(&tokens)?;
                                    // A sign-out (or cancel) that landed during the
                                    // exchange wins: the code is spent, nothing is stored.
                                    if cancel.load(Ordering::Acquire) {
                                        let _ = write_response(
                                            &mut stream,
                                            400,
                                            "Bad Request",
                                            &callback_page(
                                                SIGN_IN_FAILED_PAGE_TITLE,
                                                "登录已在 Mework 里取消。/ The sign-in was cancelled in Mework.",
                                            ),
                                            "text/html; charset=utf-8",
                                        );
                                        return Err("Codex 登录已取消".into());
                                    }
                                    if let Err(error) = self.persist_tokens(provider_id, &tokens) {
                                        let _ = write_response(
                                            &mut stream,
                                            500,
                                            "Internal Server Error",
                                            &callback_page(
                                                SIGN_IN_FAILED_PAGE_TITLE,
                                                SIGN_IN_FAILED_PAGE_DETAIL,
                                            ),
                                            "text/html; charset=utf-8",
                                        );
                                        return Err(error);
                                    }
                                    self.set_cached_expiry(
                                        provider_id,
                                        token_expiry(&tokens.access_token),
                                    );
                                    let _ = write_response(
                                        &mut stream,
                                        200,
                                        "OK",
                                        &callback_page(
                                            "登录成功 / Signed in",
                                            "可以关闭此页面回到 Mework。/ You can close this tab and return to Mework.",
                                        ),
                                        "text/html; charset=utf-8",
                                    );
                                    return Ok(CodexOauthStatus {
                                        signed_in: true,
                                        signing_in: false,
                                        account: Some(account),
                                    });
                                }
                                Err(error) => {
                                    let _ = write_response(
                                        &mut stream,
                                        500,
                                        "Internal Server Error",
                                        &callback_page(
                                            SIGN_IN_FAILED_PAGE_TITLE,
                                            SIGN_IN_FAILED_PAGE_DETAIL,
                                        ),
                                        "text/html; charset=utf-8",
                                    );
                                    return Err(error);
                                }
                            }
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn cancel_sign_in(&self, provider_id: &str) -> Result<(), String> {
        if let Some(cancel) = lock(&self.runtimes)?
            .get(provider_id)
            .and_then(|runtime| runtime.active_sign_in.as_ref())
        {
            cancel.store(true, Ordering::Release);
        }
        Ok(())
    }

    pub fn status(&self, provider_id: &str) -> Result<CodexOauthStatus, String> {
        let signing_in = lock(&self.runtimes)?
            .get(provider_id)
            .is_some_and(|runtime| runtime.active_sign_in.is_some());
        // Reads never clean up. A sign-in commits the key before the file, and
        // the renderer polls this while it runs; a read that deleted "orphaned"
        // state would race that commit and destroy the session being created.
        // Inconsistent state simply reads as signed out until the next sign-in
        // overwrites it or a sign-out removes it.
        let tokens = match self.read_session(provider_id) {
            Ok(Some(tokens)) => tokens,
            Ok(None) | Err(_) => {
                return Ok(CodexOauthStatus {
                    signed_in: false,
                    signing_in,
                    account: None,
                })
            }
        };
        Ok(CodexOauthStatus {
            signed_in: true,
            signing_in,
            account: Some(CodexAccount {
                account_id: tokens.account_id,
                email: tokens.email,
                plan_type: tokens.plan_type,
            }),
        })
    }

    /// Remove the session. A sign-in still in flight is cancelled first so its
    /// pending code exchange cannot re-create the session a moment later; the
    /// commit path re-checks that flag right before it writes.
    pub fn sign_out(&self, provider_id: &str) -> Result<CodexOauthStatus, String> {
        self.cancel_sign_in(provider_id)?;
        self.remove(provider_id)?;
        crate::api::delete_api_key(provider_id)?;
        self.clear_runtime_session(provider_id)?;
        self.status(provider_id)
    }

    pub fn credentials(&self, provider_id: &str) -> Result<CodexCredentials, String> {
        let tokens = self
            .read_session(provider_id)
            .map_err(|_| "尚未登录 ChatGPT；请在提供商设置里为 OpenAI Codex 完成登录".to_owned())?
            .ok_or_else(|| {
                "尚未登录 ChatGPT；请在提供商设置里为 OpenAI Codex 完成登录".to_owned()
            })?;
        if !self.refresh_needed(provider_id, &tokens)? {
            return Ok(credentials_for(&tokens));
        }

        let refresh_lock = {
            let mut runtimes = lock(&self.runtimes)?;
            Arc::clone(
                &runtimes
                    .entry(provider_id.to_owned())
                    .or_default()
                    .refresh_lock,
            )
        };
        let _refresh_guard = lock(&refresh_lock)?;
        let current = self
            .read_session(provider_id)
            .map_err(|_| "尚未登录 ChatGPT；请在提供商设置里为 OpenAI Codex 完成登录".to_owned())?
            .ok_or_else(|| {
                "尚未登录 ChatGPT；请在提供商设置里为 OpenAI Codex 完成登录".to_owned()
            })?;
        if !self.refresh_needed(provider_id, &current)? {
            return Ok(credentials_for(&current));
        }

        match self.refresh_tokens(&current) {
            Ok(refreshed) => {
                self.write_existing_session(provider_id, &refreshed)?;
                self.set_cached_expiry(provider_id, token_expiry(&refreshed.access_token));
                {
                    let mut runtimes = lock(&self.runtimes)?;
                    if let Some(runtime) = runtimes.get_mut(provider_id) {
                        runtime.force_refresh = false;
                    }
                }
                Ok(credentials_for(&refreshed))
            }
            Err(RefreshFailure::Terminal) => {
                let _ = self.sign_out(provider_id);
                Err("Codex 登录已失效，请重新登录".into())
            }
            Err(RefreshFailure::Temporary) => Err("刷新 Codex 登录令牌暂时失败，请稍后重试".into()),
        }
    }

    pub fn invalidate_access_token(&self, provider_id: &str) {
        if let Ok(mut runtimes) = self.runtimes.lock() {
            let runtime = runtimes.entry(provider_id.to_owned()).or_default();
            runtime.cached_expiry = None;
            runtime.force_refresh = true;
        }
    }

    pub fn remove(&self, provider_id: &str) -> Result<(), String> {
        let path = self.token_path(provider_id);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || path_is_reparse_point(&path)?
                {
                    return Err("Codex 登录令牌文件不安全".into());
                }
                fs::remove_file(path).map_err(|_| "无法删除 Codex 登录令牌文件".to_owned())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("无法检查 Codex 登录令牌文件".into()),
        }
    }

    fn bind_callback_listener(&self) -> Result<(TcpListener, u16), String> {
        for port in &self.config.callback_ports {
            if let Ok(listener) = TcpListener::bind(("127.0.0.1", *port)) {
                let port = listener
                    .local_addr()
                    .map_err(|_| "无法确定 Codex 登录回调端口".to_owned())?
                    .port();
                return Ok((listener, port));
            }
        }
        Err("无法监听 localhost:1455/1457 用于接收登录回调；请关闭占用该端口的程序后重试".into())
    }

    fn authorize_url(
        &self,
        redirect_uri: &str,
        challenge: &str,
        state: &str,
    ) -> Result<String, String> {
        let mut url = Url::parse(&format!(
            "{}/oauth/authorize",
            self.config.issuer.trim_end_matches('/')
        ))
        .map_err(|_| "Codex OAuth 授权地址无效".to_owned())?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &self.config.scope)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("state", state)
            .append_pair("originator", "mework");
        Ok(url.into())
    }

    fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<StoredTokens, String> {
        let endpoint = format!("{}/oauth/token", self.config.issuer.trim_end_matches('/'));
        let response = reqwest::blocking::Client::builder()
            .timeout(self.config.token_http_timeout)
            .build()
            .map_err(|_| "无法创建 Codex OAuth 网络客户端".to_owned())?
            .post(endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", self.config.client_id.as_str()),
                ("code_verifier", verifier),
            ])
            .send()
            .map_err(|_| "换取 Codex 令牌失败（网络错误）".to_owned())?;
        if !response.status().is_success() {
            return Err(format!("换取 Codex 令牌失败（HTTP {}）", response.status()));
        }
        let response: TokenExchangeResponse = response
            .json()
            .map_err(|_| "换取 Codex 令牌失败（响应格式无效）".to_owned())?;
        let mut tokens = StoredTokens {
            version: 1,
            id_token: Some(response.id_token),
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            account_id: String::new(),
            email: None,
            plan_type: None,
            last_refresh: Utc::now().to_rfc3339(),
        };
        let account = account_from_tokens(&tokens)?;
        tokens.account_id = account.account_id;
        tokens.email = account.email;
        tokens.plan_type = account.plan_type;
        Ok(tokens)
    }

    fn persist_tokens(&self, provider_id: &str, tokens: &StoredTokens) -> Result<(), String> {
        let key = generate_key();
        let encoded = Zeroizing::new(format!(
            "{KEY_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(key.as_ref())
        ));
        crate::api::save_api_key(provider_id, &encoded)?;
        if let Err(error) = self.write_tokens(provider_id, tokens, &key) {
            let _ = crate::api::delete_api_key(provider_id);
            return Err(error);
        }
        Ok(())
    }

    fn write_existing_session(
        &self,
        provider_id: &str,
        tokens: &StoredTokens,
    ) -> Result<(), String> {
        let key = self
            .session_key(provider_id)?
            .ok_or_else(|| "Codex 登录已失效，请重新登录".to_owned())?;
        self.write_tokens(provider_id, tokens, &key)
    }

    /// Decrypt the stored session. `Ok(None)` is "no session"; `Err` is a
    /// session that exists but cannot be read. Neither outcome deletes anything
    /// (see `status`): cleanup belongs to `sign_out` and to the next sign-in.
    fn read_session(&self, provider_id: &str) -> Result<Option<StoredTokens>, String> {
        let Some(key) = self.session_key(provider_id)? else {
            return Ok(None);
        };
        let path = self.token_path(provider_id);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("无法检查 Codex 登录令牌文件".into()),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || path_is_reparse_point(&path)?
            || metadata.len() > MAX_TOKEN_FILE_BYTES
        {
            return Err("Codex 登录令牌文件无效".into());
        }
        let encrypted = fs::read(path).map_err(|_| "无法读取 Codex 登录令牌文件".to_owned())?;
        if encrypted.len() < MAGIC.len() + NONCE_BYTES + 16 || !encrypted.starts_with(MAGIC) {
            return Err("Codex 登录令牌文件格式无效".into());
        }
        let nonce_end = MAGIC.len() + NONCE_BYTES;
        let mut aad = Vec::with_capacity(MAGIC.len() + provider_id.len());
        aad.extend_from_slice(MAGIC);
        aad.extend_from_slice(provider_id.as_bytes());
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key.as_ref()));
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&encrypted[MAGIC.len()..nonce_end]),
                Payload {
                    msg: &encrypted[nonce_end..],
                    aad: &aad,
                },
            )
            .map_err(|_| "Codex 登录令牌认证失败".to_owned())?;
        let plaintext = Zeroizing::new(plaintext);
        let tokens: StoredTokens = serde_json::from_slice(&plaintext)
            .map_err(|_| "Codex 登录令牌内容无效".to_owned())?;
        if tokens.version != 1
            || tokens.access_token.is_empty()
            || tokens.refresh_token.is_empty()
            || tokens.account_id.is_empty()
        {
            return Err("Codex 登录令牌内容无效".into());
        }
        Ok(Some(tokens))
    }

    fn session_key(&self, provider_id: &str) -> Result<Option<Zeroizing<[u8; 32]>>, String> {
        let encoded = match crate::api::reveal_api_key(provider_id) {
            Ok(value) => Zeroizing::new(value),
            Err(_) => return Ok(None),
        };
        let Some(value) = encoded.strip_prefix(KEY_PREFIX) else {
            return Ok(None);
        };
        let decoded = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(value.trim_end_matches('='))
                .map_err(|_| "Codex 登录密钥格式无效".to_owned())?,
        );
        let key: [u8; 32] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| "Codex 登录密钥长度无效".to_owned())?;
        Ok(Some(Zeroizing::new(key)))
    }

    fn write_tokens(
        &self,
        provider_id: &str,
        tokens: &StoredTokens,
        key: &[u8; 32],
    ) -> Result<(), String> {
        ensure_store_dir(&self.store_root)?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(tokens).map_err(|_| "无法编码 Codex 登录令牌".to_owned())?,
        );
        let nonce_uuid = Uuid::new_v4();
        let nonce = &nonce_uuid.as_bytes()[..NONCE_BYTES];
        let mut aad = Vec::with_capacity(MAGIC.len() + provider_id.len());
        aad.extend_from_slice(MAGIC);
        aad.extend_from_slice(provider_id.as_bytes());
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| "无法加密 Codex 登录令牌".to_owned())?;
        let mut envelope = Zeroizing::new(Vec::with_capacity(
            MAGIC.len() + nonce.len() + ciphertext.len(),
        ));
        envelope.extend_from_slice(MAGIC);
        envelope.extend_from_slice(nonce);
        envelope.extend_from_slice(&ciphertext);
        if envelope.len() as u64 > MAX_TOKEN_FILE_BYTES {
            return Err("Codex 登录令牌超过安全大小上限".into());
        }
        atomic_write_private(&self.token_path(provider_id), &envelope)
    }

    fn refresh_needed(&self, provider_id: &str, tokens: &StoredTokens) -> Result<bool, String> {
        let (force, cached_expiry) = {
            let runtimes = lock(&self.runtimes)?;
            let runtime = runtimes.get(provider_id);
            (
                runtime.is_some_and(|runtime| runtime.force_refresh),
                runtime.and_then(|runtime| runtime.cached_expiry),
            )
        };
        if force {
            return Ok(true);
        }
        let last_refresh_age = DateTime::parse_from_rfc3339(&tokens.last_refresh)
            .ok()
            .map(|then| {
                Utc::now()
                    .signed_duration_since(then.with_timezone(&Utc))
                    .num_seconds()
            });
        // A token minted moments ago is the freshest the issuer will hand out.
        // Without this floor an issuer that returns tokens already inside the
        // expiry skew would be asked again on every single request.
        if last_refresh_age.is_some_and(|age| (0..RECENT_REFRESH_FLOOR_SECONDS).contains(&age)) {
            return Ok(false);
        }
        let expiry = cached_expiry.or_else(|| token_expiry(&tokens.access_token));
        if let Some(expiry) = expiry {
            self.set_cached_expiry(provider_id, Some(expiry));
            return Ok(expiry.saturating_sub(now_seconds()) < EXPIRY_SKEW_SECONDS);
        }
        Ok(match last_refresh_age {
            Some(age) => age >= UNKNOWN_EXPIRY_REFRESH_AGE_SECONDS,
            None => true,
        })
    }

    fn refresh_tokens(&self, tokens: &StoredTokens) -> Result<StoredTokens, RefreshFailure> {
        let endpoint = format!("{}/oauth/token", self.config.issuer.trim_end_matches('/'));
        let response = reqwest::blocking::Client::builder()
            .timeout(self.config.token_http_timeout)
            .build()
            .map_err(|_| RefreshFailure::Temporary)?
            .post(endpoint)
            .json(&serde_json::json!({
                "client_id": self.config.client_id.as_str(),
                "grant_type": "refresh_token",
                "refresh_token": tokens.refresh_token.as_str(),
            }))
            .send()
            .map_err(|_| RefreshFailure::Temporary)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            let terminal = status == reqwest::StatusCode::UNAUTHORIZED
                || (status == reqwest::StatusCode::BAD_REQUEST
                    && [
                        "invalid_grant",
                        "refresh_token_expired",
                        "refresh_token_reused",
                        "refresh_token_invalidated",
                    ]
                    .iter()
                    .any(|needle| body.contains(needle)));
            return Err(if terminal {
                RefreshFailure::Terminal
            } else {
                RefreshFailure::Temporary
            });
        }
        let response: RefreshResponse = response.json().map_err(|_| RefreshFailure::Temporary)?;
        let mut refreshed = tokens.clone();
        if let Some(value) = response.id_token {
            refreshed.id_token = Some(value);
        }
        if let Some(value) = response.access_token {
            refreshed.access_token = value;
        }
        if let Some(value) = response.refresh_token {
            refreshed.refresh_token = value;
        }
        let account = account_from_tokens(&refreshed).map_err(|_| RefreshFailure::Temporary)?;
        refreshed.account_id = account.account_id;
        refreshed.email = account.email;
        refreshed.plan_type = account.plan_type;
        refreshed.last_refresh = Utc::now().to_rfc3339();
        Ok(refreshed)
    }

    fn set_cached_expiry(&self, provider_id: &str, expiry: Option<u64>) {
        if let Ok(mut runtimes) = self.runtimes.lock() {
            runtimes
                .entry(provider_id.to_owned())
                .or_default()
                .cached_expiry = expiry;
        }
    }

    fn clear_runtime_session(&self, provider_id: &str) -> Result<(), String> {
        let mut runtimes = lock(&self.runtimes)?;
        if let Some(runtime) = runtimes.get_mut(provider_id) {
            runtime.cached_expiry = None;
            runtime.force_refresh = false;
        }
        Ok(())
    }

    fn token_path(&self, provider_id: &str) -> PathBuf {
        let digest = Sha256::digest(provider_id.as_bytes());
        let mut name = String::with_capacity(64 + ".v1.bin".len());
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut name, "{byte:02x}");
        }
        self.store_root.join(format!("{name}.v1.bin"))
    }
}

struct SignInGuard<'a> {
    host: &'a CodexOauthHost,
    provider_id: String,
}

impl Drop for SignInGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut runtimes) = self.host.runtimes.lock() {
            if let Some(runtime) = runtimes.get_mut(&self.provider_id) {
                runtime.active_sign_in = None;
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct StoredTokens {
    version: u8,
    id_token: Option<String>,
    access_token: String,
    refresh_token: String,
    account_id: String,
    email: Option<String>,
    plan_type: Option<String>,
    last_refresh: String,
}

#[derive(Deserialize)]
struct TokenExchangeResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
}

#[derive(Deserialize)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

enum RefreshFailure {
    Terminal,
    Temporary,
}

enum CallbackAction {
    Continue,
    Fail(String),
    Code(String, TcpStream),
}

/// JWT claims are not signature-verified: TLS-authenticated issuer responses are used only for display and request headers.
fn jwt_claims(token: &str) -> Result<serde_json::Value, String> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| "Codex 令牌格式无效".to_owned())?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| "Codex 令牌格式无效".to_owned())?;
    serde_json::from_slice(&decoded).map_err(|_| "Codex 令牌内容无效".to_owned())
}

fn account_from_tokens(tokens: &StoredTokens) -> Result<CodexAccount, String> {
    let access = jwt_claims(&tokens.access_token).ok();
    let identity = tokens
        .id_token
        .as_deref()
        .and_then(|token| jwt_claims(token).ok());
    let auth_claims = access
        .as_ref()
        .and_then(|claims| claims.get("https://api.openai.com/auth"))
        .or_else(|| {
            identity
                .as_ref()
                .and_then(|claims| claims.get("https://api.openai.com/auth"))
        });
    let account_id = auth_claims
        .and_then(|claims| claims.get("chatgpt_account_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Codex 令牌里没有 ChatGPT 账号信息".to_owned())?
        .to_owned();
    let plan_type = auth_claims
        .and_then(|claims| claims.get("chatgpt_plan_type"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let email = identity
        .as_ref()
        .and_then(|claims| claims.get("email"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Ok(CodexAccount {
        account_id,
        email,
        plan_type,
    })
}

fn token_expiry(access_token: &str) -> Option<u64> {
    jwt_claims(access_token).ok()?.get("exp")?.as_u64()
}

fn credentials_for(tokens: &StoredTokens) -> CodexCredentials {
    CodexCredentials {
        access_token: Zeroizing::new(tokens.access_token.clone()),
        account_id: tokens.account_id.clone(),
    }
}

fn pkce_verifier() -> Zeroizing<String> {
    Zeroizing::new(random_urlsafe(64))
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_urlsafe(bytes: usize) -> String {
    let mut random = Zeroizing::new(Vec::with_capacity(bytes));
    while random.len() < bytes {
        random.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    random.truncate(bytes);
    URL_SAFE_NO_PAD.encode(random.as_slice())
}

fn generate_key() -> Zeroizing<[u8; 32]> {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(first.as_bytes());
    key[16..].copy_from_slice(second.as_bytes());
    Zeroizing::new(key)
}

fn now_seconds() -> u64 {
    Utc::now().timestamp().max(0) as u64
}

fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, String> {
    mutex.lock().map_err(|_| "Codex 登录状态已损坏".to_owned())
}

fn poll_callback(
    listener: &TcpListener,
    expected_state: &str,
) -> Result<Option<CallbackAction>, String> {
    let (mut stream, _) = match listener.accept() {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
        Err(_) => return Ok(None),
    };
    // A connection that never sends a request line is not the callback: browsers
    // pre-connect to the redirect host and hold the socket idle, and anything on
    // this machine can connect. Such a stream is dropped and the wait goes on;
    // only a well-formed callback may end the sign-in either way.
    if stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .is_err()
    {
        return Ok(Some(CallbackAction::Continue));
    }
    let request = match read_http_request(&mut stream) {
        Ok(request) => request,
        Err(_) => return Ok(Some(CallbackAction::Continue)),
    };
    let Some((method, target)) = request_line(&request) else {
        let _ = write_response(
            &mut stream,
            400,
            "Bad Request",
            &callback_page(SIGN_IN_FAILED_PAGE_TITLE, SIGN_IN_FAILED_PAGE_DETAIL),
            "text/html; charset=utf-8",
        );
        return Ok(Some(CallbackAction::Continue));
    };
    if method != "GET" {
        let _ = write_response(
            &mut stream,
            404,
            "Not Found",
            "Not found",
            "text/plain; charset=utf-8",
        );
        return Ok(Some(CallbackAction::Continue));
    }
    let url = match Url::parse(&format!("http://localhost{target}")) {
        Ok(url) => url,
        Err(_) => {
            let _ = write_response(
                &mut stream,
                400,
                "Bad Request",
                &callback_page(SIGN_IN_FAILED_PAGE_TITLE, SIGN_IN_FAILED_PAGE_DETAIL),
                "text/html; charset=utf-8",
            );
            return Ok(Some(CallbackAction::Continue));
        }
    };
    if url.path() != "/auth/callback" {
        let _ = write_response(
            &mut stream,
            404,
            "Not Found",
            "Not found",
            "text/plain; charset=utf-8",
        );
        return Ok(Some(CallbackAction::Continue));
    }
    let pairs: HashMap<String, String> = url.query_pairs().into_owned().collect();
    if pairs
        .get("state")
        .map(|state| state != expected_state)
        .unwrap_or(true)
    {
        let _ = write_response(
            &mut stream,
            400,
            "Bad Request",
            &callback_page(
                SIGN_IN_FAILED_PAGE_TITLE,
                "state 不匹配，这不是 Mework 发起的登录。/ The state does not match a sign-in Mework started.",
            ),
            "text/html; charset=utf-8",
        );
        return Ok(Some(CallbackAction::Continue));
    }
    if let Some(error) = pairs.get("error") {
        let body = callback_page(
            SIGN_IN_FAILED_PAGE_TITLE,
            &format!("OpenAI 返回：{} / OpenAI answered: {0}", escape_html(error)),
        );
        let _ = write_response(
            &mut stream,
            400,
            "Bad Request",
            &body,
            "text/html; charset=utf-8",
        );
        return Ok(Some(CallbackAction::Fail(format!(
            "OAuth 提供方返回错误：{error}"
        ))));
    }
    let Some(code) = pairs.get("code").filter(|code| !code.is_empty()) else {
        let _ = write_response(
            &mut stream,
            400,
            "Bad Request",
            &callback_page(
                SIGN_IN_FAILED_PAGE_TITLE,
                "回调里没有授权码。/ The callback carried no authorization code.",
            ),
            "text/html; charset=utf-8",
        );
        return Ok(Some(CallbackAction::Fail(
            "OAuth 提供方返回错误：缺少授权码".into(),
        )));
    };
    Ok(Some(CallbackAction::Code(code.clone(), stream)))
}

fn read_http_request(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut request = Vec::with_capacity(1024);
    let mut buffer = [0u8; 1024];
    while request.len() < 16 * 1024 {
        let read = stream
            .read(&mut buffer)
            .map_err(|_| "无法读取 Codex 登录回调".to_owned())?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    if request.len() >= 16 * 1024 {
        return Err("Codex 登录回调请求过大".into());
    }
    Ok(request)
}

fn request_line(request: &[u8]) -> Option<(&str, &str)> {
    let line_end = request.windows(2).position(|window| window == b"\r\n")?;
    let line = std::str::from_utf8(&request[..line_end]).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    let version = parts.next()?;
    if !version.starts_with("HTTP/1.") || parts.next().is_some() {
        return None;
    }
    Some((method, target))
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    body: &str,
    content_type: &str,
) -> std::io::Result<()> {
    let body = body.as_bytes();
    stream.write_all(
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    )?;
    stream.write_all(body)
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The page the browser lands on after the callback. It is the only thing the
/// user sees of this listener, so it says what happened and what to do next in
/// both languages; `detail` is already HTML-escaped by the caller when it
/// carries upstream text.
fn callback_page(title: &str, detail: &str) -> String {
    format!(
        concat!(
            "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\">",
            "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">",
            "<title>Mework</title>",
            "<style>body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;",
            "font-family:system-ui,-apple-system,Segoe UI,sans-serif;background:#f6f6f4;color:#1f1f1f}}",
            "main{{max-width:28rem;padding:2rem;text-align:center}}h1{{font-size:1.25rem;margin:0 0 .75rem}}",
            "p{{margin:0;line-height:1.6;color:#555}}</style></head>",
            "<body><main><h1>{title}</h1><p>{detail}</p></main></body></html>"
        ),
        title = title,
        detail = detail
    )
}

const SIGN_IN_FAILED_PAGE_TITLE: &str = "登录失败 / Sign-in failed";
const SIGN_IN_FAILED_PAGE_DETAIL: &str =
    "请回到 Mework 重试。/ Return to Mework and try again.";

fn ensure_store_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|_| "无法创建 Codex 登录令牌目录".to_owned())?;
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "无法检查 Codex 登录令牌目录".to_owned())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || path_is_reparse_point(path)? {
        return Err("Codex 登录令牌目录不安全".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "无法保护 Codex 登录令牌目录".to_owned())?;
    }
    Ok(())
}

fn atomic_write_private(path: &Path, payload: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Codex 登录令牌路径无效".to_owned())?;
    ensure_store_dir(parent)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || path_is_reparse_point(path)?
            {
                return Err("Codex 登录令牌文件不安全".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("无法检查 Codex 登录令牌文件".into()),
    }
    let temporary = parent.join(format!(".codex-oauth.{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|_| "无法创建 Codex 登录令牌临时文件".to_owned())?;
        if fs::symlink_metadata(&temporary)
            .map_err(|_| "无法检查 Codex 登录令牌临时文件".to_owned())?
            .file_type()
            .is_symlink()
            || path_is_reparse_point(&temporary)?
        {
            return Err("Codex 登录令牌临时文件不安全".into());
        }
        file.write_all(payload)
            .map_err(|_| "无法写入 Codex 登录令牌临时文件".to_owned())?;
        file.sync_all()
            .map_err(|_| "无法刷新 Codex 登录令牌临时文件".to_owned())?;
        make_file_private(&temporary)?;
        drop(file);
        fs::rename(&temporary, path).map_err(|_| "无法替换 Codex 登录令牌文件".to_owned())?;
        make_file_private(path)?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result
}

#[cfg(windows)]
fn path_is_reparse_point(path: &Path) -> Result<bool, String> {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .map_err(|_| "无法检查 Codex 登录令牌重解析点".to_owned())
}

#[cfg(not(windows))]
fn path_is_reparse_point(_path: &Path) -> Result<bool, String> {
    Ok(false)
}

#[cfg(unix)]
fn make_file_private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| "无法保护 Codex 登录令牌文件".to_owned())
}

#[cfg(not(unix))]
fn make_file_private(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Process-wide Codex OAuth state. The host is intentionally independent from the app state so provider calls share refresh serialization.
pub fn host() -> &'static CodexOauthHost {
    static HOST: OnceLock<CodexOauthHost> = OnceLock::new();
    HOST.get_or_init(|| {
        #[cfg(test)]
        let root = {
            static TEST_STORE: OnceLock<tempfile::TempDir> = OnceLock::new();
            TEST_STORE
                .get_or_init(|| tempfile::tempdir().expect("无法创建 Codex OAuth 测试目录"))
                .path()
                .join("codex-oauth")
        };
        #[cfg(not(test))]
        let root = dirs::home_dir()
            .expect("无法确定用户主目录")
            .join(".mework")
            .join("codex-oauth");
        CodexOauthHost::new(root, host_config())
    })
}

/// Production issuer, except under the `browser-dev` feature, where
/// `MEWORK_CODEX_OAUTH_ISSUER` may point the whole flow (authorize URL, code
/// exchange, refresh) at a loopback stand-in so the sign-in can be exercised
/// end to end without OpenAI. The release binary is built without the feature,
/// so no environment variable can redirect a real user's tokens.
fn host_config() -> CodexOauthConfig {
    let config = CodexOauthConfig::default();
    #[cfg(feature = "browser-dev")]
    {
        if let Ok(issuer) = std::env::var("MEWORK_CODEX_OAUTH_ISSUER") {
            let issuer = issuer.trim().trim_end_matches('/').to_owned();
            if let Ok(url) = Url::parse(&issuer) {
                if crate::http_util::is_local_network_url(&url) {
                    return CodexOauthConfig { issuer, ..config };
                }
            }
        }
    }
    config
}

pub fn request_headers(credentials: &CodexCredentials) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "chatgpt-account-id".to_owned(),
            credentials.account_id.clone(),
        ),
        ("originator".to_owned(), "mework".to_owned()),
        (
            "user-agent".to_owned(),
            format!(
                "mework/{} ({}; {})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        ),
    ])
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn install_session(
        host: &CodexOauthHost,
        provider_id: &str,
        access_token: &str,
        refresh_token: &str,
        account_id: &str,
    ) {
        let tokens = StoredTokens {
            version: 1,
            id_token: None,
            access_token: access_token.to_owned(),
            refresh_token: refresh_token.to_owned(),
            account_id: account_id.to_owned(),
            email: None,
            plan_type: None,
            last_refresh: Utc::now().to_rfc3339(),
        };
        host.persist_tokens(provider_id, &tokens)
            .expect("无法安装 Codex OAuth 测试会话");
        host.set_cached_expiry(provider_id, token_expiry(access_token));
    }

    pub(crate) fn fake_jwt(claims: serde_json::Value) -> String {
        format!(
            "{}.{}.sig",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("无法编码测试 JWT"))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_rfc_7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorize_url_uses_bound_callback_parameters() {
        let host = CodexOauthHost::new(std::env::temp_dir(), CodexOauthConfig::default());
        let url = host
            .authorize_url("http://localhost:43123/auth/callback", "challenge", "state")
            .unwrap();
        let url = Url::parse(&url).unwrap();
        let pairs: HashMap<String, String> = url.query_pairs().into_owned().collect();
        for key in [
            "response_type",
            "client_id",
            "redirect_uri",
            "scope",
            "code_challenge",
            "code_challenge_method",
            "id_token_add_organizations",
            "codex_cli_simplified_flow",
            "state",
            "originator",
        ] {
            assert!(pairs.contains_key(key), "missing {key}");
        }
        assert_eq!(
            pairs["redirect_uri"],
            "http://localhost:43123/auth/callback"
        );
    }

    #[test]
    fn request_headers_are_lower_case_and_identified() {
        let headers = request_headers(&CodexCredentials {
            access_token: Zeroizing::new("secret".to_owned()),
            account_id: "acct_1".to_owned(),
        });
        assert_eq!(headers["chatgpt-account-id"], "acct_1");
        assert_eq!(headers["originator"], "mework");
        assert!(headers["user-agent"].starts_with("mework/"));
        assert!(headers.keys().all(|key| key == &key.to_ascii_lowercase()));
    }

    #[test]
    fn corrupt_ciphertext_is_rejected() {
        let temporary = tempfile::TempDir::new().unwrap();
        let host = CodexOauthHost::new(temporary.path().join("oauth"), CodexOauthConfig::default());
        let provider = format!("provider_codex_test_{}", Uuid::new_v4());
        let tokens = StoredTokens {
            version: 1,
            id_token: None,
            access_token: fake_jwt(
                serde_json::json!({"https://api.openai.com/auth":{"chatgpt_account_id":"acct"},"exp":now_seconds()+10000}),
            ),
            refresh_token: "refresh".into(),
            account_id: "acct".into(),
            email: None,
            plan_type: None,
            last_refresh: Utc::now().to_rfc3339(),
        };
        host.persist_tokens(&provider, &tokens).unwrap();
        let path = host.token_path(&provider);
        let mut bytes = fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        fs::write(path, bytes).unwrap();
        assert!(!host.status(&provider).unwrap().signed_in);
        assert!(host
            .credentials(&provider)
            .err()
            .expect("a corrupt session must not yield credentials")
            .contains("登录"));
        // Reads do not clean up: the key stays until a sign-out or the next
        // sign-in replaces it, so a poll racing a sign-in cannot destroy it.
        assert!(crate::api::api_key_status(&provider).unwrap().configured);
        host.sign_out(&provider).unwrap();
        assert!(!crate::api::api_key_status(&provider).unwrap().configured);
        assert!(!host.token_path(&provider).exists());
    }

    #[test]
    fn complete_sign_in_exchanges_pkce_and_persists_session() {
        let access = fake_jwt(serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_1",
                "chatgpt_plan_type": "pro"
            },
            "exp": now_seconds() + 10 * 24 * 60 * 60
        }));
        let identity = fake_jwt(serde_json::json!({"email": "u@example.com"}));
        let issuer = FakeIssuer::start(vec![success_json(serde_json::json!({
            "id_token": identity,
            "access_token": access,
            "refresh_token": "refresh_1"
        }))]);
        let temporary = tempfile::TempDir::new().unwrap();
        let mut config = CodexOauthConfig::default();
        config.issuer = issuer.url.clone();
        config.callback_ports = vec![0];
        let host = CodexOauthHost::new(temporary.path().join("oauth"), config);
        let provider = format!("provider_codex_test_{}", Uuid::new_v4());
        let challenge = Arc::new(Mutex::new(None));
        let (page_tx, page_rx) = std::sync::mpsc::channel();
        let browser_worker = Arc::new(Mutex::new(None));
        let challenge_for_browser = Arc::clone(&challenge);
        let worker_for_browser = Arc::clone(&browser_worker);
        let status = host
            .sign_in(&provider, &move |authorization| {
                let url = Url::parse(authorization).unwrap();
                let pairs: HashMap<String, String> = url.query_pairs().into_owned().collect();
                *challenge_for_browser.lock().unwrap() = pairs.get("code_challenge").cloned();
                let state = pairs["state"].clone();
                let callback = Url::parse(&pairs["redirect_uri"]).unwrap();
                let page_tx = page_tx.clone();
                *worker_for_browser.lock().unwrap() = Some(thread::spawn(move || {
                    let mut stream = TcpStream::connect(("127.0.0.1", callback.port().unwrap())).unwrap();
                    initialize_fake_stream(&stream);
                    write!(
                        stream,
                        "GET {}?code=abc&state={} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                        callback.path(),
                        state
                    )
                    .unwrap();
                    let mut response = String::new();
                    stream.read_to_string(&mut response).unwrap();
                    page_tx.send(response).unwrap();
                }));
                Ok(())
            })
            .unwrap();
        assert!(status.signed_in);
        let account = status.account.unwrap();
        assert_eq!(account.account_id, "acct_1");
        assert_eq!(account.email.as_deref(), Some("u@example.com"));
        assert_eq!(account.plan_type.as_deref(), Some("pro"));
        assert!(host.token_path(&provider).is_file());
        assert!(crate::api::api_key_status(&provider).unwrap().configured);
        let page = page_rx.recv_timeout(Duration::from_secs(15));
        browser_worker.lock().unwrap().take().unwrap().join().unwrap();
        assert!(page.expect("OAuth callback page completion").starts_with("HTTP/1.1 200"));
        let request = issuer.requests.lock().unwrap().pop().unwrap();
        assert_eq!(request.path, "/oauth/token");
        assert!(request
            .content_type
            .contains("application/x-www-form-urlencoded"));
        let form: HashMap<String, String> =
            Url::parse(&format!("http://localhost/?{}", request.body))
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code"], "abc");
        assert_eq!(
            pkce_challenge(&form["code_verifier"]),
            challenge.lock().unwrap().as_deref().unwrap()
        );
    }

    #[test]
    fn state_mismatch_keeps_waiting_and_provider_error_fails() {
        let access = fake_jwt(serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_2"},
            "exp": now_seconds() + 10000
        }));
        let issuer = FakeIssuer::start(vec![success_json(serde_json::json!({
            "id_token": fake_jwt(serde_json::json!({})),
            "access_token": access,
            "refresh_token": "refresh"
        }))]);
        let temporary = tempfile::TempDir::new().unwrap();
        let mut config = CodexOauthConfig::default();
        config.issuer = issuer.url.clone();
        config.callback_ports = vec![0];
        let host = CodexOauthHost::new(temporary.path().join("oauth"), config.clone());
        let provider = format!("provider_codex_test_{}", Uuid::new_v4());
        let first_status = Arc::new(Mutex::new(String::new()));
        let first_status_for_browser = Arc::clone(&first_status);
        assert!(host
            .sign_in(&provider, &move |authorization| {
                let pairs: HashMap<String, String> = Url::parse(authorization)
                    .unwrap()
                    .query_pairs()
                    .into_owned()
                    .collect();
                let callback = Url::parse(&pairs["redirect_uri"]).unwrap();
                let state = pairs["state"].clone();
                let first_status = Arc::clone(&first_status_for_browser);
                thread::spawn(move || {
                    let send = |state: &str| {
                        let mut stream = TcpStream::connect(("127.0.0.1", callback.port().unwrap())).unwrap();
                        write!(stream, "GET {}?code=abc&state={state} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n", callback.path()).unwrap();
                        let mut response = String::new();
                        stream.read_to_string(&mut response).unwrap();
                        response
                    };
                    *first_status.lock().unwrap() = send("wrong");
                    assert!(send(&state).starts_with("HTTP/1.1 200"));
                });
                Ok(())
            })
            .unwrap()
            .signed_in);
        assert!(first_status.lock().unwrap().starts_with("HTTP/1.1 400"));

        let error_host = CodexOauthHost::new(temporary.path().join("error"), config);
        let error_provider = format!("provider_codex_test_{}", Uuid::new_v4());
        let error = error_host
            .sign_in(&error_provider, &move |authorization| {
                let pairs: HashMap<String, String> = Url::parse(authorization)
                    .unwrap()
                    .query_pairs()
                    .into_owned()
                    .collect();
                let callback = Url::parse(&pairs["redirect_uri"]).unwrap();
                let state = pairs["state"].clone();
                thread::spawn(move || {
                    let mut stream = TcpStream::connect(("127.0.0.1", callback.port().unwrap())).unwrap();
                    write!(stream, "GET {}?error=access_denied&state={state} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n", callback.path()).unwrap();
                    let mut response = String::new();
                    stream.read_to_string(&mut response).unwrap();
                    assert!(response.starts_with("HTTP/1.1 400"));
                });
                Ok(())
            })
            .unwrap_err();
        assert!(error.contains("access_denied"));
        assert!(!error_host.status(&error_provider).unwrap().signed_in);
    }

    #[test]
    fn cancellation_and_duplicate_sign_in_are_observable() {
        let temporary = tempfile::TempDir::new().unwrap();
        let mut config = CodexOauthConfig::default();
        config.callback_ports = vec![0];
        config.sign_in_timeout = Duration::from_secs(5);
        let host = Arc::new(CodexOauthHost::new(temporary.path().join("oauth"), config));
        let provider = format!("provider_codex_test_{}", Uuid::new_v4());
        let background_host = Arc::clone(&host);
        let background_provider = provider.clone();
        let worker =
            thread::spawn(move || background_host.sign_in(&background_provider, &|_| Ok(())));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !host.status(&provider).unwrap().signing_in && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            host.sign_in(&provider, &|_| Ok(())).unwrap_err(),
            "Codex 登录正在进行中"
        );
        host.cancel_sign_in(&provider).unwrap();
        assert_eq!(worker.join().unwrap().unwrap_err(), "Codex 登录已取消");
        assert!(!host.status(&provider).unwrap().signing_in);
    }

    /// A sign-out issued while a sign-in is still waiting must end that wait:
    /// otherwise a callback arriving a moment later would re-create the session
    /// the user just removed.
    #[test]
    fn sign_out_cancels_a_sign_in_that_is_still_waiting() {
        let temporary = tempfile::TempDir::new().unwrap();
        let mut config = CodexOauthConfig::default();
        config.callback_ports = vec![0];
        config.sign_in_timeout = Duration::from_secs(5);
        let host = Arc::new(CodexOauthHost::new(temporary.path().join("oauth"), config));
        let provider = format!("provider_codex_test_{}", Uuid::new_v4());
        let background_host = Arc::clone(&host);
        let background_provider = provider.clone();
        let worker =
            thread::spawn(move || background_host.sign_in(&background_provider, &|_| Ok(())));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !host.status(&provider).unwrap().signing_in && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let status = host.sign_out(&provider).unwrap();
        assert!(!status.signed_in);
        assert_eq!(worker.join().unwrap().unwrap_err(), "Codex 登录已取消");
        assert!(!host.status(&provider).unwrap().signing_in);
        assert!(!crate::api::api_key_status(&provider).unwrap().configured);
    }

    /// An issuer that answers a refresh with a token already inside the expiry
    /// skew must not be asked again on the very next request: the floor after a
    /// successful refresh wins over `exp`.
    #[test]
    fn a_just_refreshed_token_is_not_refreshed_again_immediately() {
        let short_lived = fake_jwt(serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_floor"},
            "exp": now_seconds() + 120
        }));
        let issuer = FakeIssuer::start(vec![success_json(serde_json::json!({
            "access_token": short_lived,
            "refresh_token": "rotated-once"
        }))]);
        let temporary = tempfile::TempDir::new().unwrap();
        let mut config = CodexOauthConfig::default();
        config.issuer = issuer.url.clone();
        let host = CodexOauthHost::new(temporary.path().join("oauth"), config);
        let provider = format!("provider_codex_test_{}", Uuid::new_v4());
        host.persist_tokens(&provider, &stored_tokens("acct_floor", now_seconds() + 60))
            .unwrap();

        let first = host.credentials(&provider).unwrap();
        assert_eq!(first.access_token.as_str(), short_lived.as_str());
        // The fake issuer holds exactly one response; a second refresh would
        // fail, so a successful second call proves no refresh was attempted.
        let second = host.credentials(&provider).unwrap();
        assert_eq!(second.access_token.as_str(), short_lived.as_str());
        assert_eq!(issuer.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn credentials_refreshes_once_and_classifies_errors() {
        let refreshed_access = fake_jwt(serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_3"},
            "exp": now_seconds() + 10000
        }));
        let issuer = FakeIssuer::start(vec![success_json(serde_json::json!({
            "access_token": refreshed_access,
            "refresh_token": "rotated"
        }))]);
        let temporary = tempfile::TempDir::new().unwrap();
        let mut config = CodexOauthConfig::default();
        config.issuer = issuer.url.clone();
        let host = CodexOauthHost::new(temporary.path().join("oauth"), config);
        let provider = format!("provider_codex_test_{}", Uuid::new_v4());
        host.persist_tokens(&provider, &stored_tokens("acct_3", now_seconds() + 60))
            .unwrap();
        assert!(host
            .credentials(&provider)
            .unwrap()
            .access_token
            .contains("."));
        assert_eq!(issuer.requests.lock().unwrap().len(), 1);
        assert_eq!(host.credentials(&provider).unwrap().account_id, "acct_3");
        assert_eq!(issuer.requests.lock().unwrap().len(), 1);
        let request = issuer.requests.lock().unwrap().first().unwrap().clone();
        assert!(request.content_type.contains("application/json"));
        assert!(request.body.contains("refresh_token"));

        for (status, body, error, expected_signed_in) in [
            (
                400,
                r#"{"error":"invalid_grant"}"#,
                "Codex 登录已失效，请重新登录",
                false,
            ),
            (503, "{}", "刷新 Codex 登录令牌暂时失败，请稍后重试", true),
        ] {
            let issuer = FakeIssuer::start(vec![(status, body.to_owned())]);
            let temporary = tempfile::TempDir::new().unwrap();
            let mut config = CodexOauthConfig::default();
            config.issuer = issuer.url.clone();
            let host = CodexOauthHost::new(temporary.path().join("oauth"), config);
            let provider = format!("provider_codex_test_{}", Uuid::new_v4());
            host.persist_tokens(&provider, &stored_tokens("acct_4", now_seconds() + 60))
                .unwrap();
            assert_eq!(
                host.credentials(&provider)
                    .err()
                    .expect("refresh failures must not yield credentials"),
                error
            );
            assert_eq!(
                host.status(&provider).unwrap().signed_in,
                expected_signed_in
            );
        }
    }

    #[test]
    fn sign_out_and_remove_missing_file_are_idempotent() {
        let temporary = tempfile::TempDir::new().unwrap();
        let host = CodexOauthHost::new(temporary.path().join("oauth"), CodexOauthConfig::default());
        let provider = format!("provider_codex_test_{}", Uuid::new_v4());
        host.persist_tokens(&provider, &stored_tokens("acct_5", now_seconds() + 10000))
            .unwrap();
        let path = host.token_path(&provider);
        assert!(host.sign_out(&provider).unwrap().signed_in == false);
        assert!(!path.exists());
        assert!(!crate::api::api_key_status(&provider).unwrap().configured);
        assert!(host.remove(&provider).is_ok());
    }

    #[derive(Clone)]
    struct CapturedRequest {
        path: String,
        content_type: String,
        body: String,
    }

    struct FakeIssuer {
        url: String,
        requests: Arc<Mutex<Vec<CapturedRequest>>>,
        shutdown: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl FakeIssuer {
        fn start(responses: Vec<(u16, String)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let requests_for_worker = Arc::clone(&requests);
            let shutdown = Arc::new(AtomicBool::new(false));
            let shutdown_for_worker = Arc::clone(&shutdown);
            let worker = thread::spawn(move || {
                let mut responses = responses.into_iter();
                while !shutdown_for_worker.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            initialize_fake_stream(&stream);
                            let request = read_fake_request(&mut stream);
                            requests_for_worker.lock().unwrap().push(request);
                            let (status, body) = responses.next().unwrap_or((500, "{}".to_owned()));
                            let reason = if status == 200 { "OK" } else { "Error" };
                            write!(stream, "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2))
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                url: format!("http://{address}"),
                requests,
                shutdown,
                worker: Some(worker),
            }
        }
    }

    impl Drop for FakeIssuer {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn initialize_fake_stream(stream: &TcpStream) {
        stream.set_nonblocking(false)
            .expect("fake issuer: restore blocking accepted stream");
        stream.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("fake issuer: configure request read timeout");
        stream.set_write_timeout(Some(Duration::from_secs(10)))
            .expect("fake issuer: configure response write timeout");
    }

    #[test]
    fn fake_issuer_reads_fragmented_request_on_inherited_nonblocking_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        // Reproduce Windows accept inheritance on every platform, without a scheduling race.
        server.set_nonblocking(true).unwrap();
        initialize_fake_stream(&server);
        let (reading_tx, reading_rx) = std::sync::mpsc::channel();
        let (header_tx, header_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            reading_tx.send(()).unwrap();
            let mut first = [0];
            server.read_exact(&mut first).expect("fake issuer: waiting for first request byte");
            assert_eq!(first, [b'P']);
            header_tx.send(()).unwrap();
            let request = read_fake_request(&mut server);
            write!(server, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}")
                .expect("fake issuer: response write");
            request
        });
        reading_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        // A nonblocking read completes with WouldBlock while no request data is available.
        assert!(matches!(header_rx.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)));
        client.write_all(b"POST /oauth/token HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 8\r\n\r\n").unwrap();
        header_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        client.write_all(b"code=abc").unwrap();
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        let request = worker.join().unwrap();
        assert_eq!(request.path, "/oauth/token");
        assert_eq!(request.content_type, "application/x-www-form-urlencoded");
        assert_eq!(request.body, "code=abc");
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.ends_with("{}"));
    }

    fn read_fake_request(stream: &mut TcpStream) -> CapturedRequest {
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 1024];
        let mut expected = None;
        loop {
            let count = stream.read(&mut buffer).unwrap_or_else(|error| {
                panic!("fake issuer: request read after {} bytes (10s timeout): {error}", bytes.len())
            });
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if expected.is_none() {
                if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&bytes[..header_end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':').and_then(|(name, value)| {
                                if name.eq_ignore_ascii_case("content-length") {
                                    value.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                        })
                        .unwrap_or(0);
                    expected = Some(header_end + 4 + length);
                }
            }
            if expected.is_some_and(|expected| bytes.len() >= expected) {
                break;
            }
        }
        let header_end = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let first = headers.lines().next().unwrap();
        let path = first.split_whitespace().nth(1).unwrap().to_owned();
        let content_type = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-type")
                        .then(|| value.trim())
                })
            })
            .unwrap_or_default()
            .to_owned();
        CapturedRequest {
            path,
            content_type,
            body: String::from_utf8_lossy(&bytes[header_end + 4..]).to_string(),
        }
    }

    fn success_json(value: serde_json::Value) -> (u16, String) {
        (200, serde_json::to_string(&value).unwrap())
    }

    fn stored_tokens(account: &str, expiry: u64) -> StoredTokens {
        StoredTokens {
            version: 1,
            id_token: Some(fake_jwt(serde_json::json!({}))),
            access_token: fake_jwt(serde_json::json!({
                "https://api.openai.com/auth": {"chatgpt_account_id": account},
                "exp": expiry
            })),
            refresh_token: "refresh".into(),
            account_id: account.into(),
            email: None,
            plan_type: None,
            // A stored session is normally hours or days old; a just-minted
            // one is protected by the recent-refresh floor and never refreshes.
            last_refresh: (Utc::now() - chrono::Duration::days(1)).to_rfc3339(),
        }
    }

    fn fake_jwt(claims: serde_json::Value) -> String {
        format!(
            "{}.{}.sig",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
    }
}
