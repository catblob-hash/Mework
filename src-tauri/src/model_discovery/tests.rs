//! Guard tests for `model_discovery`.
//!
//! Fetcher selection tests use a stub server that replays a fixed response
//! sequence. Projection tests for deduplication, grouping, naming, catalog
//! enrichment, and heuristic fallback call [`super::parse_openai_compatible`]
//! without a server.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::json;

use super::*;

// ───────────────────────────── Stub server ─────────────────────────────
//
// Every response uses `Connection: close`, so reqwest opens one connection per
// request and the `accept()` count equals the request count.

fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if request.len() >= header_end + 4 + content_length {
            break;
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}

fn write_response(stream: &mut TcpStream, status: &str, body: &Value) {
    let body = body.to_string();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).unwrap();
    stream.flush().unwrap();
}

/// Starts a server that replays a fixed response sequence, returning its address
/// and a handle for received requests.
fn serve(responses: Vec<(&'static str, Value)>) -> (SocketAddr, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_request(&mut stream));
            write_response(&mut stream, status, &body);
        }
        requests
    });
    (address, handle)
}

fn ok(body: Value) -> (&'static str, Value) {
    ("200 OK", body)
}

fn provider(format: ProviderFamily, base_url: String) -> ApiProvider {
    ApiProvider {
        // Provider IDs are renderer-generated UUIDs. Use a UUID-shaped constant
        // instead of a vendor name to prevent accidental identity matching.
        id: "provider_9f1c0b7a4e5d4f2ab3c6d8e0f1a2b3c4".into(),
        name: "Test Provider".into(),
        enabled: true,
        family: format,
        base_url,
        family_settings: Default::default(),
        endpoint_base_urls: Default::default(),
        notes: String::new(),
        models: Vec::new(),
        active_model_id: None,
    }
}

fn local(format: ProviderFamily, address: SocketAddr, path: &str) -> ApiProvider {
    provider(format, format!("http://{address}{path}"))
}

fn offline() -> ApiProvider {
    provider(ProviderFamily::OpenaiChat, "https://example.com/v1".into())
}

fn ids(models: &[ModelProfile]) -> Vec<&str> {
    models.iter().map(|model| model.id.as_str()).collect()
}

// ───────────────────────────── Fetcher selection ─────────────────────────────

#[test]
fn the_chat_protocol_is_the_only_thing_that_picks_a_leg() {
    // The protocol is the sole fetcher-selection criterion. It is a first-class,
    // user-configurable `ApiProvider` field.
    let cases: &[(ProviderFamily, Fetcher)] = &[
        (ProviderFamily::Anthropic, Fetcher::Anthropic),
        (ProviderFamily::OpenaiCodex, Fetcher::Codex),
        (ProviderFamily::ClaudeAgent, Fetcher::ClaudeAgent),
        (ProviderFamily::OpenaiChat, Fetcher::OpenAiCompatible),
        (ProviderFamily::OpenaiResponses, Fetcher::OpenAiCompatible),
        (ProviderFamily::OpenaiCompatible, Fetcher::OpenAiCompatible),
        (ProviderFamily::Google, Fetcher::OpenAiCompatible),
        (ProviderFamily::Xai, Fetcher::OpenAiCompatible),
        (ProviderFamily::Azure, Fetcher::OpenAiCompatible),
        (ProviderFamily::Bedrock, Fetcher::OpenAiCompatible),
        (ProviderFamily::Vertex, Fetcher::OpenAiCompatible),
    ];
    for (format, expected) in cases {
        let chosen = select(&provider(*format, "https://example.com/v1".into()));
        assert_eq!(chosen, *expected, "{format:?} 选错了 fetcher");
    }
    // Require both outcomes so a constant fallback implementation cannot pass
    // the table unchecked.
    assert!(cases.iter().any(|(_, leg)| *leg == Fetcher::Anthropic));
    assert!(cases
        .iter()
        .any(|(_, leg)| *leg == Fetcher::OpenAiCompatible));
}

#[test]
fn a_vendor_named_provider_id_no_longer_buys_a_special_leg() {
    // A custom provider named `ollama` must use the generic leg. This detects
    // residual provider-ID matching in `select`.
    let mut named = provider(ProviderFamily::OpenaiChat, "https://example.com/v1".into());
    named.id = "ollama".into();
    named.name = "openrouter".into();
    assert_eq!(select(&named), Fetcher::OpenAiCompatible);
}

// ───────────────────────────── Individual fetchers ─────────────────────────────

#[test]
fn the_anthropic_leg_asks_for_the_whole_catalog_not_the_default_page() {
    let (address, server) = serve(vec![ok(json!({"data":[
        {"id":"claude-sonnet-4-5-20250929","display_name":"Claude Sonnet 4.5"},
        {"id":"claude-opus-4-7-20260115","display_name":"Claude Opus 4.7"}
    ],"has_more":false}))]);
    let provider = local(ProviderFamily::Anthropic, address, "/v1");
    let models = fetch_models(&provider).unwrap();
    let request = server.join().unwrap().remove(0);

    assert!(
        request.starts_with("GET /v1/models?limit=1000 HTTP/1.1"),
        "Anthropic 的 /v1/models 默认只回 20 条，不带 limit 就永远缺一半: {request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("anthropic-version: 2023-06-01"),
        "缺少 anthropic-version: {request}"
    );
    assert_eq!(
        ids(&models),
        vec!["claude-sonnet-4-5-20250929", "claude-opus-4-7-20260115"]
    );
    assert_eq!(models[0].name, "Claude Sonnet 4.5");
}

#[test]
fn the_generic_leg_keeps_everything_the_relay_lists() {
    let (address, server) = serve(vec![ok(json!({"data":[
        {"id":"gpt-4o"},
        {"id":"whisper-1"}
    ]}))]);
    let provider = local(ProviderFamily::OpenaiChat, address, "/v1");
    let models = fetch_models(&provider).unwrap();
    let request = server.join().unwrap().remove(0);
    assert!(request.starts_with("GET /v1/models HTTP/1.1"), "{request}");
    // Generic relays list exactly the models they expose.
    assert_eq!(ids(&models), vec!["gpt-4o", "whisper-1"]);
}

/// The Codex leg is the ChatGPT backend's own catalog: OAuth Bearer plus the
/// account header, a mandatory `client_version`, the `models` envelope keyed by
/// `slug`, priority order, and capabilities read from the fields that catalog
/// actually carries (`input_modalities`, `supported_reasoning_levels`,
/// `supports_parallel_tool_calls`, `context_window`). Hidden entries stay: the
/// backend still serves them, it just does not advertise them.
#[test]
fn the_codex_leg_lists_the_chatgpt_catalog_with_the_oauth_session() {
    let (address, server) = serve(vec![ok(json!({"models":[
        {"slug":"gpt-5.5","display_name":"GPT-5.5","visibility":"list","priority":12,
         "input_modalities":["text","image"],
         "supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}],
         "supports_parallel_tool_calls":true,"context_window":272000},
        {"slug":"gpt-6-astra","display_name":"GPT-6-Astra","visibility":"list","priority":1,
         "input_modalities":["text","image"],
         "supported_reasoning_levels":[{"effort":"low"}],
         "supports_parallel_tool_calls":true,"context_window":272000},
        {"slug":"codex-auto-review","display_name":"Codex Auto Review","visibility":"hide","priority":43,
         "input_modalities":["text"],"supported_reasoning_levels":[],
         "supports_parallel_tool_calls":false,"context_window":272000}
    ]}))]);
    let mut provider = local(ProviderFamily::OpenaiCodex, address, "/backend-api/codex");
    provider.id = "provider_codex_discovery_test".into();
    let host = crate::codex_oauth::host();
    let access = crate::codex_oauth::test_support::fake_jwt(json!({
        "https://api.openai.com/auth": {"chatgpt_account_id": "acct_discovery"},
        "exp": chrono::Utc::now().timestamp() + 86_400
    }));
    crate::codex_oauth::test_support::install_session(
        host,
        &provider.id,
        &access,
        "refresh_discovery",
        "acct_discovery",
    );

    let models = fetch_models(&provider).unwrap();
    let request = server.join().unwrap().remove(0);
    host.sign_out(&provider.id).unwrap();

    assert!(
        request.starts_with(&format!(
            "GET /backend-api/codex/models?client_version={} HTTP/1.1",
            crate::codex_oauth::CODEX_MODELS_CLIENT_VERSION
        )),
        "client_version 缺省时后端回 400: {request}"
    );
    let lower = request.to_ascii_lowercase();
    assert!(
        lower.contains(&format!(
            "authorization: bearer {}",
            access.to_ascii_lowercase()
        )),
        "OAuth access token 必须作为 Bearer 发出: {request}"
    );
    assert!(
        lower.contains("chatgpt-account-id: acct_discovery"),
        "缺少账号头: {request}"
    );
    assert!(lower.contains("originator: mework"), "{request}");

    // Backend priority, not upstream array order; the hidden entry survives.
    assert_eq!(
        ids(&models),
        vec!["gpt-6-astra", "gpt-5.5", "codex-auto-review"]
    );
    let astra = &models[0];
    assert!(astra
        .capabilities
        .contains(&ModelCapability::ImageRecognition));
    assert_eq!(astra.context_window, Some(272_000));
    assert_eq!(astra.name, "GPT-6-Astra");
    let review = &models[2];
    assert!(!review
        .capabilities
        .contains(&ModelCapability::ImageRecognition));
}

/// There is no anonymous Codex catalog, so a signed-out row fails before the GET
/// with the sign-in remedy instead of sending an unauthenticated request.
#[test]
fn the_codex_leg_refuses_to_fetch_without_a_session() {
    let mut provider = provider(ProviderFamily::OpenaiCodex, String::new());
    provider.id = "provider_codex_discovery_signed_out".into();
    let error = fetch_models(&provider).unwrap_err();
    assert!(error.contains("登录"), "{error}");
}

// ───────────────────────────── Deduplication, order, and projection ─────────────────────────────

#[test]
fn duplicate_ids_collapse_to_the_first_one_and_upstream_order_survives() {
    let models = parse_openai_compatible(
        &offline(),
        &json!({"data":[
            {"id":"b-model"},
            {"id":"a-model"},
            {"id":" b-model "},
            {"id":"c-model"}
        ]}),
    )
    .unwrap();
    // Preserve upstream order because providers commonly place newer models
    // first.
    assert_eq!(ids(&models), vec!["b-model", "a-model", "c-model"]);
}

#[test]
fn discovery_drops_model_ids_the_document_cannot_persist() {
    let oversized = "m".repeat(crate::model::MAX_MODEL_ID_BYTES + 1);
    let models = parse_openai_compatible(
        &offline(),
        &json!({"data":[
            {"id":"good-model"},
            {"id":oversized},
            {"id":"has\u{0007}control"},
            {"id":"   "},
            {"id":"another-good"}
        ]}),
    )
    .unwrap();

    // Discovery must discard IDs that cannot be persisted so one invalid model
    // cannot poison every later document save.
    assert_eq!(ids(&models), vec!["good-model", "another-good"]);
    for model in &models {
        crate::model::validate_model_id(&model.id)
            .expect("every discovered id must be persistable");
    }
}

#[test]
fn the_group_comes_from_the_id_and_falls_back_to_the_provider_name() {
    let models = parse_openai_compatible(
        &offline(),
        &json!({"data":[
            {"id":"Qwen/Qwen3-8B"},
            {"id":"deepseek-v4-pro"},
            {"id":"grok"}
        ]}),
    )
    .unwrap();
    assert_eq!(models[0].group, "Qwen");
    assert_eq!(models[1].group, "deepseek");
    // Fall back to the provider name when the model family cannot be inferred;
    // provider IDs are not user-facing labels.
    assert_eq!(models[2].group, "Test Provider");
}

#[test]
fn the_catalog_fills_in_what_the_upstream_did_not_say() {
    let models = parse_openai_compatible(&offline(), &json!({"data":[{"id":"gpt-4o"}]})).unwrap();
    assert_eq!(models[0].context_window, Some(128_000));
    assert_eq!(models[0].max_output_tokens, Some(16_384));
    assert_eq!(models[0].name, "GPT-4o");
    assert!(models[0]
        .capabilities
        .contains(&ModelCapability::ImageRecognition));
}

#[test]
fn what_the_upstream_said_wins_over_the_catalog() {
    let models = parse_openai_compatible(
        &offline(),
        &json!({"data":[{"id":"gpt-4o","name":"relay-renamed","context_window":999}]}),
    )
    .unwrap();
    // Upstream data identifies the actual relayed model; the catalog fills only
    // fields the upstream did not provide.
    assert_eq!(models[0].name, "relay-renamed");
    assert_eq!(models[0].context_window, Some(999));
    assert_eq!(
        models[0].max_output_tokens,
        Some(16_384),
        "没说的仍由目录补"
    );
}

#[test]
fn a_capability_the_upstream_denies_is_not_handed_back_by_the_catalog() {
    let models = parse_openai_compatible(
        &offline(),
        &json!({"data":[{"id":"gpt-4o","supports_vision":false}]}),
    )
    .unwrap();
    // An upstream capability denial overrides catalog enrichment; exposing a
    // button that is guaranteed to fail is worse than omitting it.
    assert!(!models[0]
        .capabilities
        .contains(&ModelCapability::ImageRecognition));
    assert!(
        models[0].capabilities.is_empty(),
        "gpt-4o 的目录行只剩视觉一项可存，被否掉后就该是空集"
    );
}

#[test]
fn a_capability_the_upstream_declares_survives_a_catalog_miss() {
    let models = parse_openai_compatible(
        &offline(),
        &json!({"data":[
            {"id":"house-brand-chat-v1"},
            {"id":"house-brand-chat-v2","input_modalities":["text","image"]}
        ]}),
    )
    .unwrap();
    // ID-based inference finds nothing in a house-brand name.
    assert!(models[0].capabilities.is_empty());
    // Explicit modality declarations override ID-based inference.
    assert!(models[1]
        .capabilities
        .contains(&ModelCapability::ImageRecognition));
}

#[test]
fn an_id_the_catalog_never_heard_of_falls_back_to_inference() {
    let models = parse_openai_compatible(
        &offline(),
        &json!({"data":[
            {"id":"house-brand-chat-v1","limit":{"context":65536,"output":8192}},
            {"id":"house-brand-embedding-v1"},
            {"id":"house-brand-reranker-v1"},
            {"id":"house-brand-vision-v1"}
        ]}),
    )
    .unwrap();
    assert_eq!(models[0].context_window, Some(65_536));
    assert_eq!(models[0].max_output_tokens, Some(8_192));

    // Vision is the only capability left to infer, so every non-vision name
    // resolves to the empty set rather than to a default grant.
    for index in 0..3 {
        assert!(
            models[index].capabilities.is_empty(),
            "{} 不该被推断出任何能力",
            models[index].id
        );
    }
    assert!(models[3]
        .capabilities
        .contains(&ModelCapability::ImageRecognition));
}

#[test]
fn a_display_name_equal_to_the_id_is_not_stored() {
    let models = parse_openai_compatible(
        &offline(),
        &json!({"data":[{"id":"house-model","name":"house-model"}]}),
    )
    .unwrap();
    // An empty `name` displays the ID, matching manually added models.
    assert!(models[0].name.is_empty());
}

// ───────────────────────────── Third-party relays ─────────────────────────────
//
// A relay is any endpoint that is not the vendor's own. These tests pin the
// behaviors that previously made the built-in endpoints the only ones whose
// catalog could be listed.

/// Serves raw HTTP responses so a test can express redirects and non-JSON
/// bodies, which [`write_response`] cannot. One connection per queued response.
fn serve_raw(listener: TcpListener, responses: Vec<String>) -> JoinHandle<Vec<String>> {
    thread::spawn(move || {
        let mut requests = Vec::new();
        for raw in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            requests.push(read_request(&mut stream));
            let _ = stream.write_all(raw.as_bytes());
            let _ = stream.flush();
        }
        requests
    })
}

fn raw_json(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn request_lines(requests: &[String]) -> Vec<&str> {
    requests
        .iter()
        .map(|request| request.lines().next().unwrap_or(""))
        .collect()
}

const RELAY_CATALOG: &str =
    r#"{"object":"list","data":[{"id":"gpt-4o"},{"id":"claude-sonnet-4-5"}]}"#;

#[test]
fn an_anthropic_relay_that_authenticates_with_bearer_still_lists_models() {
    // A relay speaking the Messages protocol authenticates its catalog with
    // `Authorization: Bearer` and rejects the Anthropic header shape. Picking the
    // leg by protocol alone left the vendor endpoint as the only one that could
    // list models.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = serve_raw(
        listener,
        vec![
            raw_json(
                "401 Unauthorized",
                r#"{"error":{"message":"missing Authorization header"}}"#,
            ),
            raw_json("200 OK", RELAY_CATALOG),
        ],
    );

    let models = fetch_models(&provider(
        ProviderFamily::Anthropic,
        format!("http://{address}/v1"),
    ))
    .expect("relay leg must recover from the Anthropic-shaped 401");
    assert_eq!(ids(&models), vec!["gpt-4o", "claude-sonnet-4-5"]);
    let requests = server.join().unwrap();
    assert_eq!(
        request_lines(&requests),
        vec![
            "GET /v1/models?limit=1000 HTTP/1.1",
            "GET /v1/models HTTP/1.1"
        ],
        "回退腿必须换成通用的 OpenAI 兼容请求"
    );
    // Changing only the URL is not enough: repeating `anthropic-version` and
    // `x-api-key` reproduces the rejection the first attempt already collected.
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("anthropic-version:"),
        "第一腿必须是 Anthropic 形状: {}",
        requests[0]
    );
    assert!(
        !requests[1]
            .to_ascii_lowercase()
            .contains("anthropic-version:"),
        "回退腿必须换掉 Anthropic 的请求头: {}",
        requests[1]
    );
}

#[test]
fn an_anthropic_endpoint_that_answers_is_never_asked_twice() {
    // The fallback must not double every request the vendor endpoint already
    // answered.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = serve_raw(
        listener,
        vec![raw_json(
            "200 OK",
            r#"{"data":[{"id":"claude-sonnet-4-5","display_name":"Claude Sonnet 4.5"}]}"#,
        )],
    );

    let models = fetch_models(&provider(
        ProviderFamily::Anthropic,
        format!("http://{address}/v1"),
    ))
    .unwrap();
    assert_eq!(ids(&models), vec!["claude-sonnet-4-5"]);
    assert_eq!(
        request_lines(&server.join().unwrap()),
        vec!["GET /v1/models?limit=1000 HTTP/1.1"]
    );
}

#[test]
fn a_gateway_redirect_within_the_same_origin_is_followed() {
    // Gateways in front of a relay canonicalize paths with a 301. Refusing every
    // redirect turned that into an unexplained failure.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = serve_raw(
        listener,
        vec![
            format!("HTTP/1.1 301 Moved Permanently\r\nLocation: http://{address}/v2/models\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
            raw_json("200 OK", RELAY_CATALOG),
        ],
    );

    let models = fetch_models(&provider(
        ProviderFamily::OpenaiChat,
        format!("http://{address}/v1"),
    ))
    .unwrap();
    assert_eq!(ids(&models), vec!["gpt-4o", "claude-sonnet-4-5"]);
    assert_eq!(
        request_lines(&server.join().unwrap()),
        vec!["GET /v1/models HTTP/1.1", "GET /v2/models HTTP/1.1"]
    );
}

#[test]
fn a_redirect_to_another_origin_is_refused_so_the_key_cannot_travel() {
    // `x-api-key`, `x-goog-api-key`, and `api-key` are not headers reqwest strips
    // on a cross-host hop, so following one would hand the key to that host.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = serve_raw(
        listener,
        vec!["HTTP/1.1 301 Moved Permanently\r\nLocation: https://elsewhere.example.com/v1/models\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()],
    );

    let error = fetch_models(&provider(
        ProviderFamily::OpenaiChat,
        format!("http://{address}/v1"),
    ))
    .expect_err("跨站跳转必须停下");
    assert!(error.contains("301"), "{error}");
    assert!(error.contains("跳转"), "错误必须说清楚为什么停下: {error}");
    assert_eq!(
        request_lines(&server.join().unwrap()),
        vec!["GET /v1/models HTTP/1.1"]
    );
}

#[test]
fn a_catalog_nested_one_level_deeper_still_parses() {
    let models = parse_openai_compatible(
        &offline(),
        &json!({"data":{"object":"list","data":[{"id":"m-1"},{"id":"m-2"}]}}),
    )
    .unwrap();
    assert_eq!(ids(&models), vec!["m-1", "m-2"]);
}

#[test]
fn a_gateway_envelope_around_the_openai_envelope_still_parses() {
    // Relays wrap the OpenAI envelope in their own gateway envelope.
    for body in [
        json!({"result":{"data":[{"id":"m-1"}]}}),
        json!({"response":{"models":[{"id":"m-1"}]}}),
        json!({"items":[{"id":"m-1"}]}),
        json!({"list":[{"id":"m-1"}]}),
    ] {
        let models = parse_openai_compatible(&offline(), &body).unwrap();
        assert_eq!(ids(&models), vec!["m-1"], "解不开这层信封: {body}");
    }
}

#[test]
fn entries_keyed_by_something_other_than_id_are_not_dropped() {
    // Each of these is a live relay shape. Dropping the record reported an empty
    // catalog rather than the models the relay actually listed.
    let models = parse_openai_compatible(
        &offline(),
        &json!({"data":[
            {"id":"by-id"},
            {"model":"by-model"},
            {"model_id":"by-model-id"},
            {"modelId":"by-model-id-camel"},
            {"slug":"by-slug"},
            {"name":"by-name"},
            "bare-string",
            {"unrelated":"no identifier"}
        ]}),
    )
    .unwrap();
    assert_eq!(
        ids(&models),
        vec![
            "by-id",
            "by-model",
            "by-model-id",
            "by-model-id-camel",
            "by-slug",
            "by-name",
            "bare-string"
        ]
    );
}

#[test]
fn a_gemini_shaped_catalog_is_not_silently_empty() {
    // Google keys the identifier as `name` behind a `models/` prefix, and relays
    // proxying Gemini pass that shape straight through. Reading only `id`
    // produced an empty list and no error at all.
    let models = parse_openai_compatible(
        &offline(),
        &json!({"models":[
            {"name":"models/gemini-2.5-pro","displayName":"Gemini 2.5 Pro"},
            {"name":"models/gemini-2.5-flash"}
        ]}),
    )
    .unwrap();
    assert_eq!(ids(&models), vec!["gemini-2.5-pro", "gemini-2.5-flash"]);
    assert_eq!(models[0].name, "Gemini 2.5 Pro");
    // `name` was the identifier here, so the `models/` form must never become a
    // label. The second entry has no `displayName`, so the catalog supplies one.
    for model in &models {
        assert!(
            !model.name.starts_with("models/"),
            "标识符不能当成显示名: {}",
            model.name
        );
    }
}

#[test]
fn a_non_json_body_blames_the_base_url_rather_than_the_json_parser() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let body = "<html><title>502 Bad Gateway</title></html>";
    let server = serve_raw(
        listener,
        vec![format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )],
    );

    let error = fetch_models(&provider(
        ProviderFamily::OpenaiChat,
        format!("http://{address}/v1"),
    ))
    .expect_err("HTML 不是模型目录");
    assert!(error.contains("Base URL"), "{error}");
    assert!(
        error.contains(&format!("http://{address}/v1/models")),
        "错误要点名具体地址: {error}"
    );
    let _ = server.join();
}

#[test]
fn a_missing_catalog_endpoint_says_the_base_url_is_probably_short_a_prefix() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = serve_raw(
        listener,
        vec![raw_json("404 Not Found", r#"{"error":"not found"}"#)],
    );

    let error = fetch_models(&provider(
        ProviderFamily::OpenaiChat,
        format!("http://{address}"),
    ))
    .expect_err("404 不是成功");
    assert!(
        error.contains("/v1"),
        "错误要提示常见的 Base URL 缺段: {error}"
    );
    assert_eq!(
        request_lines(&server.join().unwrap()),
        vec!["GET /models HTTP/1.1"]
    );
}

#[test]
fn discovery_asks_for_json_so_a_gateway_cannot_answer_with_its_console() {
    let (address, server) = serve(vec![ok(json!({"data":[{"id":"m-1"}]}))]);
    fetch_models(&provider(
        ProviderFamily::OpenaiChat,
        format!("http://{address}/v1"),
    ))
    .unwrap();
    let request = server.join().unwrap().remove(0).to_ascii_lowercase();
    assert!(request.contains("accept: application/json"), "{request}");
}

// ───────────────────────────── Claude Agent ─────────────────────────────

fn row(
    value: &str,
    resolved: Option<&str>,
    display_name: &str,
    context_window: Option<u64>,
) -> crate::aisdk::agent::AgentModel {
    crate::aisdk::agent::AgentModel {
        value: value.into(),
        resolved_model: resolved.map(str::to_owned),
        display_name: display_name.into(),
        context_window,
    }
}

fn claude_agent_models(rows: Vec<crate::aisdk::agent::AgentModel>) -> Vec<ModelProfile> {
    let provider = provider(ProviderFamily::ClaudeAgent, String::new());
    finish(&provider, dedup(claude_agent_rows(rows)))
}

fn windows(models: &[ModelProfile]) -> Vec<Option<u64>> {
    models.iter().map(|model| model.context_window).collect()
}

/// What the bundled CLI answered for a Claude Max login on 2026-09-25. The Fable
/// row is named by an explicit id, so the listing reads no window for it.
fn subscription_picker() -> Vec<crate::aisdk::agent::AgentModel> {
    vec![
        row(
            "default",
            Some("claude-opus-5-5[1m]"),
            "Default (recommended)",
            Some(1_000_000),
        ),
        row(
            "opus[1m]",
            Some("claude-opus-5-5[1m]"),
            "Opus (1M context)",
            Some(1_000_000),
        ),
        row(
            "claude-fable-5-1[1m]",
            Some("claude-fable-5-1"),
            "Fable",
            None,
        ),
        row("sonnet", Some("claude-sonnet-5"), "Sonnet", Some(1_000_000)),
        row(
            "haiku",
            Some("claude-haiku-4-5-20251001"),
            "Haiku",
            Some(200_000),
        ),
    ]
}

/// The same CLI under an API key: bare budgets are 200k, and Sonnet is listed
/// twice, once per budget.
fn api_key_picker() -> Vec<crate::aisdk::agent::AgentModel> {
    vec![
        row(
            "default",
            Some("claude-opus-5-5[1m]"),
            "Default (recommended)",
            Some(1_000_000),
        ),
        row(
            "opus[1m]",
            Some("claude-opus-5-5[1m]"),
            "Opus (1M context)",
            Some(1_000_000),
        ),
        row(
            "claude-fable-5-1[1m]",
            Some("claude-fable-5-1[1m]"),
            "Fable",
            None,
        ),
        row("sonnet", Some("claude-sonnet-5"), "Sonnet", Some(200_000)),
        row(
            "sonnet[1m]",
            Some("claude-sonnet-5[1m]"),
            "Sonnet (1M context)",
            Some(1_000_000),
        ),
        row(
            "haiku",
            Some("claude-haiku-4-5-20251001"),
            "Haiku",
            Some(200_000),
        ),
    ]
}

#[test]
fn the_cli_picker_becomes_models_without_the_budget_suffix() {
    let models = claude_agent_models(subscription_picker());

    // An alias never becomes the id, so an installed model keeps meaning the same
    // model when the CLI moves on; `[1m]` is a budget, not a model.
    assert_eq!(
        ids(&models),
        vec![
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-sonnet-5",
            "claude-haiku-4-5-20251001",
        ]
    );
    let names: Vec<&str> = models.iter().map(|model| model.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "Claude Opus 5.5",
            "Claude Fable 5.1",
            "Claude Sonnet 5",
            "Claude Haiku 4.5",
        ]
    );
    // Fable's window comes from the `[1m]` in the row's value.
    assert_eq!(
        windows(&models),
        vec![
            Some(1_000_000),
            Some(1_000_000),
            Some(1_000_000),
            Some(200_000)
        ]
    );
    for model in &models {
        assert!(
            model
                .capabilities
                .contains(&ModelCapability::ImageRecognition),
            "{} 缺少视觉输入",
            model.id
        );
        assert_eq!(model.group, "claude", "{} 的分组应由 id 推出", model.id);
    }
}

#[test]
fn a_model_listed_at_two_budgets_keeps_the_larger_one() {
    let models = claude_agent_models(api_key_picker());

    assert_eq!(
        ids(&models),
        vec![
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-sonnet-5",
            "claude-haiku-4-5-20251001",
        ]
    );
    // `sonnet` (200k) and `sonnet[1m]` (1M) are one model at the position of its
    // first row. The login can have 1M, and the window is what asks for it.
    assert_eq!(
        windows(&models),
        vec![
            Some(1_000_000),
            Some(1_000_000),
            Some(1_000_000),
            Some(200_000)
        ]
    );
}

#[test]
fn a_row_without_a_window_from_the_cli_keeps_its_place() {
    let models = claude_agent_models(vec![
        // An explicit id with no budget suffix that the catalog does not know:
        // nothing claims a window for it.
        row("claude-mythos-5-1", None, "Mythos", None),
        // An unresolved alias is kept as the CLI spells it, under the CLI's label.
        row("sonnet", None, "Sonnet", Some(1_000_000)),
    ]);
    assert_eq!(ids(&models), vec!["claude-mythos-5-1", "sonnet"]);
    assert_eq!(models[0].context_window, None);
    assert_eq!(models[0].name, "Claude Mythos 5.1");
    assert_eq!(models[1].name, "Sonnet");
    assert_eq!(models[1].context_window, Some(1_000_000));
}

#[test]
fn claude_model_names_are_derived_from_the_id() {
    let cases = [
        ("claude-opus-5-5", Some("Claude Opus 5.5")),
        ("claude-fable-5-1", Some("Claude Fable 5.1")),
        ("claude-sonnet-5", Some("Claude Sonnet 5")),
        ("claude-haiku-4-5-20251001", Some("Claude Haiku 4.5")),
        // Older ids put the version first; aliases and custom ids have no version.
        ("claude-3-5-sonnet-20241022", None),
        ("sonnet", None),
        ("claude-opus", None),
        ("claude-opus-latest", None),
        ("claude-opus-5[fast]", None),
    ];
    for (id, expected) in cases {
        assert_eq!(claude_model_name(id).as_deref(), expected, "{id}");
    }
}

#[test]
fn a_model_installed_with_the_budget_suffix_still_validates() {
    // Earlier versions installed ids such as `claude-opus-5[1m]`. They keep
    // working (the sidecar passes a suffixed id through), so persistence must
    // keep accepting brackets.
    crate::model::validate_model_id("claude-opus-5[1m]").unwrap();
}

#[test]
fn the_seed_rows_carry_their_table_limits_and_vision() {
    let models = claude_agent_seed_models(&provider(ProviderFamily::ClaudeAgent, String::new()));
    let limits = |id: &str| {
        let model = models
            .iter()
            .find(|model| model.id == id)
            .unwrap_or_else(|| panic!("种子缺少 {id}"));
        (model.context_window, model.max_output_tokens)
    };

    assert_eq!(models.len(), CLAUDE_AGENT_SEED_MODELS.len());
    assert_eq!(models[0].id, "claude-fable-5-1");
    assert_eq!(models[0].name, "Claude Fable 5.1");
    // The built-in preset's Opus and Sonnet roles are bound to these, so they have to ship.
    assert_eq!(limits("claude-opus-5-5"), (Some(1_000_000), Some(128_000)));
    assert_eq!(limits("claude-sonnet-5-5"), (Some(1_000_000), Some(128_000)));
    // The catalog lists a 1M window for `claude-opus-5`; the table says 200k and wins.
    assert_eq!(limits("claude-opus-5"), (Some(200_000), Some(128_000)));
    assert_eq!(limits("claude-sonnet-5"), (Some(1_000_000), Some(128_000)));
    assert_eq!(limits("claude-opus-4-1"), (Some(200_000), Some(32_000)));
    for model in &models {
        assert!(!model.id.contains("[1m]"), "{} 带了预算后缀", model.id);
        assert!(
            model
                .capabilities
                .contains(&ModelCapability::ImageRecognition),
            "{} 缺少视觉输入",
            model.id
        );
        assert_eq!(model.group, "claude", "{} 的分组应由 id 推出", model.id);
    }
}
