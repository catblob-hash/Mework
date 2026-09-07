use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State as AxumState,
    },
    http::{
        header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, ORIGIN},
        HeaderMap, HeaderValue, StatusCode,
    },
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{env, io::Write as _, net::SocketAddr, sync::mpsc as sync_mpsc, thread, time::Duration};
use tauri::{
    ipc::{Channel, InvokeResponseBody},
    AppHandle, Manager,
};
use tokio::sync::mpsc;

use super::{
    app_exit::AppExitCoordinator,
    browser_dev_lifecycle::{
        ControlledExit, ControlledExitHandshake, RELEASE_BARRIER_MARKER, RELEASE_FAILURE_EXIT_CODE,
    },
    browser_renderer_mount::BrowserRendererMountLease,
    finalize_app_shutdown,
    model::ModelStreamEvent,
    state::AppState,
    terminal::TerminalEvent,
};

const TOKEN_ENV: &str = "MEWORK_BROWSER_DEV_TOKEN";
const ORIGIN_ENV: &str = "MEWORK_BROWSER_DEV_ORIGIN";
const ADDRESS_ENV: &str = "MEWORK_BROWSER_DEV_ADDRESS";
const DATA_IDENTIFIER_ENV: &str = "MEWORK_BROWSER_DEV_DATA_IDENTIFIER";
const INSTANCE_ID_ENV: &str = "MEWORK_BROWSER_DEV_INSTANCE_ID";
const WEB_SEARCH_E2E_RUN_ID_ENV: &str = "MEWORK_WEB_SEARCH_E2E_RUN_ID";
const WEB_SEARCH_E2E_ENABLE_ENV: &str = "MEWORK_WEB_SEARCH_E2E";
const WEB_SEARCH_E2E_DATA_IDENTIFIER_PREFIX: &str = "com.mework.app.e2e.web-search-";
const IMAGE_INPUT_E2E_RUN_ID_ENV: &str = "MEWORK_IMAGE_INPUT_E2E_RUN_ID";
const ORIGINLESS_IMAGE_CLEANUP_ENV: &str = "MEWORK_BROWSER_DEV_ORIGINLESS_E2E_CLEANUP";
const DATA_IDENTIFIER_PREFIX: &str = "com.mework.app.e2e.";
const LEGACY_TOKEN_ENV: &str = "NAIWORD_BROWSER_DEV_TOKEN";
const LEGACY_ORIGIN_ENV: &str = "NAIWORD_BROWSER_DEV_ORIGIN";
const LEGACY_ADDRESS_ENV: &str = "NAIWORD_BROWSER_DEV_ADDRESS";
const LEGACY_DATA_IDENTIFIER_ENV: &str = "NAIWORD_BROWSER_DEV_DATA_IDENTIFIER";
const LEGACY_DATA_IDENTIFIER_PREFIX: &str = "com.naiword.agentstudio.e2e.";
const BROWSER_E2E_CONTROLLED_EXIT_DELAY: Duration = Duration::from_millis(150);
#[cfg(windows)]
const BROWSER_E2E_RELEASE_TIMEOUT: Duration = Duration::from_secs(45);
// Exactly one controlled exit is ever accepted per backend instance: the authenticated E2E command
// claims this latch, the release barrier reads it, and its commit half is written only after Tauri
// exits normally behind a passed barrier.
static BROWSER_E2E_EXIT: ControlledExitHandshake = ControlledExitHandshake::new();
const IMAGE_INPUT_BROWSER_E2E_HTML: &str =
    include_str!("../resources/image-input-browser-e2e.html");
const IMAGE_INPUT_BROWSER_E2E_CSP: &str = concat!(
    "default-src 'none'; script-src 'none'; script-src-attr 'none'; ",
    "connect-src 'none'; img-src 'none'; font-src 'none'; ",
    "style-src 'none'; style-src-attr 'none'; frame-src 'none'; ",
    "object-src 'none'; worker-src 'none'; child-src 'none'; ",
    "media-src 'none'; manifest-src 'none'; base-uri 'none'; ",
    "form-action 'none'; frame-ancestors 'none'",
);

/// A minimal valid 1x1 PNG. The browser-upload bridge has no transcript to pull
/// a real attachment from, so it materializes this fixture instead.
const BROWSER_E2E_UPLOAD_FIXTURE_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0,
    0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 218, 99, 249, 207, 192, 0, 0, 3, 17,
    1, 4, 251, 51, 50, 105, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

#[derive(Clone)]
struct BridgeState {
    app: AppHandle,
    token: String,
    origin: String,
    allow_originless_image_cleanup: bool,
    /// Native-only authority for this authenticated, loopback-only development
    /// bridge. Client-supplied renderer mount fields are never trusted.
    renderer_mount: BrowserRendererMountLease,
    memory_workspace_fixture: Option<super::browser_dev_fixture::BrowserDevMemoryWorkspaceFixture>,
}

#[derive(Deserialize)]
struct BridgeQuery {
    token: String,
}

#[derive(Debug, Deserialize)]
struct Invocation {
    id: u64,
    command: String,
    #[serde(default)]
    args: Value,
}

type Outgoing = mpsc::UnboundedSender<Message>;

pub(super) fn run() -> i32 {
    let exit_coordinator = AppExitCoordinator::default();
    let mut context = tauri::generate_context!();
    context.config_mut().identifier = browser_data_identifier()
        .unwrap_or_else(|error| panic!("浏览器开发数据隔离配置无效: {error}"));
    // The browser-development binary owns only the Rust application/runtime. The UI is served by
    // Vite and opened in a normal browser, so no main WebView window is created.
    context.config_mut().app.windows.clear();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::browser_dev())
        .setup(|app| {
            // No authenticated bridge or other app-data consumer may start
            // before this isolated data directory has its lifetime authority.
            super::reconcile_image_attachments_on_startup(app.handle())
                .map_err(std::io::Error::other)?;
            // The development backend executes real tools, so its cards need
            // the same durable signing key as the desktop app's. Without it
            // every card would be signed by the ephemeral placeholder and stop
            // verifying the moment this process restarts.
            let document_path = super::document_path(app.handle()).map_err(std::io::Error::other)?;
            if let Some(app_data) = document_path.parent() {
                app.state::<AppState>()
                    .install_attestation_key(app_data)
                    .map_err(std::io::Error::other)?;
            }
            super::install_background_write_failure_reporting(app.state::<AppState>().inner());
            app.state::<AppState>()
                .browser
                .attach_app(app.handle().clone())
                .map_err(std::io::Error::other)?;

            start_server(app.handle().clone()).map_err(std::io::Error::other)?;
            Ok(())
        })
        .build(context)
        .expect("error while building the Mework browser development backend");

    let runtime_exit_code = app.run_return(move |app_handle, event| match event {
        // The browser-dev process intentionally has no main window. Keep the authenticated
        // backend/Vite session alive until an explicit process exit is requested.
        tauri::RunEvent::ExitRequested {
            code: None, api, ..
        } => api.prevent_exit(),
        tauri::RunEvent::ExitRequested {
            code: Some(code),
            api,
            ..
        } => {
            if !exit_coordinator.is_ready() {
                api.prevent_exit();
                super::request_deferred_exit_with_barrier(
                    app_handle.clone(),
                    exit_coordinator.clone(),
                    code,
                    browser_e2e_release_barrier,
                );
            }
        }
        tauri::RunEvent::Exit => {
            finalize_app_shutdown(app_handle, &exit_coordinator);
            commit_browser_e2e_controlled_exit();
        }
        _ => {}
    });
    BROWSER_E2E_EXIT.process_exit_code(runtime_exit_code)
}

/// Releases every WebView2 browser process before a controlled E2E exit is allowed to proceed, and
/// returns the effective process exit code.
///
/// This runs on the deferred-exit worker before the application is asked to exit — the only window
/// in which Tauri's main message pump can still deliver the native `BrowserProcessExited`
/// notifications the barrier waits on. Ordinary browser-dev sessions never request a controlled
/// exit, so they never pay for this.
fn browser_e2e_release_barrier(app: &AppHandle, requested_exit_code: i32) -> i32 {
    if BROWSER_E2E_EXIT.accepted().is_none() {
        return requested_exit_code;
    }
    match release_browser_webview2_processes(app, BROWSER_E2E_EXIT.requires_browser_process()) {
        Ok(process_count) => {
            BROWSER_E2E_EXIT.pass_release_barrier();
            println!("{RELEASE_BARRIER_MARKER}");
            let _ = std::io::stdout().flush();
            eprintln!("WebView2 释放屏障已确认 {process_count} 个浏览器进程正常退出");
            requested_exit_code
        }
        Err(error) => {
            eprintln!("WebView2 释放屏障失败，已放弃受控退出握手：{error}");
            RELEASE_FAILURE_EXIT_CODE
        }
    }
}

#[cfg(windows)]
fn release_browser_webview2_processes(
    app: &AppHandle,
    require_browser_process: bool,
) -> Result<usize, String> {
    app.state::<AppState>()
        .browser
        .shutdown_all_with_webview2_release_barrier(
            BROWSER_E2E_RELEASE_TIMEOUT,
            require_browser_process,
        )
}

#[cfg(not(windows))]
fn release_browser_webview2_processes(
    _app: &AppHandle,
    _require_browser_process: bool,
) -> Result<usize, String> {
    Err("当前平台没有 WebView2 浏览器进程退出通知，无法建立释放屏障".to_owned())
}

/// Writes the commit half of the controlled-exit handshake once normal Tauri exit and final host
/// cleanup have both been reached. A request marker alone can never turn a later crash — or a
/// failed release barrier — into a controlled restart or shutdown for the wrapper.
fn commit_browser_e2e_controlled_exit() {
    let Some(marker) = BROWSER_E2E_EXIT.commit() else {
        return;
    };
    println!("{marker}");
    let _ = std::io::stdout().flush();
}

fn browser_data_identifier() -> Result<String, String> {
    let (variable, prefix, identifier) = match env::var(DATA_IDENTIFIER_ENV) {
        Ok(identifier) => (DATA_IDENTIFIER_ENV, DATA_IDENTIFIER_PREFIX, identifier),
        Err(env::VarError::NotPresent) => match env::var(LEGACY_DATA_IDENTIFIER_ENV) {
            Ok(identifier) => (
                LEGACY_DATA_IDENTIFIER_ENV,
                LEGACY_DATA_IDENTIFIER_PREFIX,
                identifier,
            ),
            Err(env::VarError::NotPresent) => {
                return Err(format!(
                    "缺少 {DATA_IDENTIFIER_ENV}；请通过 npm run dev:browser 启动隔离环境"
                ));
            }
            Err(env::VarError::NotUnicode(_)) => {
                return Err(format!("{LEGACY_DATA_IDENTIFIER_ENV} 必须是 Unicode 文本"));
            }
        },
        Err(env::VarError::NotUnicode(_)) => {
            return Err(format!("{DATA_IDENTIFIER_ENV} 必须是 Unicode 文本"));
        }
    };
    if identifier.len() > 128
        || !identifier.starts_with(prefix)
        || identifier.len() == prefix.len()
        || identifier
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
    {
        return Err(format!(
            "{variable} 必须以 {prefix} 开头，且只能包含 ASCII 字母、数字、点、横线或下划线"
        ));
    }
    Ok(identifier)
}

fn start_server(app: AppHandle) -> Result<(), String> {
    let token = env::var(TOKEN_ENV)
        .or_else(|_| env::var(LEGACY_TOKEN_ENV))
        .map_err(|_| format!("缺少 {TOKEN_ENV}，请通过 npm run dev:browser 启动"))?;
    if token.len() < 32 {
        return Err(format!("{TOKEN_ENV} 长度不足"));
    }
    let allow_originless_image_cleanup =
        env::var(ORIGINLESS_IMAGE_CLEANUP_ENV).as_deref() == Ok("1");
    if allow_originless_image_cleanup {
        let data_identifier = env::var(DATA_IDENTIFIER_ENV)
            .map_err(|_| format!("缺少或无法读取 {DATA_IDENTIFIER_ENV}"))?;
        let run_id = env::var(IMAGE_INPUT_E2E_RUN_ID_ENV)
            .map_err(|_| format!("缺少或无法读取 {IMAGE_INPUT_E2E_RUN_ID_ENV}"))?;
        if !data_identifier.starts_with("com.mework.app.e2e.image-input-")
            || data_identifier.len() <= "com.mework.app.e2e.image-input-".len()
            || !data_identifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            || !matches!(run_id.len(), 24)
            || !run_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            || token.len() != 64
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err("无 Origin 图片 E2E 清理桥的隔离身份无效".into());
        }
    }
    let origin = env::var(ORIGIN_ENV)
        .or_else(|_| env::var(LEGACY_ORIGIN_ENV))
        .unwrap_or_else(|_| "http://127.0.0.1:1420".into());
    validate_origin(&origin)?;
    // The trusted frontend is served from this origin, so the page WebView must
    // not be able to load it. Record it for the navigation policy exactly as the
    // desktop entry point does from the Tauri configuration; the port is whatever
    // the development server was able to take, not a fixed number.
    if let Ok(parsed) = url::Url::parse(&origin) {
        crate::browser::install_app_dev_server(&parsed);
    }
    let address = env::var(ADDRESS_ENV)
        .or_else(|_| env::var(LEGACY_ADDRESS_ENV))
        .unwrap_or_else(|_| "127.0.0.1:1430".into());
    let address = address
        .parse::<SocketAddr>()
        .map_err(|error| format!("无效的浏览器开发后端地址: {error}"))?;
    validate_web_search_e2e_environment(&token, &origin, address)?;
    let memory_workspace_fixture =
        super::browser_dev_fixture::browser_dev_memory_workspace_fixture()?;

    let renderer_mount = app
        .state::<AppState>()
        .browser_renderer_mounts
        .register_new_mount()
        .map_err(super::browser_renderer_mount_error)?;
    let state = BridgeState {
        app,
        token,
        origin,
        allow_originless_image_cleanup,
        renderer_mount,
        memory_workspace_fixture,
    };
    let (ready_tx, ready_rx) = sync_mpsc::sync_channel::<Result<(), String>>(1);
    thread::Builder::new()
        .name("mework-browser-dev-server".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(format!("无法创建开发后端运行时: {error}")));
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::bind(address).await {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = ready_tx
                            .send(Err(format!("无法监听浏览器开发后端 {address}: {error}")));
                        return;
                    }
                };
                let router = Router::new()
                    .route("/health", get(health))
                    .route("/image-input-browser-e2e", get(image_input_browser_e2e))
                    .route("/ws", get(upgrade_websocket))
                    .with_state(state);
                println!("[browser-dev] Rust backend ready at ws://{address}/ws");
                let _ = ready_tx.send(Ok(()));
                if let Err(error) = axum::serve(listener, router).await {
                    eprintln!("浏览器开发后端已停止: {error}");
                }
            });
        })
        .map_err(|error| format!("无法启动浏览器开发后端线程: {error}"))?;

    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|error| format!("等待浏览器开发后端启动失败: {error}"))?
}

fn validate_origin(origin: &str) -> Result<(), String> {
    let parsed = url::Url::parse(origin).map_err(|error| format!("无效的开发页面来源: {error}"))?;
    if parsed.scheme() != "http" {
        return Err("浏览器开发页面来源必须使用 http".into());
    }
    let loopback = parsed.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !loopback || parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some()
    {
        return Err("浏览器开发页面来源必须是无路径的本机回环地址".into());
    }
    Ok(())
}

fn validate_web_search_e2e_environment(
    token: &str,
    frontend_origin: &str,
    backend_address: SocketAddr,
) -> Result<(), String> {
    let enabled = match env::var(WEB_SEARCH_E2E_ENABLE_ENV) {
        Ok(value) if value == "1" => true,
        Ok(_) => {
            return Err(format!("{WEB_SEARCH_E2E_ENABLE_ENV} 只接受精确值 1"));
        }
        Err(env::VarError::NotPresent) => false,
        Err(env::VarError::NotUnicode(_)) => {
            return Err(format!("{WEB_SEARCH_E2E_ENABLE_ENV} 必须是 Unicode 文本"));
        }
    };
    if !enabled {
        if env::var_os(WEB_SEARCH_E2E_RUN_ID_ENV).is_some() {
            return Err("联网搜索 E2E 未启用时拒绝孤立的 run 环境配置".into());
        }
        return Ok(());
    }

    let run_id = env::var(WEB_SEARCH_E2E_RUN_ID_ENV)
        .map_err(|_| format!("缺少或无法读取 {WEB_SEARCH_E2E_RUN_ID_ENV}"))?;
    if run_id.len() != 24
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!(
            "{WEB_SEARCH_E2E_RUN_ID_ENV} 必须是 12 字节随机值的小写十六进制编码"
        ));
    }
    if token.len() != 64
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("联网搜索 E2E 只接受 wrapper 生成的 32 字节随机桥令牌".into());
    }
    let data_identifier = env::var(DATA_IDENTIFIER_ENV)
        .map_err(|_| format!("缺少或无法读取 {DATA_IDENTIFIER_ENV}"))?;
    let data_suffix = data_identifier
        .strip_prefix(WEB_SEARCH_E2E_DATA_IDENTIFIER_PREFIX)
        .ok_or("联网搜索 E2E 必须使用独占 web-search 应用数据标识")?;
    if data_suffix.len() != 24
        || !data_suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("联网搜索 E2E 应用数据标识必须包含 12 字节随机后缀".into());
    }

    let frontend =
        url::Url::parse(frontend_origin).map_err(|_| "联网搜索 E2E 前端来源无效".to_owned())?;
    if frontend.scheme() != "http"
        || frontend.host_str() != Some("127.0.0.1")
        || frontend.port().is_none()
        || frontend.path() != "/"
        || frontend.query().is_some()
        || frontend.fragment().is_some()
        || backend_address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        || backend_address.port() < 1024
    {
        return Err("联网搜索 E2E 前后端必须使用宿主指定的高位 IPv4 loopback 端口".into());
    }
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "runtime": "rust" }))
}

async fn image_input_browser_e2e() -> Response {
    let mut response = Html(IMAGE_INPUT_BROWSER_E2E_HTML).into_response();
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(IMAGE_INPUT_BROWSER_E2E_CSP),
    );
    response
}

async fn upgrade_websocket(
    ws: WebSocketUpgrade,
    AxumState(state): AxumState<BridgeState>,
    Query(query): Query<BridgeQuery>,
    headers: HeaderMap,
) -> Response {
    let origin = headers.get(ORIGIN).and_then(|value| value.to_str().ok());
    let cleanup_only = origin.is_none() && state.allow_originless_image_cleanup;
    if query.token != state.token || (origin != Some(state.origin.as_str()) && !cleanup_only) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(16 * 1024 * 1024)
        .on_upgrade(move |socket| serve_socket(socket, state, cleanup_only))
        .into_response()
}

async fn serve_socket(socket: WebSocket, state: BridgeState, cleanup_only: bool) {
    let (mut writer, mut reader) = socket.split();
    let (outgoing, mut output) = mpsc::unbounded_channel::<Message>();
    let writer_task = tokio::spawn(async move {
        while let Some(message) = output.recv().await {
            if writer.send(message).await.is_err() {
                break;
            }
        }
    });

    while let Some(message) = reader.next().await {
        let Ok(Message::Text(text)) = message else {
            continue;
        };
        let invocation = match serde_json::from_str::<Invocation>(text.as_str()) {
            Ok(invocation) => invocation,
            Err(error) => {
                let _ = send_json(
                    &outgoing,
                    json!({ "type": "protocolError", "error": format!("无效调用消息: {error}") }),
                );
                continue;
            }
        };
        // The no-Origin bridge exists so the Node runner can do exactly two things after the
        // page's authenticated session is gone: verify the fake credentials are wiped, and drive
        // the controlled-exit handshake (which itself re-validates the exact instance id and the
        // image-E2E policy). Everything else stays page-only.
        if cleanup_only
            && !matches!(
                invocation.command.as_str(),
                "browser_e2e_cleanup_image_input_keys"
                    | "browser_e2e_instance_id"
                    | "browser_e2e_shutdown_backend"
            )
        {
            let _ = send_json(
                &outgoing,
                json!({
                    "type": "result",
                    "id": invocation.id,
                    "ok": false,
                    "error": "无 Origin E2E 桥只允许图片测试凭据清理与受控停机握手"
                }),
            );
            continue;
        }
        let app = state.app.clone();
        let memory_workspace_fixture = state.memory_workspace_fixture.clone();
        let renderer_mount = state.renderer_mount.clone();
        let invocation_outgoing = outgoing.clone();
        tokio::spawn(async move {
            let id = invocation.id;
            let response = match dispatch(
                app,
                memory_workspace_fixture,
                renderer_mount,
                invocation,
                invocation_outgoing.clone(),
            )
            .await
            {
                Ok(value) => json!({ "type": "result", "id": id, "ok": true, "value": value }),
                Err(error) => json!({ "type": "result", "id": id, "ok": false, "error": error }),
            };
            let _ = send_json(&invocation_outgoing, response);
        });
    }

    drop(outgoing);
    let _ = writer_task.await;
}

async fn dispatch(
    app: AppHandle,
    memory_workspace_fixture: Option<super::browser_dev_fixture::BrowserDevMemoryWorkspaceFixture>,
    renderer_mount: BrowserRendererMountLease,
    invocation: Invocation,
    outgoing: Outgoing,
) -> Result<Value, String> {
    let args = &invocation.args;
    match invocation.command.as_str() {
        "load_document" => result_value(super::load_document(app.clone(), app.state::<AppState>())),
        "save_document" => result_value(
            super::save_document(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "document")?,
                optional_arg(args, "durable")?,
            )
            .await,
        ),
        "flush_document_saves" => {
            result_value(super::flush_document_saves(app.state::<AppState>()).await)
        }
        "subscribe_app_events" => {
            let channel = bridge_channel::<super::push_events::AppPushEvent>(
                channel_id(args, "onEvent")?,
                outgoing,
            );
            result_value(super::subscribe_app_events(app.state::<AppState>(), channel))
        }
        "create_conversation" => result_value(super::create_conversation(
            app.clone(),
            app.state::<AppState>(),
            arg(args, "workspaceId")?,
            arg(args, "conversation")?,
        )),
        "delete_conversation" => result_value(super::delete_conversation(
            app.clone(),
            app.state::<AppState>(),
            arg(args, "workspaceId")?,
            arg(args, "conversationId")?,
        )),
        "update_conversation" => result_value(super::update_conversation(
            app.clone(),
            app.state::<AppState>(),
            arg(args, "workspaceId")?,
            arg(args, "conversation")?,
            arg(args, "expectedContextIds")?,
        )),
        "reorder_conversations" => result_value(super::reorder_conversations(
            app.clone(),
            app.state::<AppState>(),
            arg(args, "workspaceId")?,
            arg(args, "conversationIds")?,
        )),
        "load_conversation" => result_value(super::load_conversation(
            app.clone(),
            app.state::<AppState>(),
            arg(args, "conversationId")?,
        )),
        "load_conversation_plan" => result_value(super::load_conversation_plan(
            app.clone(),
            app.state::<AppState>(),
            arg(args, "conversationId")?,
        )),
        "token_usage_statistics" => {
            result_value(super::token_usage_statistics(app.clone()).await)
        }
        "backfill_token_usage" => result_value(
            super::backfill_token_usage(app.clone(), arg(args, "events")?).await,
        ),
        "fork_conversation_contexts" => result_value(super::fork_conversation_contexts(
            app.clone(),
            app.state::<AppState>(),
            arg(args, "workspaceId")?,
            arg(args, "sourceConversationId")?,
            arg(args, "targetConversationId")?,
            arg(args, "throughContextId")?,
        )),
        "reset_document" => {
            result_value(super::reset_document(app.clone(), app.state::<AppState>()))
        }
        "discover_capabilities" => result_value(super::discover_capabilities(
            app.clone(),
            app.state::<AppState>(),
        )),
        "mcp_probe_server" => result_value(
            super::mcp_probe_server(arg(args, "server")?, arg(args, "probeId")?).await,
        ),
        "mcp_cancel_probe" => result_value(super::mcp_cancel_probe(arg(args, "probeId")?)),
        // Development bridge installation commands must include `path`: the dev backend
        // cannot answer a system picker.
        "skill_install_directory" => result_value(
            super::skill_install_directory(
                app.clone(),
                app.state::<AppState>(),
                Some(arg(args, "path")?),
            )
            .await,
        ),
        "skill_install_archive" => result_value(
            super::skill_install_archive(
                app.clone(),
                app.state::<AppState>(),
                Some(arg(args, "path")?),
            )
            .await,
        ),
        "skill_uninstall" => {
            result_value(super::skill_uninstall(app.clone(), arg(args, "folderName")?).await)
        }
        "skill_search_registries" => {
            result_value(super::skill_search_registries(arg(args, "query")?).await)
        }
        "skill_install_remote" => result_value(
            super::skill_install_remote(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "installSource")?,
            )
            .await,
        ),
        "skill_scan_system" => result_value(
            super::skill_scan_system(app.clone(), app.state::<AppState>()).await,
        ),
        "environment_tool_snapshots" => result_value(
            super::environment_tool_snapshots(app.clone(), app.state::<AppState>()).await,
        ),
        "reveal_environment_tool" => result_value(
            super::reveal_environment_tool(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "executable")?,
            )
            .await,
        ),
        "list_wsl_distros" => result_value(super::list_wsl_distros().await),
        "reveal_path_in_file_manager" => result_value(
            super::reveal_path_in_file_manager(
                arg(args, "path")?,
                optional_arg(args, "baseDir")?,
            )
            .await,
        ),
        "open_external_url" => {
            result_value(super::open_external_url(arg(args, "url")?).await)
        }
        "app_version_info" => result_value(super::app_version_info(app.clone())),
        "check_app_update" => result_value(super::check_app_update(app.clone()).await),
        "download_app_update" => {
            let channel = bridge_channel::<super::app_update::DownloadEvent>(
                channel_id(args, "onProgress")?,
                outgoing,
            );
            result_value(
                super::download_app_update(
                    app.clone(),
                    app.state::<AppState>(),
                    arg(args, "asset")?,
                    optional_arg(args, "checksumsAsset")?,
                    channel,
                )
                .await,
            )
        }
        "cancel_app_update_download" => {
            result_value(super::cancel_app_update_download(app.state::<AppState>()))
        }
        "install_app_update" => result_value(
            super::install_app_update(app.clone(), app.state::<AppState>(), arg(args, "path")?)
                .await,
        ),
        "mework_memory_overview" => result_value(super::mework_memory_overview(
            app.clone(),
            app.state::<AppState>(),
            optional_arg(args, "workspaceId")?,
        )),
        "mework_memory_read_file" => result_value(super::mework_memory_read_file(
            app.clone(),
            app.state::<AppState>(),
            optional_arg(args, "workspaceId")?,
            arg(args, "tier")?,
            arg(args, "name")?,
        )),
        "mework_memory_write_file" => result_value(super::mework_memory_write_file(
            app.clone(),
            app.state::<AppState>(),
            optional_arg(args, "workspaceId")?,
            arg(args, "tier")?,
            arg(args, "name")?,
            arg(args, "content")?,
        )),
        "mework_memory_delete_document" => result_value(super::mework_memory_delete_document(
            app.clone(),
            app.state::<AppState>(),
            optional_arg(args, "workspaceId")?,
            arg(args, "tier")?,
            arg(args, "name")?,
        )),
        "project_memory_list_import_trust" => {
            result_value(super::project_memory_list_import_trust(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "workspaceId")?,
            ))
        }
        "project_memory_revoke_import_trust" => {
            result_value(super::project_memory_revoke_import_trust(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "workspaceId")?,
                arg(args, "recordId")?,
            ))
        }
        "execute_tool" => result_value(
            super::execute_tool(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "request")?,
                optional_arg(args, "approvalNonce")?,
            )
            .await,
        ),
        "request_tool_approval" => result_value(
            super::request_tool_approval(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "request")?,
            )
            .await,
        ),
        "resolve_tool_prompt" => result_value(super::resolve_tool_prompt(
            app.clone(),
            app.state::<AppState>(),
            arg(args, "promptId")?,
            arg(args, "decision")?,
            optional_arg(args, "feedback")?,
        )),
        "pick_workspace_directory" => {
            if let Some(fixture) = memory_workspace_fixture.as_ref() {
                // Renderer IPC has no path argument. The only non-dialog result is this
                // startup-validated host fixture, revalidated immediately before authorization.
                let path = fixture.authorized_workspace_path()?;
                result_value(app.state::<AppState>().authorize_workspace(&path).map(Some))
            } else {
                result_value(
                    super::pick_workspace_directory(app.clone(), app.state::<AppState>()).await,
                )
            }
        }
        "save_api_key" => result_value(
            super::save_api_key(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "provider")?,
                arg(args, "apiKey")?,
            )
            .await,
        ),
        "reveal_api_key" => result_value(
            super::reveal_api_key(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "provider")?,
            )
            .await,
        ),
        "delete_api_key" => result_value(
            super::delete_api_key(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "provider")?,
            )
            .await,
        ),
        "codex_oauth_sign_in" => result_value(
            super::codex_oauth_sign_in(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "provider")?,
            )
            .await,
        ),
        "codex_oauth_cancel_sign_in" => result_value(
            super::codex_oauth_cancel_sign_in(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "provider")?,
            )
            .await,
        ),
        "codex_oauth_status" => result_value(
            super::codex_oauth_status(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "provider")?,
            )
            .await,
        ),
        "codex_oauth_sign_out" => result_value(
            super::codex_oauth_sign_out(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "provider")?,
            )
            .await,
        ),
        "claude_agent_login_status" => result_value(
            super::claude_agent_login_status(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "provider")?,
            )
            .await,
        ),
        "claude_agent_open_login" => result_value(
            super::claude_agent_open_login(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "provider")?,
            )
            .await,
        ),
        "save_search_api_key" => result_value(
            super::save_search_api_key(
                app.state::<AppState>(),
                arg(args, "providerKind")?,
                arg(args, "slot")?,
                arg(args, "apiKey")?,
            )
            .await,
        ),
        "get_search_key_status" => result_value(
            super::get_search_key_status(
                app.state::<AppState>(),
                arg(args, "providerKind")?,
                arg(args, "slot")?,
            )
            .await,
        ),
        "reveal_search_api_key" => result_value(
            super::reveal_search_api_key(
                app.state::<AppState>(),
                arg(args, "providerKind")?,
                arg(args, "slot")?,
            )
            .await,
        ),
        "delete_search_api_key" => result_value(
            super::delete_search_api_key(
                app.state::<AppState>(),
                arg(args, "providerKind")?,
                arg(args, "slot")?,
            )
            .await,
        ),
        "fetch_models" => result_value(
            super::fetch_models(app.clone(), app.state::<AppState>(), arg(args, "provider")?).await,
        ),
        "run_model" => {
            let channel =
                bridge_channel::<ModelStreamEvent>(channel_id(args, "onEvent")?, outgoing);
            result_value(
                super::run_model(
                    app.clone(),
                    app.state::<AppState>(),
                    arg(args, "request")?,
                    arg(args, "requestId")?,
                    channel,
                    optional_arg(args, "forkPromptContextId")?,
                )
                .await,
            )
        }
        "cancel_model_run" => result_value(super::cancel_model_run(
            app.state::<AppState>(),
            arg(args, "requestId")?,
        )),
        "cancel_conversation_run" => result_value(super::cancel_conversation_run(
            app.state::<AppState>(),
            arg(args, "conversationId")?,
        )),
        "attach_model_run" => {
            let channel =
                bridge_channel::<ModelStreamEvent>(channel_id(args, "onEvent")?, outgoing);
            result_value(super::attach_model_run(
                app.state::<AppState>(),
                arg(args, "conversationId")?,
                channel,
            ))
        }
        "take_run_settlement" => result_value(super::take_run_settlement(
            app.state::<AppState>(),
            arg(args, "conversationId")?,
        )),
        "list_resumable_runs" => result_value(super::list_resumable_runs(app.state::<AppState>())),
        "steer_model_run" => result_value(super::steer_model_run(
            app.state::<AppState>(),
            arg(args, "requestId")?,
            arg(args, "messageId")?,
            arg(args, "content")?,
            optional_arg(args, "images")?,
            arg(args, "createdAt")?,
        )),
        "workflow_step_control" => result_value(super::workflow_step_control(
            app.state::<AppState>(),
            arg(args, "requestId")?,
            arg(args, "runId")?,
            arg(args, "stepIndex")?,
            arg(args, "action")?,
        )),
        "workflow_run_history" => result_value(super::workflow_run_history(
            app.clone(), arg(args, "conversationId")?,
        ).await),
        "workflow_step_record" => result_value(super::workflow_step_record(
            app.clone(),
            arg(args, "conversationId")?,
            arg(args, "runId")?,
            arg(args, "stepIndex")?,
        )),
        "image_attachment_upload" => result_value(super::image_attachment_upload(
            app.clone(),
            arg(args, "name")?,
            arg(args, "data")?,
        )),
        "image_attachment_data" => result_value(super::image_attachment_data(
            app.clone(),
            arg(args, "imageId")?,
        )),
        "browser_register_renderer_mount" => serde_json::to_value(&renderer_mount)
            .map_err(|error| format!("无法序列化 browser-dev renderer mount: {error}")),
        "browser_renderer_mount_heartbeat" => result_value(
            app.state::<AppState>()
                .browser_renderer_mounts
                .heartbeat(&renderer_mount.mount_id, renderer_mount.generation)
                .map_err(super::browser_renderer_mount_error),
        ),
        "browser_open" => result_value(
            super::browser_open(
                app.state::<AppState>(),
                optional_arg(args, "sessionId")?.unwrap_or_else(|| "browser-dev".to_owned()),
                optional_arg(args, "url")?,
                arg(args, "lifecycleEpoch")?,
                renderer_mount.mount_id.clone(),
                renderer_mount.generation,
            )
            .await,
        ),
        "browser_close" => result_value(
            super::browser_close(
                app.state::<AppState>(),
                optional_arg(args, "sessionId")?.unwrap_or_else(|| "browser-dev".to_owned()),
                arg(args, "lifecycleEpoch")?,
                renderer_mount.mount_id.clone(),
                renderer_mount.generation,
            )
            .await,
        ),
        "browser_status" => result_value(
            super::browser_status(
                app.state::<AppState>(),
                optional_arg(args, "sessionId")?.unwrap_or_else(|| "browser-dev".to_owned()),
                renderer_mount.mount_id.clone(),
                renderer_mount.generation,
            )
            .await,
        ),
        "browser_set_panel_bounds" => result_value(
            super::browser_set_panel_bounds(
                app.state::<AppState>(),
                optional_arg(args, "sessionId")?.unwrap_or_else(|| "browser-dev".to_owned()),
                arg(args, "bounds")?,
                arg(args, "lifecycleEpoch")?,
                renderer_mount.mount_id.clone(),
                renderer_mount.generation,
            )
            .await,
        ),
        "browser_navigate" => result_value(
            super::browser_navigate(
                app.state::<AppState>(),
                optional_arg(args, "sessionId")?.unwrap_or_else(|| "browser-dev".to_owned()),
                arg(args, "url")?,
                renderer_mount.mount_id.clone(),
                renderer_mount.generation,
            )
            .await,
        ),
        "browser_action" => result_value(
            super::browser_action(
                app.clone(),
                app.state::<AppState>(),
                optional_arg(args, "sessionId")?.unwrap_or_else(|| "browser-dev".to_owned()),
                arg(args, "action")?,
                optional_arg(args, "value")?,
                optional_arg(args, "lifecycleEpoch")?,
                renderer_mount.mount_id.clone(),
                renderer_mount.generation,
            )
            .await,
        ),
        "get_git_workspace_snapshot" => result_value(
            super::get_git_workspace_snapshot(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
            )
            .await,
        ),
        "get_git_workspace_summary" => result_value(
            super::get_git_workspace_summary(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                optional_arg(args, "knownRevision")?,
            )
            .await,
        ),
        "get_git_change_page" => result_value(
            super::get_git_change_page(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "request")?,
            )
            .await,
        ),
        "get_git_diff" => result_value(
            super::get_git_diff(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "request")?,
            )
            .await,
        ),
        "get_git_branches" => result_value(
            super::get_git_branches(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
            )
            .await,
        ),
        "create_conversation_worktree" => result_value(
            super::create_conversation_worktree(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "conversationId")?,
                optional_arg(args, "fromBranch")?,
            )
            .await,
        ),
        "release_conversation_worktree" => result_value(
            super::release_conversation_worktree(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "conversationId")?,
            )
            .await,
        ),
        "get_git_history" => result_value(
            super::get_git_history(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "request")?,
            )
            .await,
        ),
        "prepare_git_discard" => result_value(
            super::prepare_git_discard(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "paths")?,
                arg(args, "includeUntracked")?,
            )
            .await,
        ),
        "prepare_git_stage_all" => result_value(
            super::prepare_git_stage_all(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
            )
            .await,
        ),
        "prepare_git_commit" => result_value(
            super::prepare_git_commit(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "message")?,
            )
            .await,
        ),
        "execute_git_action" => result_value(
            super::execute_git_action(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "action")?,
            )
            .await,
        ),
        "get_github_repository" => result_value(
            super::get_github_repository(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
            )
            .await,
        ),
        "get_github_pull_requests" => result_value(
            super::get_github_pull_requests(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "page")?,
                arg(args, "pageSize")?,
            )
            .await,
        ),
        "get_github_pull_request_detail" => result_value(
            super::get_github_pull_request_detail(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "number")?,
            )
            .await,
        ),
        "get_github_pull_request_readiness" => result_value(
            super::get_github_pull_request_readiness(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "number")?,
            )
            .await,
        ),
        "get_github_pull_request_diff" => result_value(
            super::get_github_pull_request_diff(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "number")?,
                optional_arg(args, "path")?,
            )
            .await,
        ),
        "get_github_pull_request_review_threads" => result_value(
            super::get_github_pull_request_review_threads(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "request")?,
            )
            .await,
        ),
        "get_github_pull_request_review_thread_comments" => result_value(
            super::get_github_pull_request_review_thread_comments(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "request")?,
            )
            .await,
        ),
        "execute_github_action" => result_value(
            super::execute_github_action(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "target")?,
                arg(args, "action")?,
            )
            .await,
        ),
        // Development-only authenticated bridge used by the real-WebView browser tool suite.
        // Production model calls still go through execute_tool and its security classifier.
        "browser_execute_agent_tool" => {
            let tool_name: String = arg(args, "toolName")?;
            if tool_name != "playwright" {
                return Err("browser_execute_agent_tool only permits the playwright tool".into());
            }
            let session_id =
                optional_arg(args, "sessionId")?.unwrap_or_else(|| "browser-dev".to_owned());
            let input: Map<String, Value> = arg(args, "input")?;
            let action = crate::browser::PlaywrightAction::from_input(&input)?;
            // Tab actions are manager-level, so the harness must reach them through the same
            // dispatcher production uses rather than through one page's tool table.
            if action.is_tab_action() {
                return result_value(app.state::<AppState>().browser.execute_agent_tab_tool(
                    &session_id,
                    action,
                    &input,
                ));
            }
            let grants = browser_e2e_grants(&app, action)?;
            result_value(
                app.state::<AppState>()
                    .browser
                    .execute_tool(session_id, action, input, grants)
                    .await,
            )
        }
        "browser_e2e_screenshot_data" => {
            let path = browser_e2e_directory(&app)?.join("latest.png");
            let bytes = std::fs::read(&path)
                .map_err(|error| format!("Could not read browser E2E screenshot: {error}"))?;
            if bytes.len() > 64 * 1024 * 1024 {
                return Err("Browser E2E screenshot exceeds the 64 MiB safety limit".into());
            }
            value(format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ))
        }
        "browser_e2e_live_page_count" => value(app.state::<AppState>().browser.live_page_count()),
        "browser_e2e_instance_id" => value(active_browser_e2e_instance_id()?),
        "browser_e2e_cleanup_web_search_keys" => {
            result_value(browser_e2e_cleanup_web_search_keys(&app).await)
        }
        "browser_e2e_cleanup_image_input_keys" => {
            result_value(browser_e2e_cleanup_image_input_keys(&app).await)
        }
        "browser_e2e_restart_backend" => {
            // The replacement backend re-attaches to this instance's isolated user-data folder, so
            // a leaked browser process would collide with it. The image E2E is the run that drives
            // real browser tools before restarting, and it must therefore prove those processes
            // actually exited rather than merely that its pages were closed.
            accept_browser_e2e_controlled_exit(
                &app,
                args,
                ControlledExit::Restart,
                image_input_e2e_policy_active(),
            )
        }
        "browser_e2e_shutdown_backend" => {
            // Unlike a restart, the final shutdown is the image-input E2E's WebView2 release
            // acceptance step, and the wrapper only ever admits it for that run identity.
            if !image_input_e2e_policy_active() {
                return Err("浏览器 E2E 受控关停握手未启用".into());
            }
            // Nothing re-attaches to the folder afterwards, and the backend that reaches this point
            // is the post-restart instance, which never opened a page of its own. Releasing zero
            // processes is therefore a real outcome here, not missing evidence.
            accept_browser_e2e_controlled_exit(&app, args, ControlledExit::FinalShutdown, false)
        }
        "open_terminal" => {
            let channel = bridge_channel::<TerminalEvent>(channel_id(args, "onEvent")?, outgoing);
            result_value(super::open_terminal(
                app.clone(),
                app.state::<AppState>(),
                arg(args, "conversationId")?,
                arg(args, "terminalId")?,
                arg(args, "cols")?,
                arg(args, "rows")?,
                channel,
            ))
        }
        "write_terminal" => result_value(super::write_terminal(
            app.state::<AppState>(),
            arg(args, "conversationId")?,
            arg(args, "terminalId")?,
            arg(args, "sessionId")?,
            arg(args, "data")?,
        )),
        "resize_terminal" => result_value(super::resize_terminal(
            app.state::<AppState>(),
            arg(args, "conversationId")?,
            arg(args, "terminalId")?,
            arg(args, "sessionId")?,
            arg(args, "cols")?,
            arg(args, "rows")?,
        )),
        "detach_terminal" => value(super::detach_terminal(
            app.state::<AppState>(),
            arg(args, "conversationId")?,
            arg(args, "terminalId")?,
            arg(args, "sessionId")?,
        )),
        "close_terminal" => value(super::close_terminal(
            app.state::<AppState>(),
            arg(args, "conversationId")?,
            arg(args, "terminalId")?,
        )),
        "stop_shell_task" => value(super::stop_shell_task(
            app.state::<AppState>(),
            arg(args, "conversationId")?,
            arg(args, "shellTaskId")?,
        )),
        "stop_conversation_task" => result_value(super::stop_conversation_task(
            app.state::<AppState>(),
            arg(args, "conversationId")?,
            arg(args, "task")?,
        )),
        "list_wake_pending_conversations" => result_value(super::list_wake_pending_conversations(
            app.state::<AppState>(),
        )),
        "list_pending_tool_prompts" => result_value(super::list_pending_tool_prompts(
            app.state::<AppState>(),
        )),
        "list_pending_fork_starts" => result_value(super::list_pending_fork_starts(app.clone())),
        "list_pending_fork_requests" => result_value(super::list_pending_fork_requests(
            app.state::<AppState>(),
        )),
        "resolve_fork_request" => result_value(super::resolve_fork_request(
            app.clone(),
            app.state::<AppState>(),
            arg(args, "forkId")?,
            arg(args, "approved")?,
        )),
        "list_shell_tasks" => value(super::list_shell_tasks(
            app.state::<AppState>(),
            arg(args, "conversationId")?,
        )),
        "open_shell_task_output" => {
            let channel = bridge_channel::<super::shell_tasks::ShellOutputEvent>(
                channel_id(args, "onEvent")?,
                outgoing,
            );
            result_value(super::open_shell_task_output(
                app.state::<AppState>(),
                arg(args, "conversationId")?,
                arg(args, "shellTaskId")?,
                channel,
            ))
        }
        "detach_shell_task_output" => value(super::detach_shell_task_output(
            app.state::<AppState>(),
            arg(args, "conversationId")?,
            arg(args, "shellTaskId")?,
            optional_arg(args, "subscriptionId")?,
        )),
        command => Err(format!("浏览器开发后端不支持命令: {command}")),
    }
}

fn active_browser_e2e_instance_id() -> Result<String, String> {
    // The authenticated loopback WebSocket is necessary but not sufficient: this host-owned
    // policy additionally proves the dedicated browser-dev feature, isolated data identifier,
    // opt-in flag, token strength, and exact loopback backend address.
    if !web_search_e2e_policy_active() && !image_input_e2e_policy_active() {
        return Err("浏览器 E2E 后端重启握手未启用".into());
    }
    let instance_id =
        env::var(INSTANCE_ID_ENV).map_err(|_| format!("缺少或无法读取 {INSTANCE_ID_ENV}"))?;
    if instance_id.len() != 64
        || !instance_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!(
            "{INSTANCE_ID_ENV} 必须是 32 字节随机值的小写十六进制编码"
        ));
    }
    Ok(instance_id)
}

/// Claims this backend instance's single controlled-exit slot and schedules the exit itself.
///
/// Requiring the caller to name the exact instance id keeps a stale renderer — one still talking to
/// a previous backend generation — from taking down the replacement it never observed.
fn accept_browser_e2e_controlled_exit(
    app: &AppHandle,
    args: &Value,
    exit: ControlledExit,
    require_browser_process: bool,
) -> Result<Value, String> {
    let requested_instance_id: String = arg(args, "instanceId")?;
    let current_instance_id = active_browser_e2e_instance_id()?;
    if requested_instance_id != current_instance_id {
        return Err("浏览器 E2E 受控退出实例标识不匹配".into());
    }
    if !BROWSER_E2E_EXIT.accept(exit) {
        return Err("当前浏览器 E2E 后端实例已接受过受控退出请求".into());
    }
    // A rejected request must leave this instance's release requirements untouched, so raise them
    // after the slot is claimed rather than before.
    if require_browser_process {
        BROWSER_E2E_EXIT.set_require_browser_process();
    }

    // This fixed host marker lets the Node wrapper distinguish an accepted controlled exit from a
    // coincidental `cargo run` exit code. It intentionally contains no instance id.
    println!("{}", exit.request_marker());
    let _ = std::io::stdout().flush();

    // Return the authenticated bridge result first. The delayed AppHandle exit then enters the
    // ordinary ExitRequested path, so document/import/browser cleanup is not bypassed.
    let exiting_app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(BROWSER_E2E_CONTROLLED_EXIT_DELAY).await;
        exiting_app.exit(exit.requested_exit_code());
    });
    value(json!({
        "accepted": true,
        "instanceId": current_instance_id,
    }))
}

/// Whether this process is a genuine web-search E2E backend.
///
/// No bundled search or fetch service has a host URL policy to substitute, so
/// the E2E points the search provider's own `base_url` at the protocol mock.
/// The environment proof gates the restart handshake and credential cleanup;
/// these invariants are checked directly and fail closed.
fn web_search_e2e_policy_active() -> bool {
    if env::var(WEB_SEARCH_E2E_ENABLE_ENV).as_deref() != Ok("1") {
        return false;
    }
    let Ok(data_identifier) = env::var(DATA_IDENTIFIER_ENV) else {
        return false;
    };
    let Ok(run_id) = env::var(WEB_SEARCH_E2E_RUN_ID_ENV) else {
        return false;
    };
    let Ok(token) = env::var(TOKEN_ENV) else {
        return false;
    };
    let Ok(origin) = env::var(ORIGIN_ENV) else {
        return false;
    };
    let Ok(address) = env::var(ADDRESS_ENV).and_then(|value| {
        value
            .parse::<SocketAddr>()
            .map_err(|_| env::VarError::NotPresent)
    }) else {
        return false;
    };
    data_identifier.starts_with(WEB_SEARCH_E2E_DATA_IDENTIFIER_PREFIX)
        && data_identifier.len() > WEB_SEARCH_E2E_DATA_IDENTIFIER_PREFIX.len()
        && data_identifier.len() <= 128
        && run_id.len() == 24
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        && token.len() == 64
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        && validate_origin(&origin).is_ok()
        && address.ip().is_loopback()
}

fn image_input_e2e_policy_active() -> bool {
    if env::var(ORIGINLESS_IMAGE_CLEANUP_ENV).as_deref() != Ok("1") {
        return false;
    }
    let Ok(data_identifier) = env::var(DATA_IDENTIFIER_ENV) else {
        return false;
    };
    let Ok(run_id) = env::var(IMAGE_INPUT_E2E_RUN_ID_ENV) else {
        return false;
    };
    let Ok(token) = env::var(TOKEN_ENV) else {
        return false;
    };
    let Ok(origin) = env::var(ORIGIN_ENV) else {
        return false;
    };
    let Ok(address) = env::var(ADDRESS_ENV).and_then(|value| {
        value
            .parse::<SocketAddr>()
            .map_err(|_| env::VarError::NotPresent)
    }) else {
        return false;
    };
    data_identifier.starts_with("com.mework.app.e2e.image-input-")
        && data_identifier.len() > "com.mework.app.e2e.image-input-".len()
        && data_identifier.len() <= 128
        && data_identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && run_id.len() == 24
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        && token.len() == 64
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        && validate_origin(&origin).is_ok()
        && address.ip().is_loopback()
}

async fn browser_e2e_cleanup_web_search_keys(app: &AppHandle) -> Result<Value, String> {
    if !web_search_e2e_policy_active() {
        return Err("联网搜索 E2E Key 清理未启用".into());
    }
    let run_id = env::var(WEB_SEARCH_E2E_RUN_ID_ENV)
        .map_err(|_| format!("缺少或无法读取 {WEB_SEARCH_E2E_RUN_ID_ENV}"))?;
    if run_id.len() != 24
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!(
            "{WEB_SEARCH_E2E_RUN_ID_ENV} 必须是 12 字节随机值的小写十六进制编码"
        ));
    }

    let state = app.state::<AppState>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Native search uses the conversation's provider, so tests covering both wire
        // protocols need two entries. Do not modify the fixed search-provider catalog:
        // writing mock keys there would replace real user configuration.
        let provider_ids = ["openai", "anthropic"]
            .map(|wire| format!("web-search-e2e-provider-{wire}-{run_id}"));
        let mut errors = Vec::new();
        let mut configured = Vec::new();
        for provider_id in &provider_ids {
            match super::api::delete_api_key(provider_id) {
                Ok(status) => configured.push(status.configured),
                Err(error) => {
                    errors.push(format!("{provider_id}: {error}"));
                    configured.push(true);
                }
            }
        }
        if !errors.is_empty() {
            return Err(format!(
                "联网搜索 E2E 假 Key 清理未全部完成：{}",
                errors.join("；")
            ));
        }
        Ok(json!({
            "providerIds": provider_ids,
            "providerConfigured": configured.iter().any(|value| *value),
        }))
    })
    .await
    .map_err(|error| format!("清理联网搜索 E2E 假 Key 的后台任务失败: {error}"))?
}

async fn browser_e2e_cleanup_image_input_keys(app: &AppHandle) -> Result<Value, String> {
    if !image_input_e2e_policy_active() {
        return Err("图片输入 E2E Key 清理未启用".into());
    }
    let data_identifier = env::var(DATA_IDENTIFIER_ENV)
        .map_err(|_| format!("缺少或无法读取 {DATA_IDENTIFIER_ENV}"))?;
    if !data_identifier.starts_with("com.mework.app.e2e.image-input-")
        || data_identifier.len() <= "com.mework.app.e2e.image-input-".len()
        || data_identifier.len() > 128
        || !data_identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("图片输入 E2E Key 清理仅允许隔离的 image-input 应用数据标识".into());
    }
    let run_id = env::var(IMAGE_INPUT_E2E_RUN_ID_ENV)
        .map_err(|_| format!("缺少或无法读取 {IMAGE_INPUT_E2E_RUN_ID_ENV}"))?;
    if run_id.len() != 24
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!(
            "{IMAGE_INPUT_E2E_RUN_ID_ENV} 必须是 12 字节随机值的小写十六进制编码"
        ));
    }

    let state = app.state::<AppState>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // These must match `imageE2eProviderId` in the renderer exactly: the fake key is stored
        // under the id the runner registers, and the runner compares this list against its own.
        let provider_ids = [
            format!("image-e2e-openai_chat-{run_id}"),
            format!("image-e2e-openai_responses-{run_id}"),
            format!("image-e2e-anthropic-{run_id}"),
        ];
        let mut configured = Vec::with_capacity(provider_ids.len());
        let mut errors = Vec::new();
        for provider_id in &provider_ids {
            match super::api::delete_api_key(provider_id) {
                Ok(status) => configured.push(status.configured),
                Err(error) => {
                    configured.push(true);
                    errors.push(format!("{provider_id}: {error}"));
                }
            }
        }
        if !errors.is_empty() {
            return Err(format!(
                "图片输入 E2E 假 Key 清理未全部完成：{}",
                errors.join("；")
            ));
        }
        Ok(json!({
            "providerIds": provider_ids,
            "configured": configured
        }))
    })
    .await
    .map_err(|error| format!("图片输入 E2E Key 清理后台任务失败: {error}"))?
}

/// Development-only filesystem grants for the real-WebView E2E bridge. Production tool calls build
/// their grants inside `tool_executor` behind the security classifier; this bypass exists only so
/// the E2E harness can exercise the `screenshot` and `file_upload` actions end to end.
fn browser_e2e_grants(
    app: &AppHandle,
    action: crate::browser::PlaywrightAction,
) -> Result<crate::browser::BrowserToolGrants, String> {
    use crate::browser::PlaywrightAction as Action;
    match action {
        Action::Screenshot => Ok(crate::browser::BrowserToolGrants {
            screenshot_path: Some(browser_e2e_directory(app)?.join("latest.png")),
            upload_paths: None,
        }),
        Action::FileUpload => {
            let fixture = browser_e2e_directory(app)?.join("upload-fixture.txt");
            std::fs::write(&fixture, b"MEWORK_UPLOAD_FIXTURE")
                .map_err(|error| format!("Could not create upload fixture file: {error}"))?;
            Ok(crate::browser::BrowserToolGrants {
                screenshot_path: None,
                upload_paths: Some(vec![fixture]),
            })
        }
        // Production materializes the attachment inside the run loop; the E2E
        // bridge has no transcript, so it stands in a fixed 1×1 PNG fixture.
        Action::UploadImage => {
            let fixture = browser_e2e_directory(app)?.join("upload-image-fixture.png");
            std::fs::write(&fixture, BROWSER_E2E_UPLOAD_FIXTURE_PNG)
                .map_err(|error| format!("Could not create image-upload fixture file: {error}"))?;
            Ok(crate::browser::BrowserToolGrants {
                screenshot_path: None,
                upload_paths: Some(vec![fixture]),
            })
        }
        _ => Ok(crate::browser::BrowserToolGrants::default()),
    }
}

fn browser_e2e_directory(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let directory = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve application data directory: {error}"))?
        .join("image-input-browser-e2e");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create browser E2E directory: {error}"))?;
    Ok(directory)
}

fn arg<T: DeserializeOwned>(args: &Value, key: &str) -> Result<T, String> {
    let value = args
        .get(key)
        .cloned()
        .ok_or_else(|| format!("调用参数缺少 {key}"))?;
    serde_json::from_value(value).map_err(|error| format!("调用参数 {key} 无效: {error}"))
}

fn optional_arg<T: DeserializeOwned>(args: &Value, key: &str) -> Result<Option<T>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| format!("调用参数 {key} 无效: {error}")),
    }
}

fn channel_id(args: &Value, key: &str) -> Result<String, String> {
    let id = args
        .get(key)
        .and_then(|value| value.get("__meworkChannel"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("调用参数 {key} 缺少开发通道标识"))?;
    if id.is_empty() || id.len() > 128 {
        return Err(format!("调用参数 {key} 的开发通道标识无效"));
    }
    Ok(id.to_owned())
}

fn bridge_channel<T>(channel_id: String, outgoing: Outgoing) -> Channel<T> {
    Channel::new(move |body| {
        let payload = match body {
            InvokeResponseBody::Json(json) => serde_json::from_str::<Value>(&json)?,
            InvokeResponseBody::Raw(bytes) => serde_json::to_value(bytes)?,
        };
        send_json(
            &outgoing,
            json!({ "type": "channel", "channelId": channel_id, "payload": payload }),
        )
        .map_err(|error| tauri::Error::Io(std::io::Error::other(error)))
    })
}

fn result_value<T: Serialize>(result: Result<T, String>) -> Result<Value, String> {
    result.and_then(value)
}

fn value<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| format!("无法序列化命令结果: {error}"))
}

fn send_json(outgoing: &Outgoing, value: Value) -> Result<(), String> {
    let text = serde_json::to_string(&value)
        .map_err(|error| format!("无法序列化开发通道消息: {error}"))?;
    outgoing
        .send(Message::Text(text.into()))
        .map_err(|_| "浏览器开发通道已关闭".to_owned())
}
