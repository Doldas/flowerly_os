//! Opt-in OpenAI-compatible remote backend (ADR-0022, **proposed**).
//!
//! Contract claims:
//! - Egress happens only here, only to the administrator-configured origin,
//!   over verified TLS, without redirects or proxies (see `net`).
//! - Asynchronous: `provider.submit` returns at once; `provider.poll`
//!   reports `pending`, `succeeded`, `failed` (with `not_sent` / `sent`) or
//!   `unknown`. A request whose outcome is unknown (timeout after send,
//!   server error, cancellation after send) is never retried here, because
//!   the server may already have executed and billed it.
//! - Idempotent per `(created_by, request_id)` while the job is retained;
//!   the same identity with different arguments is a `Conflict`.
//! - Bounded prompt, tokens, response body, text, concurrency and retained
//!   jobs. Usage is reported only when the server supplies it.
//! - The API key never appears in a response, an error or a log line;
//!   server error bodies are discarded unread.
//! - The generated text is data. It grants nothing and selects nothing.
//!
//! flowerlyd cannot reach this backend until ADR-0022 is accepted and its
//! gates are wired (#263): `provider.register` still refuses remote kinds.
pub mod config;
pub mod net;
pub mod openai;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::{coded_error_envelope, ok_envelope};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use config::{ApiKey, RemoteConfig, MAX_PROMPT_BYTES, MAX_RESPONSE_BYTES};
use net::AddressPolicy;
use openai::ChatRejection;

const MAX_IN_FLIGHT_PER_ACTOR: usize = 4;
const MAX_RETAINED_JOBS: usize = 256;
const COMPLETED_JOB_TTL: Duration = Duration::from_secs(15 * 60);
const DISCOVERY_TTL: Duration = Duration::from_secs(5 * 60);
/// Live discovery inside the synchronous IPC handler must answer within the
/// 10 s IPC request deadline.
const DISCOVER_COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);

/// Whether the provider may have executed (and billed) the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing reached the server's chat endpoint: safe to retry.
    NotSent,
    /// The server answered; it did or did not execute, and we know which.
    Sent,
    /// The request may have executed; never retried automatically.
    Unknown,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotSent => "not_sent",
            Self::Sent => "sent",
            Self::Unknown => "unknown",
        }
    }
}

/// A typed failure. `message` is built only from static text, status codes
/// and the configured host: never from headers, bodies or the key.
#[derive(Debug, Clone)]
pub struct Failure {
    pub code: ErrorCode,
    pub class: &'static str,
    pub outcome: Outcome,
    pub message: String,
    pub retry_after_ms: Option<u64>,
}

impl Failure {
    fn new(
        code: ErrorCode,
        class: &'static str,
        outcome: Outcome,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            class,
            outcome,
            message: message.into(),
            retry_after_ms: None,
        }
    }

    fn to_json(&self) -> Value {
        serde_json::json!({
            "code": self.code,
            "class": self.class,
            "outcome": self.outcome.as_str(),
            "message": self.message,
            "retry_after_ms": self.retry_after_ms,
        })
    }
}

enum JobStatus {
    Pending,
    Done(Result<Value, Failure>),
}

struct Job {
    digest: [u8; 32],
    created: Instant,
    finished: Option<Instant>,
    /// Set immediately before the chat request is handed to the network.
    sent: Arc<AtomicBool>,
    status: JobStatus,
    abort: Option<tokio::task::AbortHandle>,
}

type JobKey = (String, String);

pub struct Backend {
    config: RemoteConfig,
    key: ApiKey,
    client: reqwest::Client,
    runtime: tokio::runtime::Handle,
    jobs: Mutex<HashMap<JobKey, Job>>,
    permits: Arc<Semaphore>,
    /// 401/403 opens the circuit until restart: a rejected key is not
    /// retried against the server.
    auth_failed: AtomicBool,
    discovery: Mutex<Option<(Instant, Vec<String>)>>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Backend")
            .field("config", &self.config)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Backend {
    /// Production construction: system trust roots, key from the systemd
    /// credentials directory, loopback always refused.
    pub fn from_config_file(path: &std::path::Path) -> Result<Arc<Self>, String> {
        let config = RemoteConfig::load(path)?;
        let key = ApiKey::from_credentials_dir(&config.credential)?;
        let roots = net::system_roots()?;
        let policy = AddressPolicy {
            allow_private: config.allow_private_addresses,
            allow_loopback: false,
        };
        Self::new(
            config,
            key,
            roots,
            policy,
            tokio::runtime::Handle::current(),
        )
    }

    pub fn new(
        config: RemoteConfig,
        key: ApiKey,
        roots: rustls::RootCertStore,
        policy: AddressPolicy,
        runtime: tokio::runtime::Handle,
    ) -> Result<Arc<Self>, String> {
        let client = net::build_client(&config, roots, policy)?;
        Ok(Arc::new(Self {
            permits: Arc::new(Semaphore::new(config.max_concurrency as usize)),
            config,
            key,
            client,
            runtime,
            jobs: Mutex::new(HashMap::new()),
            auth_failed: AtomicBool::new(false),
            discovery: Mutex::new(None),
        }))
    }

    pub fn self_describe(&self) -> Value {
        serde_json::json!({
            "provider": "openai-compatible",
            "kind": "remote-https",
            "synthetic": false,
            "destination": self.config.host,
            "api_style": self.config.api_style.as_str(),
            "models_allowlisted": self.config.models,
            "streaming": false,
            "asynchronous": true,
            "commands": ["provider.discover", "provider.submit", "provider.poll", "provider.cancel"],
            "max_prompt_bytes": MAX_PROMPT_BYTES,
            "max_tokens_per_request": self.config.max_tokens_cap,
            "max_response_bytes": MAX_RESPONSE_BYTES,
            "max_text_bytes": config::MAX_TEXT_BYTES,
            "max_concurrency": self.config.max_concurrency,
            "max_in_flight_per_actor": MAX_IN_FLIGHT_PER_ACTOR,
            "request_timeout_ms": self.config.request_timeout.as_millis() as u64,
            "promise": "opt-in-remote (ADR-0022 proposed; not reachable from flowerlyd)",
        })
    }

    /// The IPC handler. Runs on a blocking worker thread (never inside the
    /// async runtime), so `provider.discover` may block on its bounded call.
    pub fn handle(self: &Arc<Self>, request: &Envelope) -> Envelope {
        let Payload::Command { name, args } = &request.payload else {
            return coded_error_envelope(
                request.id.clone(),
                ErrorCode::InvalidArgument,
                "command required",
            );
        };
        let result = match name.as_str() {
            "provider.discover" => self.discover_command(),
            "provider.submit" => self.submit(args),
            "provider.poll" => self.poll(args),
            "provider.cancel" => self.cancel(args),
            "provider.generate" => Err(Failure::new(
                ErrorCode::InvalidArgument,
                "asynchronous_only",
                Outcome::NotSent,
                "the remote backend is asynchronous: use provider.submit and provider.poll",
            )),
            other => Err(Failure::new(
                ErrorCode::InvalidArgument,
                "unknown_command",
                Outcome::NotSent,
                format!(
                    "providerd: unknown command `{}`",
                    other.chars().take(64).collect::<String>()
                ),
            )),
        };
        match result {
            Ok(data) => ok_envelope(request.id.clone(), data),
            Err(failure) => coded_error_envelope(
                request.id.clone(),
                failure.code,
                format!(
                    "{} [{}; outcome {}]",
                    failure.message,
                    failure.class,
                    failure.outcome.as_str()
                ),
            ),
        }
    }

    fn discover_command(self: &Arc<Self>) -> Result<Value, Failure> {
        let this = Arc::clone(self);
        let live = self.runtime.block_on(async move {
            tokio::time::timeout(DISCOVER_COMMAND_TIMEOUT, this.discover(true)).await
        });
        let models = match live {
            Ok(result) => result?,
            Err(_) => {
                return Err(Failure::new(
                    ErrorCode::Unavailable,
                    "timeout",
                    Outcome::NotSent,
                    format!(
                        "model discovery at {} exceeded {} ms",
                        self.config.host,
                        DISCOVER_COMMAND_TIMEOUT.as_millis()
                    ),
                ))
            }
        };
        let mut described = self.self_describe();
        described["models_available"] = serde_json::json!(self
            .config
            .models
            .iter()
            .filter(|model| models.contains(model))
            .collect::<Vec<_>>());
        Ok(described)
    }

    fn submit(self: &Arc<Self>, args: &Value) -> Result<Value, Failure> {
        let actor = actor(args);
        let request_id = request_id(args)?;
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing string arg `prompt`"))?;
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(invalid(format!(
                "`prompt` exceeds {MAX_PROMPT_BYTES} bytes"
            )));
        }
        let cap = self.config.max_tokens_cap;
        let max_tokens = match args.get("max_tokens") {
            None | Some(Value::Null) => 128.min(cap),
            Some(value) => value
                .as_u64()
                .filter(|raw| (1..=cap).contains(raw))
                .ok_or_else(|| invalid(format!("`max_tokens` must be 1..={cap}")))?,
        };
        let model = match args.get("model") {
            None | Some(Value::Null) => self.config.models[0].clone(),
            Some(Value::String(model)) if self.config.models.contains(model) => model.clone(),
            Some(_) => return Err(invalid("`model` is not in the administrator's allowlist")),
        };
        let digest: [u8; 32] = Sha256::digest(
            serde_json::to_vec(&serde_json::json!([model, prompt, max_tokens])).expect("json"),
        )
        .into();

        let mut jobs = self.jobs.lock().map_err(|_| poisoned())?;
        let key = (actor.clone(), request_id.clone());
        if let Some(job) = jobs.get(&key) {
            if job.digest != digest {
                return Err(Failure::new(
                    ErrorCode::Conflict,
                    "request_id_reused",
                    Outcome::NotSent,
                    "request_id was already used with different arguments",
                ));
            }
            return Ok(status_json(&request_id, job));
        }
        if self.auth_failed.load(Ordering::SeqCst) {
            return Err(Failure::new(
                ErrorCode::PermissionDenied,
                "auth_failed",
                Outcome::NotSent,
                format!(
                    "{} rejected the credential earlier; restart after fixing it",
                    self.config.host
                ),
            ));
        }
        prune(&mut jobs);
        let pending_for_actor = jobs
            .iter()
            .filter(|((owner, _), job)| owner == &actor && matches!(job.status, JobStatus::Pending))
            .count();
        if pending_for_actor >= MAX_IN_FLIGHT_PER_ACTOR || jobs.len() >= MAX_RETAINED_JOBS {
            return Err(Failure::new(
                ErrorCode::Unavailable,
                "rate_limited",
                Outcome::NotSent,
                "provider request limit reached; retry with bounded backoff",
            ));
        }
        let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() else {
            return Err(Failure::new(
                ErrorCode::Unavailable,
                "rate_limited",
                Outcome::NotSent,
                "provider concurrency limit reached; retry with bounded backoff",
            ));
        };
        let sent = Arc::new(AtomicBool::new(false));
        let this = Arc::clone(self);
        let task_key = key.clone();
        let task_sent = Arc::clone(&sent);
        let prompt = prompt.to_owned();
        let task = self.runtime.spawn(async move {
            let _permit = permit;
            let outcome = this
                .execute(&task_key.1, &model, &prompt, max_tokens, &task_sent)
                .await;
            this.finish(&task_key, outcome);
        });
        jobs.insert(
            key,
            Job {
                digest,
                created: Instant::now(),
                finished: None,
                sent,
                status: JobStatus::Pending,
                abort: Some(task.abort_handle()),
            },
        );
        Ok(serde_json::json!({"request_id": request_id, "state": "pending", "sent": false}))
    }

    fn poll(&self, args: &Value) -> Result<Value, Failure> {
        let key = (actor(args), request_id(args)?);
        let jobs = self.jobs.lock().map_err(|_| poisoned())?;
        match jobs.get(&key) {
            Some(job) => Ok(status_json(&key.1, job)),
            None => Err(unknown_request()),
        }
    }

    fn cancel(&self, args: &Value) -> Result<Value, Failure> {
        let key = (actor(args), request_id(args)?);
        let mut jobs = self.jobs.lock().map_err(|_| poisoned())?;
        let job = jobs.get_mut(&key).ok_or_else(unknown_request)?;
        if matches!(job.status, JobStatus::Pending) {
            if let Some(abort) = job.abort.take() {
                abort.abort();
            }
            let outcome = if job.sent.load(Ordering::SeqCst) {
                Outcome::Unknown
            } else {
                Outcome::NotSent
            };
            job.status = JobStatus::Done(Err(Failure::new(
                ErrorCode::Unavailable,
                "cancelled",
                outcome,
                "cancelled by the caller",
            )));
            job.finished = Some(Instant::now());
        }
        Ok(status_json(&key.1, job))
    }

    fn finish(&self, key: &JobKey, outcome: Result<Value, Failure>) {
        match &outcome {
            Ok(_) => {
                tracing::info!(request_id = %key.1, destination = %self.config.host, "remote inference succeeded")
            }
            Err(failure) => tracing::warn!(
                request_id = %key.1,
                destination = %self.config.host,
                class = failure.class,
                outcome = failure.outcome.as_str(),
                "remote inference failed"
            ),
        }
        if let Ok(mut jobs) = self.jobs.lock() {
            if let Some(job) = jobs.get_mut(key) {
                // A cancellation recorded first wins.
                if matches!(job.status, JobStatus::Pending) {
                    job.status = JobStatus::Done(outcome);
                    job.finished = Some(Instant::now());
                    job.abort = None;
                }
            }
        }
    }

    /// Models the server lists, cached for `DISCOVERY_TTL`.
    async fn discover(&self, refresh: bool) -> Result<Vec<String>, Failure> {
        if !refresh {
            if let Ok(cache) = self.discovery.lock() {
                if let Some((at, models)) = cache.as_ref() {
                    if at.elapsed() < DISCOVERY_TTL {
                        return Ok(models.clone());
                    }
                }
            }
        }
        if self.auth_failed.load(Ordering::SeqCst) {
            return Err(Failure::new(
                ErrorCode::PermissionDenied,
                "auth_failed",
                Outcome::NotSent,
                format!(
                    "{} rejected the credential earlier; restart after fixing it",
                    self.config.host
                ),
            ));
        }
        let request = self
            .client
            .get(self.config.url(self.config.api_style.models_path()))
            .header(reqwest::header::AUTHORIZATION, self.bearer())
            .header(reqwest::header::ACCEPT, "application/json");
        // A discovery failure means the chat request was never sent.
        let body = self
            .exchange(request)
            .await
            .map(|(body, _)| body)
            .map_err(|mut failure| {
                failure.outcome = Outcome::NotSent;
                failure
            })?;
        let models = openai::parse_models(&body).map_err(|message| {
            Failure::new(
                ErrorCode::Internal,
                "invalid_response",
                Outcome::NotSent,
                message,
            )
        })?;
        if let Ok(mut cache) = self.discovery.lock() {
            *cache = Some((Instant::now(), models.clone()));
        }
        Ok(models)
    }

    async fn execute(
        &self,
        request_id: &str,
        model: &str,
        prompt: &str,
        max_tokens: u64,
        sent: &AtomicBool,
    ) -> Result<Value, Failure> {
        let available = self.discover(false).await?;
        if !available.iter().any(|listed| listed == model) {
            return Err(Failure::new(
                ErrorCode::NotFound,
                "model_unavailable",
                Outcome::NotSent,
                format!("model `{model}` is not listed by {}", self.config.host),
            ));
        }
        let request = self
            .client
            .post(self.config.url(self.config.api_style.chat_path()))
            .header(reqwest::header::AUTHORIZATION, self.bearer())
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(openai::chat_body(model, prompt, max_tokens).to_string());
        sent.store(true, Ordering::SeqCst);
        let (body, latency) = match self.exchange(request).await {
            Ok(exchanged) => exchanged,
            Err(failure) => {
                if failure.class == "model_unavailable" {
                    if let Ok(mut cache) = self.discovery.lock() {
                        *cache = None;
                    }
                }
                return Err(failure);
            }
        };
        let completion = openai::parse_chat(&body, model).map_err(|rejection| match rejection {
            ChatRejection::Invalid(message) => Failure::new(
                ErrorCode::Internal,
                "invalid_response",
                Outcome::Sent,
                message,
            ),
            ChatRejection::Unsupported(message) => Failure::new(
                ErrorCode::Internal,
                "unsupported_response",
                Outcome::Sent,
                message,
            ),
            ChatRejection::ModelMismatch { reported } => Failure::new(
                ErrorCode::Conflict,
                "model_mismatch",
                Outcome::Sent,
                format!("requested `{model}` but the server reported `{reported}`; text withheld"),
            ),
            ChatRejection::Empty => Failure::new(
                ErrorCode::Internal,
                "empty_output",
                Outcome::Sent,
                "the model returned no text (raise max_tokens for reasoning models)",
            ),
        })?;
        let (source, tokens_in, tokens_out) = match completion.usage {
            Some(usage) => (
                "provider_reported",
                Some(usage.prompt_tokens),
                Some(usage.completion_tokens),
            ),
            None => ("unknown", None, None),
        };
        Ok(serde_json::json!({
            "request_id": request_id,
            "provider": "openai-compatible",
            "destination": self.config.host,
            "synthetic": false,
            "model_requested": model,
            "model_used": completion.model,
            "text": completion.text,
            "truncated": completion.truncated,
            "finish_reason": completion.finish_reason,
            "usage": {"source": source, "tokens_in": tokens_in, "tokens_out": tokens_out},
            "latency_ms": latency.as_millis() as u64,
        }))
    }

    fn bearer(&self) -> reqwest::header::HeaderValue {
        let mut value =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.key.expose()))
                .expect("ApiKey::new admits only visible ASCII");
        value.set_sensitive(true);
        value
    }

    /// One bounded HTTP exchange: status mapping, then a body read capped at
    /// `MAX_RESPONSE_BYTES`. Latency covers send through the last byte.
    async fn exchange(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<(Value, Duration), Failure> {
        let host = &self.config.host;
        let started = Instant::now();
        let mut response = request
            .send()
            .await
            .map_err(|error| transport_failure(&error, host))?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.status_failure(&response, status));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(oversized());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            let mut failure = transport_failure(&error, host);
            // The server answered 2xx: it executed, and the result was lost.
            failure.outcome = Outcome::Unknown;
            failure
        })? {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(oversized());
            }
            body.extend_from_slice(&chunk);
        }
        let latency = started.elapsed();
        let value = serde_json::from_slice(&body).map_err(|_| {
            Failure::new(
                ErrorCode::Internal,
                "invalid_response",
                Outcome::Sent,
                "response is not valid JSON",
            )
        })?;
        Ok((value, latency))
    }

    fn status_failure(&self, response: &reqwest::Response, status: reqwest::StatusCode) -> Failure {
        let host = &self.config.host;
        let code = status.as_u16();
        match code {
            300..=399 => Failure::new(
                ErrorCode::PermissionDenied,
                "redirect_refused",
                Outcome::Sent,
                format!("{host} answered HTTP {code}; redirects are never followed"),
            ),
            401 | 403 => {
                self.auth_failed.store(true, Ordering::SeqCst);
                Failure::new(
                    ErrorCode::PermissionDenied,
                    "auth_failed",
                    Outcome::Sent,
                    format!("{host} rejected the credential (HTTP {code})"),
                )
            }
            404 => Failure::new(
                ErrorCode::NotFound,
                "model_unavailable",
                Outcome::Sent,
                format!("{host} answered HTTP 404"),
            ),
            429 => {
                let mut failure = Failure::new(
                    ErrorCode::Unavailable,
                    "rate_limited",
                    Outcome::Sent,
                    format!("{host} is rate limiting (HTTP 429)"),
                );
                failure.retry_after_ms = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse::<u64>().ok())
                    .map(|seconds| {
                        Duration::from_secs(seconds)
                            .min(MAX_RETRY_AFTER)
                            .as_millis() as u64
                    });
                failure
            }
            408 | 500..=599 => Failure::new(
                ErrorCode::Unavailable,
                "server_error",
                Outcome::Unknown,
                format!("{host} answered HTTP {code}; the request may have executed"),
            ),
            _ => Failure::new(
                ErrorCode::InvalidArgument,
                "request_rejected",
                Outcome::Sent,
                format!("{host} rejected the request (HTTP {code})"),
            ),
        }
    }
}

fn transport_failure(error: &reqwest::Error, host: &str) -> Failure {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    let (mut refused, mut tls) = (false, false);
    while let Some(current) = source {
        refused |= current.is::<net::AddressRefused>();
        tls |= current.is::<rustls::Error>();
        // hyper-rustls nests rustls errors inside io::Errors, and
        // `io::Error::source()` skips the wrapped error itself: descend
        // through `get_ref` instead.
        source = match current.downcast_ref::<std::io::Error>() {
            Some(io) => io
                .get_ref()
                .map(|inner| inner as &(dyn std::error::Error + 'static)),
            None => current.source(),
        };
    }
    if refused {
        return Failure::new(
            ErrorCode::PermissionDenied,
            "address_refused",
            Outcome::NotSent,
            format!("every address of {host} is refused by the address policy"),
        );
    }
    if tls {
        return Failure::new(
            ErrorCode::Unavailable,
            "tls",
            Outcome::NotSent,
            format!("TLS verification with {host} failed; nothing was sent"),
        );
    }
    if error.is_connect() {
        let class = if error.is_timeout() {
            "connect_timeout"
        } else {
            "connect"
        };
        return Failure::new(
            ErrorCode::Unavailable,
            class,
            Outcome::NotSent,
            format!("could not connect to {host}; nothing was sent"),
        );
    }
    if error.is_timeout() {
        return Failure::new(
            ErrorCode::Unavailable,
            "timeout",
            Outcome::Unknown,
            format!("{host} did not answer in time; the request may have executed"),
        );
    }
    Failure::new(
        ErrorCode::Unavailable,
        "transport",
        Outcome::Unknown,
        format!("the exchange with {host} failed after sending; the request may have executed"),
    )
}

fn oversized() -> Failure {
    Failure::new(
        ErrorCode::Internal,
        "invalid_response",
        Outcome::Sent,
        format!("response exceeds {MAX_RESPONSE_BYTES} bytes"),
    )
}

fn invalid(message: impl Into<String>) -> Failure {
    Failure::new(
        ErrorCode::InvalidArgument,
        "invalid_argument",
        Outcome::NotSent,
        message,
    )
}

fn poisoned() -> Failure {
    Failure::new(
        ErrorCode::Internal,
        "internal",
        Outcome::NotSent,
        "provider job table poisoned",
    )
}

fn unknown_request() -> Failure {
    Failure::new(
        ErrorCode::NotFound,
        "unknown_request",
        Outcome::Unknown,
        "no retained job for this request_id (never submitted, expired, or lost in a provider restart); its outcome is unknown",
    )
}

fn actor(args: &Value) -> String {
    args.get("created_by")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .chars()
        .take(128)
        .collect()
}

fn request_id(args: &Value) -> Result<String, Failure> {
    args.get("request_id")
        .and_then(Value::as_str)
        .filter(|raw| {
            !raw.is_empty()
                && raw.len() <= 128
                && raw
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
        })
        .map(str::to_owned)
        .ok_or_else(|| invalid("missing or malformed `request_id` (bounded idempotency key rules)"))
}

fn prune(jobs: &mut HashMap<JobKey, Job>) {
    jobs.retain(|_, job| {
        job.finished
            .is_none_or(|finished| finished.elapsed() < COMPLETED_JOB_TTL)
            && job.created.elapsed() < COMPLETED_JOB_TTL + config::MAX_REQUEST_TIMEOUT
    });
}

fn status_json(request_id: &str, job: &Job) -> Value {
    match &job.status {
        JobStatus::Pending => serde_json::json!({
            "request_id": request_id,
            "state": "pending",
            "sent": job.sent.load(Ordering::SeqCst),
        }),
        JobStatus::Done(Ok(result)) => serde_json::json!({
            "request_id": request_id,
            "state": "succeeded",
            "result": result,
        }),
        JobStatus::Done(Err(failure)) => serde_json::json!({
            "request_id": request_id,
            "state": if failure.outcome == Outcome::Unknown { "unknown" } else { "failed" },
            "error": failure.to_json(),
        }),
    }
}
