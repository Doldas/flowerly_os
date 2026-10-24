//! Typed IPC-v1 command arguments (#53).
//!
//! Each service deserializes its known argument fields into a typed struct
//! before touching the database. Contract for every migrated command:
//! arguments must be a JSON object; a missing field and an explicit `null`
//! both mean "absent"; a known field of the wrong JSON type fails as
//! `invalid_argument`; unknown fields are ignored so older and newer
//! clients keep working.

use flowerly_core::ipc::ErrorCode;
use serde_json::Value;

pub(crate) fn typed_args<T: serde::de::DeserializeOwned>(
    service: &str,
    args: &Value,
) -> Result<T, (ErrorCode, String)> {
    if !args.is_object() {
        return Err((
            ErrorCode::InvalidArgument,
            format!("{service} arguments must be a JSON object"),
        ));
    }
    serde_json::from_value(args.clone()).map_err(|error| {
        (
            ErrorCode::InvalidArgument,
            format!("invalid {service} arguments: {error}"),
        )
    })
}
