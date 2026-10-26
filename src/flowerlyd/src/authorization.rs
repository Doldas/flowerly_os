//! Command authorization inventory (#31).
//!
//! Every IPC command flowerlyd implements is classified here, and the router
//! dispatches only classified commands: an implemented handler arm that is
//! missing from this table is unreachable (deny by default). A unit test
//! scans the handler sources so a new command cannot ship unclassified.
//!
//! `Admin` commands change system configuration, security-relevant records
//! or shared resources. They require kernel-attested membership of the
//! configured admin group (`AGENTIC_ADMIN_GROUP`); the service account is
//! not implicitly an administrator, and with no admin group configured every
//! Admin command is refused.
//!
//! `ServiceAccount` commands drive PTY shells, which run as the service
//! account and therefore hold full authority over its state database. Until
//! shells can be launched as the requesting user they are limited to peers
//! running as the service account itself, even for admin group members.

use flowerly_core::ipc::{AccessPolicy, ErrorCode, PeerIdentity};

/// Who may invoke a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandClass {
    /// Any admitted peer: the service account or a users/admin group member.
    User,
    /// Requires membership of the configured admin group.
    Admin,
    /// Requires the peer to run as the service account.
    ServiceAccount,
}

/// Administrative policy in force, reported by Status.
pub(crate) fn admin_policy(policy: &AccessPolicy) -> &'static str {
    if policy.admin_gid.is_some() {
        "linux_group"
    } else {
        "denied_unconfigured"
    }
}

/// Who may connect, reported by Status. Only a users group admits peers
/// other than the service account.
pub(crate) fn client_policy(policy: &AccessPolicy) -> &'static str {
    if policy.users_gid.is_some() {
        "service_account_and_linux_groups"
    } else {
        "service_account_only"
    }
}

/// Environment variable that, set to [`UNENFORCED_OWNERSHIP_ACK_VALUE`],
/// lets a test harness start with a users group before ownership is enforced.
pub(crate) const UNENFORCED_OWNERSHIP_ACK: &str = "AGENTIC_TEST_ONLY_UNENFORCED_OWNERSHIP";
pub(crate) const UNENFORCED_OWNERSHIP_ACK_VALUE: &str = "members-can-read-each-others-data";

/// User commands whose data is not yet owner-scoped (#225).
pub(crate) fn pending_scopes() -> impl Iterator<Item = (&'static str, &'static str)> {
    DATA_SCOPES.iter().filter_map(|(name, scope)| match scope {
        DataScope::Pending(pr) => Some((*name, *pr)),
        _ => None,
    })
}

/// Refuse to admit other UIDs while any User command is not owner-scoped
/// (#225): users group members could read and change each other's data.
/// Only an exact, deliberately unwieldy acknowledgment (used by the
/// cross-UID tests) starts anyway; the shipped unit unsets it.
pub(crate) fn refuse_unenforced_multi_user(
    policy: &AccessPolicy,
    ack: Option<&str>,
) -> Result<(), String> {
    let pending = pending_scopes().count();
    if policy.users_gid.is_none() || pending == 0 || ack == Some(UNENFORCED_OWNERSHIP_ACK_VALUE) {
        return Ok(());
    }
    Err(format!(
        "a users group is configured, but per-user data ownership is not enforced yet \
         (#225; {pending} User commands pending): users could read and change each other's \
         data. Unset AGENTIC_USERS_GROUP / users_group; flowerlyd stays service-account-only \
         until #225 is closed. Test harnesses only: \
         {UNENFORCED_OWNERSHIP_ACK}={UNENFORCED_OWNERSHIP_ACK_VALUE}"
    ))
}

/// How a User command's stored data is separated between uids (#225).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataScope {
    /// Only the caller's own objects, plus the shared General Flow for Flow
    /// commands and read-only `system` profiles for profile commands;
    /// provenance edges only when both endpoints are visible. Enforced in the
    /// owner-scoped storage API, inside the same SQLite statement or
    /// transaction as any mutation; other uids' objects are `not_found`.
    OwnerScoped,
    /// Shared system catalog data, read-only for User commands.
    SharedSystem,
    /// Reads no stored data.
    Stateless,
    /// Not yet owner-scoped; the named #225 PR scopes it. While any command is
    /// pending, multi-user admission stays disabled (startup refusal).
    Pending(&'static str),
}

/// The data scope of every User command. A unit test keeps this in step with
/// [`COMMANDS`].
pub(crate) const DATA_SCOPES: &[(&str, DataScope)] = &[
    ("flow.create", DataScope::OwnerScoped),
    ("flow.list", DataScope::OwnerScoped),
    ("flow.activate", DataScope::OwnerScoped),
    ("flow.archive", DataScope::OwnerScoped),
    ("goal.create", DataScope::OwnerScoped),
    ("goal.child", DataScope::OwnerScoped),
    ("goal.show", DataScope::OwnerScoped),
    ("goal.list", DataScope::OwnerScoped),
    ("goal.complete", DataScope::OwnerScoped),
    ("goal.abandon", DataScope::OwnerScoped),
    ("session.create", DataScope::OwnerScoped),
    ("session.list", DataScope::OwnerScoped),
    ("session.terminate", DataScope::OwnerScoped),
    ("execution.create", DataScope::OwnerScoped),
    ("execution.show", DataScope::OwnerScoped),
    ("execution.list", DataScope::OwnerScoped),
    ("provenance.from", DataScope::OwnerScoped),
    ("provenance.to", DataScope::OwnerScoped),
    (
        "generation.create",
        DataScope::Pending("PR 6: private candidates"),
    ),
    (
        "generation.list",
        DataScope::Pending("PR 6: private candidates"),
    ),
    (
        "generation.inspect",
        DataScope::Pending("PR 6: private candidates"),
    ),
    ("capability.list", DataScope::Stateless),
    ("model.list", DataScope::SharedSystem),
    ("model.show", DataScope::SharedSystem),
    ("model.compatibility", DataScope::SharedSystem),
    ("device.budget", DataScope::SharedSystem),
    ("provider.list", DataScope::SharedSystem),
    ("provider.show", DataScope::SharedSystem),
    ("profile.create", DataScope::OwnerScoped),
    ("profile.clone", DataScope::OwnerScoped),
    ("profile.rename", DataScope::OwnerScoped),
    ("profile.edit", DataScope::OwnerScoped),
    ("profile.import", DataScope::OwnerScoped),
    ("profile.export", DataScope::OwnerScoped),
    ("profile.list", DataScope::OwnerScoped),
    ("profile.show", DataScope::OwnerScoped),
    ("profile.version_history", DataScope::OwnerScoped),
    ("profile.resolve", DataScope::OwnerScoped),
    ("workload.submit", DataScope::OwnerScoped),
    ("workload.list", DataScope::OwnerScoped),
    ("workload.show", DataScope::OwnerScoped),
    ("workload.stop", DataScope::OwnerScoped),
    ("workload.complete", DataScope::OwnerScoped),
    ("workload.usage_brief", DataScope::OwnerScoped),
    ("inference.capabilities", DataScope::SharedSystem),
    ("inference.request", DataScope::OwnerScoped),
];

/// Decide a command for a kernel-attested peer. Only inventoried commands
/// can be authorized; request arguments play no part in the decision.
pub(crate) fn authorize(
    name: &str,
    peer: &PeerIdentity,
    policy: &AccessPolicy,
) -> Result<(), (ErrorCode, String)> {
    match classify(name) {
        None => Err((
            ErrorCode::InvalidArgument,
            format!("unknown command: {name}"),
        )),
        Some(CommandClass::User) => Ok(()),
        Some(CommandClass::Admin) if policy.is_admin(peer) => Ok(()),
        Some(CommandClass::Admin) => Err((
            ErrorCode::PermissionDenied,
            if policy.admin_gid.is_some() {
                format!("{name} requires membership of the configured admin group")
            } else {
                format!("{name} is refused: no admin group is configured")
            },
        )),
        Some(CommandClass::ServiceAccount) if peer.service_account => Ok(()),
        Some(CommandClass::ServiceAccount) => Err((
            ErrorCode::PermissionDenied,
            format!("{name} is limited to the service account: PTY shells run with its authority"),
        )),
    }
}

/// The complete command inventory: name, class and the reason for the class.
pub(crate) const COMMANDS: &[(&str, CommandClass, &str)] = &[
    // Flows, Goals and Sessions: user-scoped with existing ownership rules.
    ("flow.create", CommandClass::User, "user Flow"),
    ("flow.list", CommandClass::User, "user Flow"),
    ("flow.activate", CommandClass::User, "user Flow"),
    (
        "flow.archive",
        CommandClass::User,
        "user Flow; General refused",
    ),
    ("goal.create", CommandClass::User, "user Goal"),
    ("goal.child", CommandClass::User, "user Goal"),
    ("goal.show", CommandClass::User, "user Goal"),
    ("goal.list", CommandClass::User, "user Goal"),
    ("goal.complete", CommandClass::User, "user Goal"),
    ("goal.abandon", CommandClass::User, "user Goal"),
    (
        "session.create",
        CommandClass::User,
        "user Session metadata",
    ),
    ("session.list", CommandClass::User, "user Session"),
    (
        "session.terminate",
        CommandClass::User,
        "own Session; PTY control token",
    ),
    (
        "session.spawn",
        CommandClass::ServiceAccount,
        "own PTY; guardian and cgroup checks; shell runs as the service account",
    ),
    (
        "session.spawn.lookup",
        CommandClass::ServiceAccount,
        "own PTY reservation; shell runs as the service account",
    ),
    (
        "session.attach",
        CommandClass::ServiceAccount,
        "PTY control token; shell runs as the service account",
    ),
    (
        "session.detach",
        CommandClass::ServiceAccount,
        "PTY attachment token; shell runs as the service account",
    ),
    (
        "session.heartbeat",
        CommandClass::ServiceAccount,
        "PTY attachment token; shell runs as the service account",
    ),
    (
        "session.read",
        CommandClass::ServiceAccount,
        "PTY control token; shell runs as the service account",
    ),
    (
        "session.follow",
        CommandClass::ServiceAccount,
        "PTY control token; shell runs as the service account",
    ),
    (
        "session.write",
        CommandClass::ServiceAccount,
        "PTY attachment token; shell runs as the service account",
    ),
    (
        "session.resize",
        CommandClass::ServiceAccount,
        "PTY attachment token; shell runs as the service account",
    ),
    (
        "session.status",
        CommandClass::ServiceAccount,
        "PTY control token; shell runs as the service account",
    ),
    // Executions and provenance queries record or read the user's own work.
    (
        "execution.create",
        CommandClass::User,
        "user execution record",
    ),
    ("execution.show", CommandClass::User, "read"),
    ("execution.list", CommandClass::User, "read"),
    ("provenance.from", CommandClass::User, "read"),
    ("provenance.to", CommandClass::User, "read"),
    (
        "provenance.add",
        CommandClass::Admin,
        "arbitrary edges can forge the audit trail",
    ),
    // Generations: candidates are user work; activation changes the system.
    ("generation.create", CommandClass::User, "candidate only"),
    ("generation.list", CommandClass::User, "read"),
    ("generation.inspect", CommandClass::User, "read"),
    (
        "generation.activate",
        CommandClass::Admin,
        "changes the active system Generation",
    ),
    ("capability.list", CommandClass::User, "descriptive catalog"),
    // Compute: the model and provider catalogs are shared system configuration.
    (
        "model.register",
        CommandClass::Admin,
        "system model catalog",
    ),
    ("model.retire", CommandClass::Admin, "system model catalog"),
    ("model.list", CommandClass::User, "read"),
    ("model.show", CommandClass::User, "read"),
    ("model.compatibility", CommandClass::User, "read"),
    ("device.budget", CommandClass::User, "read"),
    (
        "provider.register",
        CommandClass::Admin,
        "system provider registry",
    ),
    ("provider.list", CommandClass::User, "read"),
    ("provider.show", CommandClass::User, "read"),
    ("profile.create", CommandClass::User, "user preference data"),
    ("profile.clone", CommandClass::User, "user preference data"),
    ("profile.rename", CommandClass::User, "user preference data"),
    ("profile.edit", CommandClass::User, "user preference data"),
    ("profile.import", CommandClass::User, "user preference data"),
    ("profile.export", CommandClass::User, "read"),
    ("profile.list", CommandClass::User, "read"),
    ("profile.show", CommandClass::User, "read"),
    ("profile.version_history", CommandClass::User, "read"),
    ("profile.resolve", CommandClass::User, "read"),
    (
        "workload.submit",
        CommandClass::User,
        "own workload admission",
    ),
    ("workload.list", CommandClass::User, "read"),
    ("workload.show", CommandClass::User, "read"),
    (
        "workload.stop",
        CommandClass::User,
        "own workload transition",
    ),
    (
        "workload.complete",
        CommandClass::User,
        "own workload transition",
    ),
    ("workload.usage_brief", CommandClass::User, "read"),
    (
        "usage.record",
        CommandClass::Admin,
        "writes the shared usage ledger directly",
    ),
    ("inference.capabilities", CommandClass::User, "read"),
    (
        "inference.request",
        CommandClass::User,
        "own admitted workload",
    ),
];

/// Class of a command, or `None` for anything not in the inventory.
pub(crate) fn classify(name: &str) -> Option<CommandClass> {
    COMMANDS
        .iter()
        .find(|(command, _, _)| *command == name)
        .map(|(_, class, _)| *class)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_user_command_has_exactly_one_data_scope() {
        let users: BTreeSet<_> = with_class(CommandClass::User).into_iter().collect();
        let scoped: Vec<_> = DATA_SCOPES.iter().map(|(name, _)| *name).collect();
        let unique: BTreeSet<_> = scoped.iter().copied().collect();
        assert_eq!(unique.len(), scoped.len(), "duplicate data scope");
        assert_eq!(unique, users, "User commands and data scopes differ");
    }

    #[test]
    fn a_users_group_refuses_to_start_without_the_exact_acknowledgment() {
        let private = AccessPolicy {
            users_gid: None,
            admin_gid: Some(ADMIN),
        };
        let shared = AccessPolicy {
            users_gid: Some(USERS),
            admin_gid: Some(ADMIN),
        };
        assert!(refuse_unenforced_multi_user(&private, None).is_ok());
        assert!(refuse_unenforced_multi_user(&private, Some("x")).is_ok());
        for ack in [
            None,
            Some(""),
            Some("1"),
            Some("true"),
            Some("Members-can-read-each-others-data"),
        ] {
            let error = refuse_unenforced_multi_user(&shared, ack).unwrap_err();
            assert!(error.contains("#225"), "{error}");
        }
        assert!(
            refuse_unenforced_multi_user(&shared, Some(UNENFORCED_OWNERSHIP_ACK_VALUE)).is_ok()
        );
    }

    const SERVICE_UID: u32 = 961;
    const USERS: u32 = 2000;
    const ADMIN: u32 = 2001;

    fn peer(uid: u32, gid: u32, groups: &[u32]) -> PeerIdentity {
        PeerIdentity {
            uid,
            gid,
            pid: Some(1),
            groups: groups.into(),
            service_account: uid == SERVICE_UID,
        }
    }

    fn with_class(class: CommandClass) -> Vec<&'static str> {
        COMMANDS
            .iter()
            .filter(|(_, c, _)| *c == class)
            .map(|(name, _, _)| *name)
            .collect()
    }

    fn denied(name: &str, peer: &PeerIdentity, policy: &AccessPolicy) -> bool {
        matches!(
            authorize(name, peer, policy),
            Err((ErrorCode::PermissionDenied, _))
        )
    }

    #[test]
    fn admin_commands_require_admin_group_membership() {
        let admin = with_class(CommandClass::Admin);
        assert_eq!(admin.len(), 6, "{admin:?}");
        let configured = AccessPolicy {
            users_gid: Some(USERS),
            admin_gid: Some(ADMIN),
        };
        let service = peer(SERVICE_UID, SERVICE_UID, &[]);
        let user = peer(1000, 1000, &[USERS]);
        let admin_supplementary = peer(1001, 1001, &[USERS, ADMIN]);
        let admin_primary = peer(1002, ADMIN, &[]);
        let service_admin = peer(SERVICE_UID, SERVICE_UID, &[ADMIN]);
        for name in &admin {
            // The service account is never implicitly an administrator.
            assert!(denied(name, &service, &configured), "{name}");
            assert!(denied(name, &user, &configured), "{name}");
            for granted in [&admin_supplementary, &admin_primary, &service_admin] {
                assert_eq!(authorize(name, granted, &configured), Ok(()), "{name}");
            }
            // No admin group configured: refused for everyone.
            for policy in [
                AccessPolicy::default(),
                AccessPolicy {
                    users_gid: Some(ADMIN),
                    admin_gid: None,
                },
            ] {
                for anyone in [&service, &admin_supplementary, &admin_primary] {
                    let (code, message) = authorize(name, anyone, &policy).unwrap_err();
                    assert_eq!(code, ErrorCode::PermissionDenied, "{name}");
                    assert!(message.contains("no admin group"), "{message}");
                }
            }
        }
    }

    #[test]
    fn pty_commands_are_limited_to_the_service_account() {
        let pty = with_class(CommandClass::ServiceAccount);
        assert_eq!(pty.len(), 10, "{pty:?}");
        assert!(pty.iter().all(|name| name.starts_with("session.")));
        let policy = AccessPolicy {
            users_gid: Some(USERS),
            admin_gid: Some(ADMIN),
        };
        for name in &pty {
            assert_eq!(
                authorize(name, &peer(SERVICE_UID, SERVICE_UID, &[]), &policy),
                Ok(()),
                "{name}"
            );
            // Admin group membership does not grant a service-account shell.
            for other in [peer(1000, 1000, &[USERS]), peer(1001, ADMIN, &[USERS])] {
                assert!(denied(name, &other, &policy), "{name}");
            }
        }
    }

    #[test]
    fn user_commands_are_open_to_admitted_peers_and_unknown_commands_never_are() {
        let policy = AccessPolicy {
            users_gid: Some(USERS),
            admin_gid: Some(ADMIN),
        };
        let user = peer(1000, 1000, &[USERS]);
        for name in with_class(CommandClass::User) {
            assert_eq!(authorize(name, &user, &policy), Ok(()), "{name}");
        }
        let admin = peer(1001, ADMIN, &[]);
        for name in ["", "admin", "model.register.force", "Model.register"] {
            assert!(matches!(
                authorize(name, &admin, &policy),
                Err((ErrorCode::InvalidArgument, _))
            ));
        }
    }

    #[test]
    fn status_labels_describe_the_policy() {
        assert_eq!(
            admin_policy(&AccessPolicy::default()),
            "denied_unconfigured"
        );
        assert_eq!(
            client_policy(&AccessPolicy::default()),
            "service_account_only"
        );
        let groups = AccessPolicy {
            users_gid: Some(USERS),
            admin_gid: Some(ADMIN),
        };
        assert_eq!(admin_policy(&groups), "linux_group");
        assert_eq!(client_policy(&groups), "service_account_and_linux_groups");
        // An admin group alone admits no other UID (#225).
        let admin_only = AccessPolicy {
            users_gid: None,
            admin_gid: Some(ADMIN),
        };
        assert_eq!(client_policy(&admin_only), "service_account_only");
    }

    /// Command names used as match-arm patterns in handler sources
    /// (`"x.y" =>`, `"x.y" | "x.z"`, and the PTY `routes` list).
    fn handler_command_names() -> BTreeSet<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![root.clone()];
        let mut names = BTreeSet::new();
        while let Some(path) = files.pop() {
            if path.is_dir() {
                files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") || path.ends_with("authorization.rs")
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let production = source.split("#[cfg(test)]").next().unwrap_or("");
            for line in production.lines() {
                let mut rest = line.trim().trim_start_matches('|').trim();
                let mut literals = Vec::new();
                while let Some(after) = rest.strip_prefix('"') {
                    let Some(end) = after.find('"') else { break };
                    literals.push(&after[..end]);
                    rest = after[end + 1..].trim().trim_start_matches('|').trim();
                }
                let is_pattern = rest.is_empty() || rest.starts_with("=>") || rest.starts_with(')');
                if !is_pattern {
                    continue;
                }
                for literal in literals {
                    let command = literal.contains('.')
                        && literal
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_');
                    if command {
                        names.insert(literal.to_owned());
                    }
                }
            }
        }
        names
    }

    #[test]
    fn every_handler_command_is_classified() {
        let handlers = handler_command_names();
        assert!(
            handlers.len() > 50,
            "scanner found too few commands: {handlers:?}"
        );
        let unclassified: Vec<_> = handlers
            .iter()
            .filter(|name| classify(name).is_none())
            .collect();
        assert!(
            unclassified.is_empty(),
            "unclassified commands: {unclassified:?}"
        );
        let classified: BTreeSet<String> = COMMANDS
            .iter()
            .map(|(name, _, _)| (*name).to_owned())
            .collect();
        let stale: Vec<_> = classified
            .difference(&handlers)
            .filter(|name| name.as_str() != "capability.list")
            .collect();
        assert!(
            stale.is_empty(),
            "inventory lists commands with no handler: {stale:?}"
        );
    }

    #[test]
    fn inventory_has_no_duplicates() {
        let mut seen = BTreeSet::new();
        for (name, _, _) in COMMANDS {
            assert!(seen.insert(*name), "duplicate inventory entry {name}");
        }
    }

    #[test]
    fn named_administrative_operations_are_admin() {
        for name in [
            "generation.activate",
            "model.register",
            "model.retire",
            "provider.register",
        ] {
            assert_eq!(classify(name), Some(CommandClass::Admin), "{name}");
        }
        assert_eq!(classify("flow.create"), Some(CommandClass::User));
        assert_eq!(classify("flow.drop_everything"), None);
    }
}
