//! Bounded synthetic echo generation for the reference provider.
//!
//! Honesty invariants:
//! - Latency is *measured* wall time, never fabricated.
//! - Token counts are word-derived and bounded; a truncated result carries
//!   the same bounds as a first-class one (no hidden inflation).
//! - No durable state is written; nothing executes beyond string slicing.
//! - `FLOWERLY_PROVIDER_OUTAGE=1` deterministically fails every generate
//!   request (fail-closed outage seam for tests that must exercise provider
//!   outage/fallback with explicit consent).

use flowerly_core::ipc::ErrorCode;
use serde_json::Value;

/// Hard caps for the synthetic echo provider.
pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
pub const MAX_TOKENS_OUT: usize = 4096;

#[derive(Debug)]
pub(crate) struct GenerateFailure {
    pub code: ErrorCode,
    pub message: String,
}

impl From<&str> for GenerateFailure {
    fn from(message: &str) -> Self {
        Self {
            code: ErrorCode::InvalidArgument,
            message: message.to_owned(),
        }
    }
}

/// Count whitespace-separated pseudo tokens (deterministic, no NLP claims).
fn approx_tokens(text: &str) -> usize {
    text.split_whitespace().count()
}

fn truncate_to_token_budget(prompt: &str, tokens: usize) -> String {
    prompt
        .split_whitespace()
        .take(tokens)
        .collect::<Vec<_>>()
        .join(" ")
}

fn outage_forced() -> bool {
    std::env::var("FLOWERLY_PROVIDER_OUTAGE")
        .map(|value| value == "1")
        .unwrap_or(false)
}

pub(crate) fn generate(args: &Value) -> Result<serde_json::Value, GenerateFailure> {
    if outage_forced() {
        return Err(GenerateFailure {
            code: ErrorCode::Unavailable,
            message: "reference provider forced outage (outage seam active)".to_owned(),
        });
    }
    let request_id = args
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|raw| {
            !raw.is_empty()
                && raw.len() <= 128
                && raw
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
        })
        .ok_or_else(|| {
            GenerateFailure::from(
                "missing or malformed `request_id` (bounded idempotency key rules)",
            )
        })?
        .to_owned();
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .ok_or("missing string arg `prompt`")?;
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err(GenerateFailure {
            code: ErrorCode::InvalidArgument,
            message: format!("`prompt` exceeds {MAX_PROMPT_BYTES} bytes"),
        });
    }
    let max_tokens = match args.get("max_tokens") {
        None | Some(Value::Null) => 128usize,
        Some(value) => value
            .as_u64()
            .filter(|raw| *raw > 0 && *raw <= MAX_TOKENS_OUT as u64)
            .map(|raw| raw as usize)
            .ok_or(GenerateFailure {
                code: ErrorCode::InvalidArgument,
                message: format!("`max_tokens` must be 1..={MAX_TOKENS_OUT}"),
            })?,
    };
    let started = std::time::Instant::now();
    let tokens_in = approx_tokens(prompt);
    let echo = truncate_to_token_budget(&format!("echo: {prompt}"), max_tokens);
    let tokens_out = approx_tokens(&echo);
    let latency = started.elapsed();
    Ok(serde_json::json!({
        "request_id": request_id,
        "provider": "echo",
        "model_used": "echo-synthetic-1",
        "synthetic": true,
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
        "latency_ms": latency.as_millis() as u64,
        "text": echo,
        "bounded": true,
        "confidence": "reference-contract-only",
    }))
}

pub(crate) fn self_test() -> Result<(), String> {
    let result = generate(
        &serde_json::json!({"prompt": "alpha beta gamma", "max_tokens": 4,
        "request_id": "probe-1"}),
    )
    .map_err(|error| error.message)?;
    if result["tokens_out"].as_u64() != Some(4) {
        return Err("echo token bound violated in self-test".to_owned());
    }
    if result["request_id"] != "probe-1" {
        return Err("request_id echo violated in self-test".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_echo_truncates_and_measures() {
        let result = generate(&serde_json::json!({
            "prompt": "one two three four five six",
            "max_tokens": 3,
            "request_id": "req-1",
        }))
        .expect("bounded generate");
        assert_eq!(result["tokens_out"], 3);
        assert_eq!(result["text"], "echo: one two");
        assert_eq!(result["tokens_in"], 6);
        assert!(result["latency_ms"].as_u64().is_some());
        assert!(result["synthetic"].as_bool().unwrap());
    }

    #[test]
    fn prompt_and_token_limits_are_enforced() {
        let long_prompt = "x".repeat(MAX_PROMPT_BYTES + 1);
        let failure = generate(&serde_json::json!({"prompt": long_prompt, "request_id": "r"}))
            .expect_err("oversize prompt refused");
        assert_eq!(failure.code, ErrorCode::InvalidArgument);
        let failure = generate(
            &serde_json::json!({"prompt": "hi", "max_tokens": MAX_TOKENS_OUT as u64 + 1,
            "request_id": "r"}),
        )
        .expect_err("unsound token cap");
        assert_eq!(failure.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn malformed_request_ids_are_refused() {
        let failure = generate(&serde_json::json!({"prompt": "hi", "request_id": "bad id"}))
            .expect_err("malformed request id");
        assert_eq!(failure.code, ErrorCode::InvalidArgument);
        let failure =
            generate(&serde_json::json!({"prompt": "hi"})).expect_err("missing request id");
        assert_eq!(failure.code, ErrorCode::InvalidArgument);
    }
}
