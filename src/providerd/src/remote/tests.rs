//! Offline tests for the remote backend against a local HTTPS mock of an
//! OpenAI-compatible server. Nothing here contacts a real server or uses a
//! real key: the mock's certificates come from a throwaway test CA, and the
//! only trust root the client gets is that CA.
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::config::{ApiKey, RemoteConfig, MAX_PROMPT_BYTES, MAX_RESPONSE_BYTES};
use super::net::AddressPolicy;
use super::Backend;

const SENTINEL: &str = "sk-flowerly-SENTINEL-0123456789abcdef";
const MODEL: &str = "gpt-oss:20b";
fn loopback() -> AddressPolicy {
    AddressPolicy {
        private_allowlist: Vec::new(),
        allow_loopback: true,
    }
}

// --- captured logs -------------------------------------------------------

static LOGS: Mutex<Vec<u8>> = Mutex::new(Vec::new());

struct LogSink;

impl std::io::Write for LogSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        LOGS.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every test logs at TRACE into one buffer, including reqwest, hyper and
/// rustls internals, so the secret checks cover the whole HTTP stack.
fn capture_logs() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(|| LogSink)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
    });
}

fn assert_no_secret(context: &str, text: &str) {
    assert!(
        !text.contains(SENTINEL),
        "{context} leaked the API key: {text}"
    );
    assert!(
        !text.contains("SENTINEL"),
        "{context} leaked part of the API key"
    );
}

// --- test PKI ------------------------------------------------------------

struct Ca {
    issuer: rcgen::CertifiedIssuer<'static, rcgen::KeyPair>,
}

impl Ca {
    fn new(name: &str) -> Self {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, name);
        let key = rcgen::KeyPair::generate().unwrap();
        Self {
            issuer: rcgen::CertifiedIssuer::self_signed(params, key).unwrap(),
        }
    }

    fn roots(&self) -> rustls::RootCertStore {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.issuer.der().clone()).unwrap();
        roots
    }

    fn server_config(&self, names: &[&str]) -> Arc<rustls::ServerConfig> {
        let params = rcgen::CertificateParams::new(
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.signed_by(&key, &self.issuer).unwrap();
        let chain: Vec<CertificateDer<'static>> = vec![cert.der().clone()];
        let private = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(chain, private)
            .unwrap(),
        )
    }
}

// --- mock OpenAI-compatible server ---------------------------------------

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

enum Reply {
    Json(u16, Value),
    Raw {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
        content_length: bool,
    },
    /// Accept the request and never answer.
    Hang,
}

type Behavior = Arc<dyn Fn(&Recorded) -> Reply + Send + Sync>;

struct Mock {
    addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl Mock {
    async fn start(tls: Arc<rustls::ServerConfig>, behavior: Behavior) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let acceptor = tokio_rustls::TlsAcceptor::from(tls);
        let (counted, recorded) = (Arc::clone(&connections), Arc::clone(&requests));
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                counted.fetch_add(1, Ordering::SeqCst);
                let (acceptor, behavior, recorded) = (
                    acceptor.clone(),
                    Arc::clone(&behavior),
                    Arc::clone(&recorded),
                );
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(stream).await else {
                        return;
                    };
                    let Some(request) = read_request(&mut tls).await else {
                        return;
                    };
                    recorded.lock().unwrap().push(request.clone());
                    match behavior(&request) {
                        Reply::Hang => tokio::time::sleep(Duration::from_secs(60)).await,
                        Reply::Json(status, value) => {
                            write_response(
                                &mut tls,
                                status,
                                &[],
                                value.to_string().as_bytes(),
                                true,
                            )
                            .await
                        }
                        Reply::Raw {
                            status,
                            headers,
                            body,
                            content_length,
                        } => {
                            write_response(&mut tls, status, &headers, &body, content_length).await
                        }
                    }
                });
            }
        });
        Self {
            addr,
            connections,
            requests,
        }
    }

    fn base(&self) -> String {
        format!("https://localhost:{}", self.addr.port())
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    fn chat_requests(&self) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.path.ends_with("/chat/completions"))
            .count()
    }
}

async fn read_request<S: AsyncReadExt + Unpin>(stream: &mut S) -> Option<Recorded> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let mut lines = head.lines();
    let mut start = lines.next()?.split_whitespace();
    let (method, path) = (start.next()?.to_owned(), start.next()?.to_owned());
    let mut length = 0usize;
    let mut authorization = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().ok()?,
                "authorization" => authorization = Some(value.trim().to_owned()),
                _ => {}
            }
        }
    }
    while buffer.len() < head_end + length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = String::from_utf8_lossy(&buffer[head_end..head_end + length]).to_string();
    Some(Recorded {
        method,
        path,
        authorization,
        body,
    })
}

async fn write_response<S: AsyncWriteExt + Unpin>(
    stream: &mut S,
    status: u16,
    headers: &[(&'static str, String)],
    body: &[u8],
    content_length: bool,
) {
    let mut head = format!(
        "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nConnection: close\r\n"
    );
    if content_length {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.shutdown().await;
}

fn models(ids: &[&str]) -> Value {
    serde_json::json!({
        "object": "list",
        "data": ids.iter().map(|id| serde_json::json!({"id": id, "object": "model"})).collect::<Vec<_>>(),
    })
}

fn completion(model: &str, content: Value, usage: Value) -> Value {
    serde_json::json!({
        "id": "chatcmpl-1", "object": "chat.completion", "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
        "usage": usage,
    })
}

fn usage(prompt: u64, completion: u64) -> Value {
    serde_json::json!({"prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion})
}

/// Discovery lists `listed`; chat answers with `chat`.
fn openai(
    listed: &'static [&'static str],
    chat: impl Fn(&Recorded) -> Reply + Send + Sync + 'static,
) -> Behavior {
    Arc::new(move |request: &Recorded| {
        if request.path.ends_with("/models") {
            Reply::Json(200, models(listed))
        } else {
            chat(request)
        }
    })
}

fn ready() -> Behavior {
    openai(&[MODEL, "other:7b"], |_| {
        Reply::Json(
            200,
            completion(MODEL, "flowerly-ready".into(), usage(12, 3)),
        )
    })
}

// --- backend harness -----------------------------------------------------

/// A config for `api_base`; `extra` lines override the test defaults.
fn config_text(api_base: &str, extra: &str) -> String {
    let mut text = format!(
        "[remote]\napi_base = \"{api_base}\"\napi_style = \"openai\"\nmodels = [\"{MODEL}\"]\n\
         credential = \"remote.key\"\n"
    );
    for (key, value) in [
        ("request_timeout_ms", "3000"),
        ("connect_timeout_ms", "1000"),
    ] {
        if !extra.contains(key) {
            text.push_str(&format!("{key} = {value}\n"));
        }
    }
    text + extra
}

fn backend_for(
    api_base: &str,
    extra: &str,
    roots: rustls::RootCertStore,
    policy: AddressPolicy,
) -> Arc<Backend> {
    capture_logs();
    let config = RemoteConfig::parse(&config_text(api_base, extra)).unwrap();
    Backend::new(
        config,
        ApiKey::new(SENTINEL.to_owned()).unwrap(),
        roots,
        policy,
        tokio::runtime::Handle::current(),
    )
    .unwrap()
}

/// One IPC command, run on a blocking thread as the real runner does.
async fn ipc(
    backend: &Arc<Backend>,
    name: &str,
    args: Value,
) -> Result<Value, (ErrorCode, String)> {
    let backend = Arc::clone(backend);
    let request = Envelope::new(Payload::Command {
        name: name.to_owned(),
        args,
    });
    let response = tokio::task::spawn_blocking(move || backend.handle(&request))
        .await
        .unwrap();
    let text = serde_json::to_string(&response).unwrap();
    assert_no_secret(name, &text);
    match response.payload {
        Payload::Ok { data } => Ok(data),
        Payload::Err { code, message } => Err((code.expect("typed error"), message)),
        other => panic!("unexpected payload {other:?}"),
    }
}

fn submit_args(request_id: &str, prompt: &str) -> Value {
    serde_json::json!({"request_id": request_id, "prompt": prompt, "max_tokens": 64, "created_by": "uid:1001"})
}

async fn submit(backend: &Arc<Backend>, request_id: &str, prompt: &str) -> Value {
    ipc(backend, "provider.submit", submit_args(request_id, prompt))
        .await
        .expect("submitted")
}

/// Poll until the job leaves `pending`.
async fn settle(backend: &Arc<Backend>, request_id: &str) -> Value {
    let args = serde_json::json!({"request_id": request_id, "created_by": "uid:1001"});
    for _ in 0..500 {
        let status = ipc(backend, "provider.poll", args.clone())
            .await
            .expect("poll");
        if status["state"] != "pending" {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("job {request_id} never settled");
}

async fn run(backend: &Arc<Backend>, request_id: &str, prompt: &str) -> Value {
    submit(backend, request_id, prompt).await;
    settle(backend, request_id).await
}

fn error_of(status: &Value) -> (&str, &str, &str) {
    let error = &status["error"];
    (
        error["code"].as_str().unwrap(),
        error["class"].as_str().unwrap(),
        error["outcome"].as_str().unwrap(),
    )
}

// --- tests ---------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_and_inference_succeed_with_reported_usage() {
    let ca = Ca::new("flowerly test CA");
    let mock = Mock::start(ca.server_config(&["localhost"]), ready()).await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());

    let discovered = ipc(&backend, "provider.discover", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(discovered["models_available"], serde_json::json!([MODEL]));
    assert_eq!(discovered["synthetic"], false);
    assert_eq!(discovered["destination"], "localhost");

    let status = run(&backend, "req-ok", "Reply with exactly: flowerly-ready").await;
    assert_eq!(status["state"], "succeeded", "{status}");
    let result = &status["result"];
    assert_eq!(result["text"], "flowerly-ready");
    assert_eq!(result["model_requested"], MODEL);
    assert_eq!(result["model_used"], MODEL);
    assert_eq!(
        result["usage"],
        serde_json::json!({"source": "provider_reported", "tokens_in": 12, "tokens_out": 3})
    );
    assert!(result["latency_ms"].as_u64().is_some());
    assert_eq!(result["truncated"], false);

    let requests = mock.requests();
    let chat = requests
        .iter()
        .find(|r| r.path == "/v1/chat/completions")
        .expect("chat request");
    assert_eq!(chat.method, "POST");
    assert_eq!(
        chat.authorization.as_deref(),
        Some(format!("Bearer {SENTINEL}").as_str())
    );
    let body: Value = serde_json::from_str(&chat.body).unwrap();
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["stream"], false);
    assert!(body.get("tools").is_none() && body.get("functions").is_none());
    assert!(requests
        .iter()
        .all(|r| r.path == "/v1/models" || r.path == "/v1/chat/completions"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_webui_style_uses_its_api_paths() {
    let ca = Ca::new("flowerly test CA");
    let mock = Mock::start(ca.server_config(&["localhost"]), ready()).await;
    capture_logs();
    let config =
        RemoteConfig::parse(&config_text(&mock.base(), "").replace("\"openai\"", "\"open-webui\""))
            .unwrap();
    let backend = Backend::new(
        config,
        ApiKey::new(SENTINEL.to_owned()).unwrap(),
        ca.roots(),
        loopback(),
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    assert_eq!(run(&backend, "req-webui", "hi").await["state"], "succeeded");
    let paths: Vec<String> = mock.requests().into_iter().map(|r| r.path).collect();
    assert_eq!(paths, ["/api/models", "/api/chat/completions"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_or_mismatched_model_fails_closed() {
    let ca = Ca::new("flowerly test CA");
    let mock = Mock::start(
        ca.server_config(&["localhost"]),
        openai(&["other:7b"], |_| {
            Reply::Json(200, completion(MODEL, "x".into(), Value::Null))
        }),
    )
    .await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let status = run(&backend, "req-missing", "hi").await;
    assert_eq!(status["state"], "failed");
    assert_eq!(
        error_of(&status),
        ("not_found", "model_unavailable", "not_sent")
    );
    assert_eq!(mock.chat_requests(), 0, "chat sent for an unlisted model");

    let mock = Mock::start(
        ca.server_config(&["localhost"]),
        openai(&[MODEL], |_| {
            Reply::Json(
                200,
                completion("gpt-oss:120b", "secret text".into(), usage(1, 1)),
            )
        }),
    )
    .await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let status = run(&backend, "req-mismatch", "hi").await;
    assert_eq!(error_of(&status), ("conflict", "model_mismatch", "sent"));
    assert!(
        !status.to_string().contains("secret text"),
        "mismatched text returned"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_credentials_open_the_circuit_without_retrying() {
    let ca = Ca::new("flowerly test CA");
    // 401 at discovery: nothing reaches the chat endpoint.
    let mock = Mock::start(
        ca.server_config(&["localhost"]),
        Arc::new(|_: &Recorded| {
            Reply::Json(
                401,
                serde_json::json!({"error": {"message": format!("bad key {SENTINEL}")}}),
            )
        }),
    )
    .await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let status = run(&backend, "req-401", "hi").await;
    assert_eq!(
        error_of(&status),
        ("permission_denied", "auth_failed", "not_sent")
    );
    let seen = mock.requests().len();
    let refused = ipc(&backend, "provider.submit", submit_args("req-401b", "hi"))
        .await
        .unwrap_err();
    assert_eq!(refused.0, ErrorCode::PermissionDenied);
    assert!(refused.1.contains("auth_failed"), "{}", refused.1);
    assert_eq!(mock.requests().len(), seen, "a rejected key was retried");

    // 403 at chat time.
    let mock = Mock::start(
        ca.server_config(&["localhost"]),
        openai(&[MODEL], |_| Reply::Json(403, serde_json::json!({}))),
    )
    .await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let status = run(&backend, "req-403", "hi").await;
    assert_eq!(
        error_of(&status),
        ("permission_denied", "auth_failed", "sent")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limits_and_server_errors_are_typed_and_never_retried() {
    let ca = Ca::new("flowerly test CA");
    let mock = Mock::start(
        ca.server_config(&["localhost"]),
        openai(&[MODEL], |_| Reply::Raw {
            status: 429,
            headers: vec![("Retry-After", "7".to_owned())],
            body: b"{}".to_vec(),
            content_length: true,
        }),
    )
    .await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let status = run(&backend, "req-429", "hi").await;
    assert_eq!(error_of(&status), ("unavailable", "rate_limited", "sent"));
    assert_eq!(status["error"]["retry_after_ms"], 7000);
    assert_eq!(mock.chat_requests(), 1);

    let mock = Mock::start(
        ca.server_config(&["localhost"]),
        openai(&[MODEL], |_| Reply::Json(502, serde_json::json!({}))),
    )
    .await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let status = run(&backend, "req-5xx", "hi").await;
    assert_eq!(status["state"], "unknown", "a 5xx may have executed");
    assert_eq!(
        error_of(&status),
        ("unavailable", "server_error", "unknown")
    );
    assert_eq!(mock.chat_requests(), 1, "an unknown outcome was retried");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_connections_and_timeouts_are_distinguished() {
    let ca = Ca::new("flowerly test CA");
    // Bind and drop: nothing listens on this port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let backend = backend_for(
        &format!("https://localhost:{port}"),
        "",
        ca.roots(),
        loopback(),
    );
    let status = run(&backend, "req-refused", "hi").await;
    assert_eq!(error_of(&status), ("unavailable", "connect", "not_sent"));

    let mock = Mock::start(
        ca.server_config(&["localhost"]),
        openai(&[MODEL], |_| Reply::Hang),
    )
    .await;
    let backend = backend_for(
        &mock.base(),
        "request_timeout_ms = 400\n",
        ca.roots(),
        loopback(),
    );
    let status = run(&backend, "req-timeout", "hi").await;
    assert_eq!(status["state"], "unknown");
    assert_eq!(error_of(&status), ("unavailable", "timeout", "unknown"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_failures_send_nothing_and_never_downgrade() {
    let trusted = Ca::new("flowerly trusted test CA");
    let untrusted = Ca::new("flowerly untrusted test CA");
    for (case, server) in [
        ("untrusted CA", untrusted.server_config(&["localhost"])),
        ("wrong hostname", trusted.server_config(&["other.example"])),
    ] {
        let mock = Mock::start(server, ready()).await;
        let backend = backend_for(&mock.base(), "", trusted.roots(), loopback());
        let status = run(&backend, "req-tls", "hi").await;
        assert_eq!(
            error_of(&status),
            ("unavailable", "tls", "not_sent"),
            "{case}"
        );
        assert!(
            mock.connections.load(Ordering::SeqCst) > 0,
            "{case}: never connected"
        );
        assert!(
            mock.requests().is_empty(),
            "{case}: HTTP request sent over a failed handshake"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirects_are_refused_and_never_followed() {
    let ca = Ca::new("flowerly test CA");
    let behavior: Behavior = Arc::new(|request: &Recorded| {
        if request.path == "/v1/models" {
            Reply::Json(200, models(&[MODEL]))
        } else {
            Reply::Raw {
                status: 307,
                headers: vec![("Location", "/v1/stolen".to_owned())],
                body: Vec::new(),
                content_length: true,
            }
        }
    });
    let mock = Mock::start(ca.server_config(&["localhost"]), behavior).await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let status = run(&backend, "req-redirect", "hi").await;
    assert_eq!(
        error_of(&status),
        ("permission_denied", "redirect_refused", "sent")
    );
    assert!(
        mock.requests().iter().all(|r| r.path != "/v1/stolen"),
        "redirect followed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_malformed_and_tool_call_responses_are_rejected() {
    let ca = Ca::new("flowerly test CA");
    let cases: Vec<(&str, Behavior, &str)> = vec![
        (
            "declared oversize",
            openai(&[MODEL], |_| Reply::Raw {
                status: 200,
                headers: vec![],
                body: vec![b' '; MAX_RESPONSE_BYTES + 1],
                content_length: true,
            }),
            "invalid_response",
        ),
        (
            "streamed oversize",
            openai(&[MODEL], |_| Reply::Raw {
                status: 200,
                headers: vec![],
                body: vec![b' '; MAX_RESPONSE_BYTES + 1],
                content_length: false,
            }),
            "invalid_response",
        ),
        (
            "malformed",
            openai(&[MODEL], |_| Reply::Raw {
                status: 200,
                headers: vec![],
                body: b"{\"model\": ".to_vec(),
                content_length: true,
            }),
            "invalid_response",
        ),
        (
            "tool calls",
            openai(&[MODEL], |_| {
                Reply::Json(
                    200,
                    serde_json::json!({
                        "model": MODEL,
                        "choices": [{"message": {"content": null, "tool_calls": [{"id": "t", "function": {"name": "shell"}}]}}],
                    }),
                )
            }),
            "unsupported_response",
        ),
    ];
    for (case, behavior, class) in cases {
        let mock = Mock::start(ca.server_config(&["localhost"]), behavior).await;
        let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
        let status = run(&backend, "req-bad", "hi").await;
        assert_eq!(status["state"], "failed", "{case}: {status}");
        assert_eq!(error_of(&status).1, class, "{case}");
        assert_eq!(error_of(&status).2, "sent", "{case}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_limits_are_enforced_before_any_network_use() {
    let ca = Ca::new("flowerly test CA");
    let mock = Mock::start(ca.server_config(&["localhost"]), ready()).await;
    let backend = backend_for(
        &mock.base(),
        "max_tokens_cap = 256\n",
        ca.roots(),
        loopback(),
    );
    for (case, args) in [
        (
            "prompt",
            serde_json::json!({"request_id": "r", "prompt": "x".repeat(MAX_PROMPT_BYTES + 1)}),
        ),
        (
            "max_tokens",
            serde_json::json!({"request_id": "r", "prompt": "hi", "max_tokens": 257}),
        ),
        (
            "zero tokens",
            serde_json::json!({"request_id": "r", "prompt": "hi", "max_tokens": 0}),
        ),
        (
            "model",
            serde_json::json!({"request_id": "r", "prompt": "hi", "model": "other:7b"}),
        ),
        (
            "request id",
            serde_json::json!({"request_id": "bad id", "prompt": "hi"}),
        ),
        (
            "generate",
            serde_json::json!({"request_id": "r", "prompt": "hi"}),
        ),
    ] {
        let name = if case == "generate" {
            "provider.generate"
        } else {
            "provider.submit"
        };
        let refused = ipc(&backend, name, args).await.unwrap_err();
        assert_eq!(
            refused.0,
            ErrorCode::InvalidArgument,
            "{case}: {}",
            refused.1
        );
    }
    assert_eq!(
        mock.connections.load(Ordering::SeqCst),
        0,
        "a refused request used the network"
    );

    // Long output is bounded and flagged, never silently cut.
    let long: Behavior = openai(&[MODEL], |_| {
        Reply::Json(
            200,
            completion(MODEL, "a".repeat(40_000).into(), usage(1, 4096)),
        )
    });
    let mock = Mock::start(ca.server_config(&["localhost"]), long).await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let status = run(&backend, "req-long", "hi").await;
    assert_eq!(status["result"]["truncated"], true);
    assert_eq!(
        status["result"]["text"].as_str().unwrap().len(),
        super::config::MAX_TEXT_BYTES
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_usage_is_recorded_as_unknown() {
    let ca = Ca::new("flowerly test CA");
    let mock = Mock::start(
        ca.server_config(&["localhost"]),
        openai(&[MODEL], |_| {
            Reply::Json(200, completion(MODEL, "ok".into(), Value::Null))
        }),
    )
    .await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let status = run(&backend, "req-nousage", "hi").await;
    assert_eq!(
        status["result"]["usage"],
        serde_json::json!({"source": "unknown", "tokens_in": null, "tokens_out": null})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_submissions_execute_once_and_reuse_conflicts() {
    let ca = Ca::new("flowerly test CA");
    let mock = Mock::start(ca.server_config(&["localhost"]), ready()).await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let first = run(&backend, "req-dup", "hi").await;
    assert_eq!(first["result"]["text"], "flowerly-ready");
    let again = ipc(&backend, "provider.submit", submit_args("req-dup", "hi"))
        .await
        .unwrap();
    assert_eq!(
        again["state"], "succeeded",
        "a duplicate submit did not return the retained job"
    );
    assert_eq!(again["result"]["usage"], first["result"]["usage"]);
    assert_eq!(mock.chat_requests(), 1, "a duplicate submit executed again");
    let conflict = ipc(
        &backend,
        "provider.submit",
        submit_args("req-dup", "different"),
    )
    .await
    .unwrap_err();
    assert_eq!(conflict.0, ErrorCode::Conflict);
    // Identities are per actor: another actor's request_id is independent.
    let mut other = submit_args("req-dup", "hi");
    other["created_by"] = "uid:1002".into();
    ipc(&backend, "provider.submit", other).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_after_send_reports_an_unknown_outcome() {
    let ca = Ca::new("flowerly test CA");
    let mock = Mock::start(
        ca.server_config(&["localhost"]),
        openai(&[MODEL], |_| Reply::Hang),
    )
    .await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    submit(&backend, "req-cancel", "hi").await;
    for _ in 0..200 {
        if mock.chat_requests() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(mock.chat_requests(), 1);
    let cancelled = ipc(
        &backend,
        "provider.cancel",
        serde_json::json!({"request_id": "req-cancel", "created_by": "uid:1001"}),
    )
    .await
    .unwrap();
    assert_eq!(cancelled["state"], "unknown");
    assert_eq!(
        error_of(&cancelled),
        ("unavailable", "cancelled", "unknown")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrency_is_bounded_without_queueing() {
    let ca = Ca::new("flowerly test CA");
    let mock = Mock::start(
        ca.server_config(&["localhost"]),
        openai(&[MODEL], |_| Reply::Hang),
    )
    .await;
    let backend = backend_for(
        &mock.base(),
        "max_concurrency = 1\n",
        ca.roots(),
        loopback(),
    );
    submit(&backend, "req-a", "hi").await;
    let refused = ipc(&backend, "provider.submit", submit_args("req-b", "hi"))
        .await
        .unwrap_err();
    assert_eq!(refused.0, ErrorCode::Unavailable);
    assert!(
        refused.1.contains("rate_limited") && refused.1.contains("not_sent"),
        "{}",
        refused.1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restarted_provider_reports_lost_jobs_as_unknown_and_recovers() {
    let ca = Ca::new("flowerly test CA");
    let failing = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = Arc::clone(&failing);
    let mock = Mock::start(
        ca.server_config(&["localhost"]),
        openai(&[MODEL], move |_| {
            if flag.load(Ordering::SeqCst) {
                Reply::Json(503, serde_json::json!({}))
            } else {
                Reply::Json(200, completion(MODEL, "back".into(), usage(1, 1)))
            }
        }),
    )
    .await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    assert_eq!(run(&backend, "req-outage", "hi").await["state"], "unknown");

    // A new process has no record of the old job: its outcome is unknown.
    drop(backend);
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let lost = ipc(
        &backend,
        "provider.poll",
        serde_json::json!({"request_id": "req-outage", "created_by": "uid:1001"}),
    )
    .await
    .unwrap_err();
    assert_eq!(lost.0, ErrorCode::NotFound);
    assert!(lost.1.contains("outcome is unknown"), "{}", lost.1);

    failing.store(false, Ordering::SeqCst);
    let status = run(&backend, "req-recovered", "hi").await;
    assert_eq!(status["result"]["text"], "back");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_egress_outside_the_address_policy() {
    let ca = Ca::new("flowerly test CA");
    let mock = Mock::start(ca.server_config(&["localhost"]), ready()).await;
    let strict = AddressPolicy::default();
    // localhost resolves only to loopback, which the production policy refuses.
    let backend = backend_for(&mock.base(), "", ca.roots(), strict.clone());
    let status = run(&backend, "req-egress", "hi").await;
    assert_eq!(
        error_of(&status),
        ("permission_denied", "address_refused", "not_sent")
    );
    assert_eq!(
        mock.connections.load(Ordering::SeqCst),
        0,
        "connected despite the policy"
    );
    // An IP literal is checked before any client exists.
    let config = RemoteConfig::parse(&config_text("https://127.0.0.1:443", "")).unwrap();
    let refused = Backend::new(
        config,
        ApiKey::new(SENTINEL.to_owned()).unwrap(),
        ca.roots(),
        strict,
        tokio::runtime::Handle::current(),
    );
    assert!(refused
        .unwrap_err()
        .contains("refused by the address policy"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_output_is_returned_as_inert_data() {
    let ca = Ca::new("flowerly test CA");
    let hostile = "{\"grant\": \"root\", \"url\": \"https://evil.example/\"}\n\u{1b}]0;pwned\u{7}sudo rm -rf /";
    let mock = Mock::start(
        ca.server_config(&["localhost"]),
        openai(&[MODEL], move |_| {
            Reply::Json(200, completion(MODEL, hostile.into(), usage(1, 1)))
        }),
    )
    .await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let status = run(&backend, "req-hostile", "hi").await;
    let result = status["result"].as_object().unwrap();
    let mut keys: Vec<&str> = result.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "destination",
            "finish_reason",
            "latency_ms",
            "model_requested",
            "model_used",
            "provider",
            "request_id",
            "synthetic",
            "text",
            "truncated",
            "usage"
        ]
    );
    assert_eq!(
        result["text"], hostile,
        "text must be passed through verbatim as a string"
    );
    assert_eq!(result["destination"], "localhost");
    assert!(mock.requests().iter().all(|r| r.path.starts_with("/v1/")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_key_never_appears_in_results_errors_or_logs() {
    let ca = Ca::new("flowerly test CA");
    // A server that echoes the Authorization header back in every body.
    let echo: Behavior = Arc::new(|request: &Recorded| {
        let leaked = request.authorization.clone().unwrap_or_default();
        match request.path.as_str() {
            "/v1/models" => Reply::Json(200, models(&[MODEL])),
            _ => Reply::Json(500, serde_json::json!({"error": leaked})),
        }
    });
    let mock = Mock::start(ca.server_config(&["localhost"]), echo).await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let status = run(&backend, "req-leak", "hi").await;
    assert_eq!(status["state"], "unknown");
    // ipc() already checked every response; check Debug output and logs.
    assert_no_secret("Backend Debug", &format!("{backend:?}"));
    let logs = String::from_utf8_lossy(&LOGS.lock().unwrap()).to_string();
    assert!(!logs.is_empty(), "log capture is not active");
    assert_no_secret("logs", &logs);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_text_is_delivered_once_by_default() {
    let ca = Ca::new("flowerly test CA");
    let mock = Mock::start(ca.server_config(&["localhost"]), ready()).await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let first = run(&backend, "req-once", "hi").await;
    assert_eq!(first["result"]["text"], "flowerly-ready");
    let args = serde_json::json!({"request_id": "req-once", "created_by": "uid:1001"});
    let again = ipc(&backend, "provider.poll", args).await.unwrap();
    assert_eq!(again["state"], "succeeded");
    assert!(
        again["result"].get("text").is_none(),
        "text retained after delivery: {again}"
    );
    assert_eq!(again["result"]["text_released"], true);
    // Metadata survives the release; nothing was re-executed.
    assert_eq!(again["result"]["usage"]["tokens_out"], 3);
    assert_eq!(again["result"]["model_used"], MODEL);
    assert_eq!(mock.chat_requests(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_opt_in_replay_window_redelivers_until_it_expires() {
    let ca = Ca::new("flowerly test CA");
    let mock = Mock::start(ca.server_config(&["localhost"]), ready()).await;
    let backend = backend_for(&mock.base(), "", ca.roots(), loopback());
    let too_long = serde_json::json!({"request_id": "r", "prompt": "hi", "replay_seconds": 901});
    assert_eq!(
        ipc(&backend, "provider.submit", too_long)
            .await
            .unwrap_err()
            .0,
        ErrorCode::InvalidArgument
    );

    let mut args = submit_args("req-replay", "hi");
    args["replay_seconds"] = 1.into();
    ipc(&backend, "provider.submit", args).await.unwrap();
    let first = settle(&backend, "req-replay").await;
    assert_eq!(first["result"]["text"], "flowerly-ready");
    let poll = serde_json::json!({"request_id": "req-replay", "created_by": "uid:1001"});
    let within = ipc(&backend, "provider.poll", poll.clone()).await.unwrap();
    assert_eq!(
        within["result"]["text"], "flowerly-ready",
        "not replayed inside the window"
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let expired = ipc(&backend, "provider.poll", poll).await.unwrap();
    assert!(
        expired["result"].get("text").is_none(),
        "replayed after the window: {expired}"
    );
    assert_eq!(expired["result"]["text_released"], true);
    assert_eq!(mock.chat_requests(), 1, "replay re-executed");
}
