//! Command authorization inventory (#31).
//!
//! Every IPC command flowerlyd implements is classified here, and the router
//! dispatches only classified commands: an implemented handler arm that is
//! missing from this table is unreachable (deny by default). A unit test
//! scans the handler sources so a new command cannot ship unclassified.
//!
//! `Admin` commands change system configuration, security-relevant records
//! or shared resources. Until an explicit administrative capability exists
//! they are still authorized by the same-UID peer check alone; Status
//! reports this transitional policy so it is never mistaken for enforcement.

/// Who may invoke a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandClass {
    /// Ordinary user-scoped operation governed by its domain ownership rules.
    User,
    /// Requires an explicitly authorized administrative capability.
    Admin,
}

/// Administrative policy currently in force, reported by Status.
pub(crate) const ADMIN_POLICY: &str = "same_uid_transitional";

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
        CommandClass::User,
        "own PTY; guardian and cgroup checks",
    ),
    (
        "session.spawn.lookup",
        CommandClass::User,
        "own PTY reservation",
    ),
    ("session.attach", CommandClass::User, "PTY control token"),
    ("session.detach", CommandClass::User, "PTY attachment token"),
    (
        "session.heartbeat",
        CommandClass::User,
        "PTY attachment token",
    ),
    ("session.read", CommandClass::User, "PTY control token"),
    ("session.follow", CommandClass::User, "PTY control token"),
    ("session.write", CommandClass::User, "PTY attachment token"),
    ("session.resize", CommandClass::User, "PTY attachment token"),
    ("session.status", CommandClass::User, "PTY control token"),
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
