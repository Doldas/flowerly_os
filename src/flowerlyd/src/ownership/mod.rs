//! Audited Admin ownership operations (#225 PR 7).
//!
//! `ownership.inspect`, `ownership.claim` and `ownership.release` are
//! classified `Admin`; the router has already checked kernel-attested
//! admin group membership. The caller is the real admin uid and is written
//! to `admin_audit` with the reason in the same transaction as the change
//! (see `flowerly_core::storage::admin`). Inspect returns ownership
//! metadata only, never object content.

use std::sync::{Arc, Mutex};

use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::{
    AdminAuditRecord, OwnedObject, StorageError, Store, MAX_AUDIT_REASON,
};
use flowerly_core::types::{Caller, Owner};
use flowerly_core::{coded_error_envelope, error_envelope, ok_envelope};
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) fn handle_command(
    store: &Arc<Mutex<Store>>,
    request: &Envelope,
    caller: Caller,
) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_envelope(request.id.clone(), "storage lock poisoned"),
    };
    match run_command(&mut store, name, args, caller) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err((code, message)) => coded_error_envelope(request.id.clone(), code, message),
    }
}

/// Typed IPC-v1 arguments (see `crate::args` for the shared contract).
#[derive(Debug, Deserialize)]
struct InspectArgs {
    #[serde(rename = "ref")]
    object: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct ClaimArgs {
    #[serde(rename = "ref")]
    object: String,
    to_uid: u32,
    reason: String,
    expect: usize,
}

#[derive(Debug, Deserialize)]
struct ReleaseArgs {
    #[serde(rename = "ref")]
    object: String,
    reason: String,
    expect: usize,
}

type Failure = (ErrorCode, String);

fn typed<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, Failure> {
    crate::args::typed_args("ownership", args)
}

fn require_reason(reason: &str) -> Result<(), Failure> {
    if reason.trim().is_empty() || reason.len() > MAX_AUDIT_REASON {
        return Err((
            ErrorCode::InvalidArgument,
            format!("`reason` must be 1..{MAX_AUDIT_REASON} bytes and not blank"),
        ));
    }
    Ok(())
}

fn storage(error: StorageError) -> Failure {
    (error.ipc_error_code(), error.to_string())
}

fn owner_json(owner: Owner) -> Value {
    json!({ "class": owner.class(), "uid": owner.uid() })
}

fn objects_json(objects: &[OwnedObject]) -> Value {
    objects
        .iter()
        .map(|object| json!({ "ref": object.object_ref, "owner": owner_json(object.owner) }))
        .collect()
}

fn audit_json(records: &[AdminAuditRecord]) -> Value {
    records
        .iter()
        .map(|record| {
            json!({
                "actor_uid": record.actor_uid,
                "command": record.command,
                "from": owner_json(record.from_owner),
                "to": owner_json(record.to_owner),
                "reason": record.reason,
                "created_at": record.created_at,
            })
        })
        .collect()
}

fn run_command(
    store: &mut Store,
    name: &str,
    args: &Value,
    caller: Caller,
) -> Result<Value, Failure> {
    match name {
        "ownership.inspect" => {
            let typed: InspectArgs = typed(args)?;
            require_reason(&typed.reason)?;
            let report = store
                .inspect_ownership(caller, &typed.object, &typed.reason)
                .map_err(storage)?;
            tracing::info!(
                object = %report.object_ref,
                admin_uid = caller.uid(),
                "ownership inspected"
            );
            Ok(json!({
                "ref": report.object_ref,
                "owner": owner_json(report.owner),
                "created_by": report.created_by,
                "component": objects_json(&report.component),
                "foreign_links": objects_json(&report.foreign_links),
                "audit": audit_json(&report.audit),
            }))
        }
        "ownership.claim" => {
            let typed: ClaimArgs = typed(args)?;
            require_reason(&typed.reason)?;
            let moved = store
                .claim_ownership(
                    caller,
                    &typed.object,
                    typed.to_uid,
                    &typed.reason,
                    typed.expect,
                )
                .map_err(storage)?;
            tracing::info!(
                object = %typed.object,
                admin_uid = caller.uid(),
                to_uid = typed.to_uid,
                moved = moved.len(),
                "ownership claimed"
            );
            Ok(json!({ "moved": moved, "owner": owner_json(Owner::User(typed.to_uid)) }))
        }
        "ownership.release" => {
            let typed: ReleaseArgs = typed(args)?;
            require_reason(&typed.reason)?;
            let moved = store
                .release_ownership(caller, &typed.object, &typed.reason, typed.expect)
                .map_err(storage)?;
            tracing::info!(
                object = %typed.object,
                admin_uid = caller.uid(),
                moved = moved.len(),
                "ownership released"
            );
            Ok(json!({ "moved": moved, "owner": owner_json(Owner::Unresolved) }))
        }
        other => Err((
            ErrorCode::InvalidArgument,
            format!("ownership: unknown command `{other}`"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowerly_core::types::{Goal, GoalId};

    const ADMIN: Caller = Caller::new(1500);

    fn store() -> Arc<Mutex<Store>> {
        let mut store = Store::open_in_memory().expect("open");
        store.migrate().expect("migrate");
        store.ensure_general_flow().expect("General");
        // A pre-#225 row: no provable owner.
        let mut legacy = Goal::new("secret objective", "user");
        legacy.id = GoalId::from_raw("goal-legacy");
        assert_eq!(legacy.owner, Owner::Unresolved);
        store.insert_goal(&legacy).expect("legacy goal");
        Arc::new(Mutex::new(store))
    }

    fn call(store: &Arc<Mutex<Store>>, name: &str, args: Value) -> Result<Value, Failure> {
        let request = Envelope::new(Payload::Command {
            name: name.into(),
            args,
        });
        match handle_command(store, &request, ADMIN).payload {
            Payload::Ok { data } => Ok(data),
            Payload::Err { code, message } => Err((code.expect("typed"), message)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn inspect_claim_and_release_round_trip_over_ipc() {
        let store = store();
        let inspected = call(
            &store,
            "ownership.inspect",
            json!({"ref": "goal-legacy", "reason": "triage"}),
        )
        .unwrap();
        assert_eq!(inspected["owner"]["class"], "unresolved");
        assert_eq!(inspected["component"].as_array().unwrap().len(), 1);
        // Metadata only: no Goal content leaves through inspect.
        assert!(!inspected.to_string().contains("secret objective"));

        let claimed = call(
            &store,
            "ownership.claim",
            json!({"ref": "goal-legacy", "to_uid": 1001, "reason": "Alice's", "expect": 1}),
        )
        .unwrap();
        assert_eq!(claimed["moved"], json!(["goal-legacy"]));
        assert_eq!(claimed["owner"], json!({"class": "user", "uid": 1001}));

        let released = call(
            &store,
            "ownership.release",
            json!({"ref": "goal-legacy", "reason": "wrong uid", "expect": 1}),
        )
        .unwrap();
        assert_eq!(released["owner"]["class"], "unresolved");

        let history = call(
            &store,
            "ownership.inspect",
            json!({"ref": "goal-legacy", "reason": "verify"}),
        )
        .unwrap();
        let commands: Vec<_> = history["audit"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["command"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            commands,
            ["ownership.inspect", "ownership.claim", "ownership.release"]
        );
        assert!(history["audit"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["actor_uid"] == 1500));
    }

    #[test]
    fn malformed_arguments_are_typed_client_errors() {
        let store = store();
        for (name, args, code) in [
            ("ownership.inspect", json!({}), ErrorCode::InvalidArgument),
            (
                "ownership.inspect",
                json!({"ref": "goal-legacy", "reason": " "}),
                ErrorCode::InvalidArgument,
            ),
            (
                "ownership.claim",
                json!({"ref": "goal-legacy", "to_uid": -1, "reason": "x", "expect": 1}),
                ErrorCode::InvalidArgument,
            ),
            (
                "ownership.claim",
                json!({"ref": "goal-legacy", "to_uid": 1001, "reason": "x"}),
                ErrorCode::InvalidArgument,
            ),
            (
                "ownership.claim",
                json!({"ref": "goal-legacy", "to_uid": 1001, "reason": "x", "expect": 2}),
                ErrorCode::Conflict,
            ),
            (
                "ownership.release",
                json!({"ref": "goal-legacy", "reason": "x", "expect": 1}),
                ErrorCode::Conflict,
            ),
            (
                "ownership.release",
                json!({"ref": "goal-missing", "reason": "x", "expect": 1}),
                ErrorCode::NotFound,
            ),
            ("ownership.transfer", json!({}), ErrorCode::InvalidArgument),
        ] {
            let (got, message) = call(&store, name, args.clone()).unwrap_err();
            assert_eq!(got, code, "{name} {args}: {message}");
        }
    }
}
