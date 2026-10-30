//! OpenAI-compatible wire format: request bodies and strict parsing of the
//! two responses the backend reads (ADR-0022 §5, §7, §9).
//!
//! Generated text is untrusted data: it is returned as a bounded string
//! and nothing else from the response is passed through.
use serde_json::Value;

use super::config::{MAX_DISCOVERED_MODELS, MAX_TEXT_BYTES};

pub fn chat_body(model: &str, prompt: &str, max_tokens: u64) -> Value {
    // No tools, functions or response formats are offered: the provider has
    // nothing to call and nothing structured to return.
    serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens,
        "stream": false,
    })
}

/// The model ids a discovery response lists (`{"data": [{"id": ...}]}`).
pub fn parse_models(body: &Value) -> Result<Vec<String>, &'static str> {
    let data = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or("model discovery response has no `data` array")?;
    if data.len() > MAX_DISCOVERED_MODELS {
        return Err("model discovery response lists too many models");
    }
    Ok(data
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .filter(|id| id.len() <= 256)
        .map(str::to_owned)
        .collect())
}

/// Token usage when the server reports it; `None` means unknown, never 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Completion {
    pub model: String,
    pub text: String,
    pub truncated: bool,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChatRejection {
    /// Structurally not a chat completion we can read.
    Invalid(&'static str),
    /// Tool or function calls: never offered, never accepted.
    Unsupported(&'static str),
    /// The server answered with a different model than requested.
    ModelMismatch { reported: String },
    /// The model produced no text (for example only reasoning tokens).
    Empty,
}

pub fn parse_chat(body: &Value, requested_model: &str) -> Result<Completion, ChatRejection> {
    let reported = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or(ChatRejection::Invalid("chat response has no `model`"))?;
    if reported != requested_model {
        return Err(ChatRejection::ModelMismatch {
            reported: reported.chars().take(128).collect(),
        });
    }
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or(ChatRejection::Invalid("chat response has no choices"))?;
    let message = choice
        .get("message")
        .filter(|message| message.is_object())
        .ok_or(ChatRejection::Invalid("chat choice has no message object"))?;
    let has_calls = |field: &str| {
        message
            .get(field)
            .is_some_and(|calls| !calls.is_null() && calls.as_array().is_none_or(|a| !a.is_empty()))
    };
    if has_calls("tool_calls") || has_calls("function_call") {
        return Err(ChatRejection::Unsupported(
            "chat response requested tool or function calls",
        ));
    }
    let content = match message.get("content") {
        Some(Value::String(text)) => text.as_str(),
        None | Some(Value::Null) => "",
        Some(_) => {
            return Err(ChatRejection::Unsupported(
                "chat response content is not plain text",
            ))
        }
    };
    if content.is_empty() {
        return Err(ChatRejection::Empty);
    }
    let (text, truncated) = bounded_text(content);
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(|reason| reason.chars().take(32).collect());
    Ok(Completion {
        model: reported.to_owned(),
        text,
        truncated,
        finish_reason,
        usage: parse_usage(body),
    })
}

/// Usage only when both counts are present as non-negative integers.
pub fn parse_usage(body: &Value) -> Option<Usage> {
    let usage = body.get("usage")?;
    Some(Usage {
        prompt_tokens: usage.get("prompt_tokens")?.as_u64()?,
        completion_tokens: usage.get("completion_tokens")?.as_u64()?,
    })
}

fn bounded_text(text: &str) -> (String, bool) {
    if text.len() <= MAX_TEXT_BYTES {
        return (text.to_owned(), false);
    }
    let mut end = MAX_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(model: &str, message: Value, usage: Value) -> Value {
        serde_json::json!({
            "id": "c-1", "model": model,
            "choices": [{"index": 0, "message": message, "finish_reason": "stop"}],
            "usage": usage,
        })
    }

    #[test]
    fn a_plain_completion_is_parsed_with_reported_usage() {
        let body = chat(
            "gpt-oss:20b",
            serde_json::json!({"role": "assistant", "content": "flowerly-ready"}),
            serde_json::json!({"prompt_tokens": 9, "completion_tokens": 3, "total_tokens": 12}),
        );
        let parsed = parse_chat(&body, "gpt-oss:20b").unwrap();
        assert_eq!(parsed.text, "flowerly-ready");
        assert_eq!(
            parsed.usage,
            Some(Usage {
                prompt_tokens: 9,
                completion_tokens: 3
            })
        );
        assert_eq!(parsed.finish_reason.as_deref(), Some("stop"));
        assert!(!parsed.truncated);
    }

    #[test]
    fn missing_or_partial_usage_is_unknown_not_zero() {
        for usage in [
            Value::Null,
            serde_json::json!({}),
            serde_json::json!({"prompt_tokens": 5}),
            serde_json::json!({"prompt_tokens": -1, "completion_tokens": 2}),
            serde_json::json!({"prompt_tokens": "5", "completion_tokens": 2}),
        ] {
            let body = chat("m", serde_json::json!({"content": "x"}), usage.clone());
            assert_eq!(parse_chat(&body, "m").unwrap().usage, None, "{usage}");
        }
    }

    #[test]
    fn mismatched_models_tool_calls_and_empty_output_are_rejected() {
        let ok = serde_json::json!({"content": "hi"});
        assert_eq!(
            parse_chat(&chat("other", ok.clone(), Value::Null), "m"),
            Err(ChatRejection::ModelMismatch {
                reported: "other".to_owned()
            })
        );
        for message in [
            serde_json::json!({"content": "hi", "tool_calls": [{"id": "t"}]}),
            serde_json::json!({"content": null, "function_call": {"name": "f"}}),
            serde_json::json!({"content": [{"type": "image_url"}]}),
        ] {
            assert!(matches!(
                parse_chat(&chat("m", message, Value::Null), "m"),
                Err(ChatRejection::Unsupported(_))
            ));
        }
        // An empty tool_calls list is not a call.
        assert!(parse_chat(
            &chat(
                "m",
                serde_json::json!({"content": "hi", "tool_calls": []}),
                Value::Null
            ),
            "m"
        )
        .is_ok());
        for message in [
            serde_json::json!({"content": ""}),
            serde_json::json!({"content": null}),
        ] {
            assert_eq!(
                parse_chat(&chat("m", message, Value::Null), "m"),
                Err(ChatRejection::Empty)
            );
        }
        for body in [
            serde_json::json!({"choices": []}),
            serde_json::json!({"model": "m", "choices": []}),
            serde_json::json!({"model": "m", "choices": [{"message": "text"}]}),
            serde_json::json!([]),
        ] {
            assert!(
                matches!(parse_chat(&body, "m"), Err(ChatRejection::Invalid(_))),
                "{body}"
            );
        }
    }

    #[test]
    fn oversized_text_is_truncated_on_a_char_boundary_and_flagged() {
        let text = "é".repeat(MAX_TEXT_BYTES); // 2 bytes each
        let body = chat("m", serde_json::json!({"content": text}), Value::Null);
        let parsed = parse_chat(&body, "m").unwrap();
        assert!(parsed.truncated);
        assert!(parsed.text.len() <= MAX_TEXT_BYTES);
        assert!(parsed.text.chars().all(|ch| ch == 'é'));
    }

    #[test]
    fn discovery_lists_ids_and_is_bounded() {
        let body =
            serde_json::json!({"data": [{"id": "gpt-oss:20b"}, {"name": "no id"}, {"id": 5}]});
        assert_eq!(parse_models(&body).unwrap(), ["gpt-oss:20b"]);
        assert!(parse_models(&serde_json::json!({"models": []})).is_err());
        let many: Vec<Value> = (0..=MAX_DISCOVERED_MODELS)
            .map(|index| serde_json::json!({"id": index.to_string()}))
            .collect();
        assert!(parse_models(&serde_json::json!({"data": many})).is_err());
    }
}
