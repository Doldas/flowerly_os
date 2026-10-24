# Command authorization inventory

Status: **inventory and deny-by-default routing implemented; administrative
capability enforcement not yet implemented** (#31).

Source of truth: `src/flowerlyd/src/authorization.rs`. Every IPC command that
`flowerlyd` implements is classified there, and the router dispatches only
classified commands. A handler arm missing from the table is unreachable.
`every_handler_command_is_classified` scans the handler sources and fails
when a command is unclassified, or when the table lists a command with no
handler.

## Classes

- **User**: ordinary user-scoped operations. Flow, Goal and Session
  operations keep their existing ownership rules, and PTY commands keep
  their control and attachment tokens, guardian peer checks and cgroup
  requirements.
- **Admin**: system configuration, security-relevant records, shared
  catalogs and Generation activation. These require an **explicitly
  authorized administrative capability**; the same Linux UID is not
  sufficient.

## Current policy (transitional)

No administrative capability mechanism exists yet. Admin commands are still
authorized by the same-UID peer check alone. Status reports
`admin_authorization: "same_uid_transitional"` so the gap is visible and
never mistaken for enforcement. Enforcement is the next step, pending the
mechanism decision recorded in #31.

## Inventory

62 commands: 6 Admin, 56 User.

| Command | Class | Reason |
| --- | --- | --- |
| `provenance.add` | Admin | arbitrary edges can forge the audit trail |
| `generation.activate` | Admin | changes the active system Generation |
| `model.register` | Admin | system model catalog |
| `model.retire` | Admin | system model catalog |
| `provider.register` | Admin | system provider registry |
| `usage.record` | Admin | writes the shared usage ledger directly |
| `flow.create` | User | user Flow |
| `flow.list` | User | user Flow |
| `flow.activate` | User | user Flow |
| `flow.archive` | User | user Flow; General refused |
| `goal.create` | User | user Goal |
| `goal.child` | User | user Goal |
| `goal.show` | User | user Goal |
| `goal.list` | User | user Goal |
| `goal.complete` | User | user Goal |
| `goal.abandon` | User | user Goal |
| `session.create` | User | user Session metadata |
| `session.list` | User | user Session |
| `session.terminate` | User | own Session; PTY control token |
| `session.spawn` | User | own PTY; guardian and cgroup checks |
| `session.spawn.lookup` | User | own PTY reservation |
| `session.attach` | User | PTY control token |
| `session.detach` | User | PTY attachment token |
| `session.heartbeat` | User | PTY attachment token |
| `session.read` | User | PTY control token |
| `session.follow` | User | PTY control token |
| `session.write` | User | PTY attachment token |
| `session.resize` | User | PTY attachment token |
| `session.status` | User | PTY control token |
| `execution.create` | User | user execution record |
| `execution.show` | User | read |
| `execution.list` | User | read |
| `provenance.from` | User | read |
| `provenance.to` | User | read |
| `generation.create` | User | candidate only |
| `generation.list` | User | read |
| `generation.inspect` | User | read |
| `capability.list` | User | descriptive catalog |
| `model.list` | User | read |
| `model.show` | User | read |
| `model.compatibility` | User | read |
| `device.budget` | User | read |
| `provider.list` | User | read |
| `provider.show` | User | read |
| `profile.create` | User | user preference data |
| `profile.clone` | User | user preference data |
| `profile.rename` | User | user preference data |
| `profile.edit` | User | user preference data |
| `profile.import` | User | user preference data |
| `profile.export` | User | read |
| `profile.list` | User | read |
| `profile.show` | User | read |
| `profile.version_history` | User | read |
| `profile.resolve` | User | read |
| `workload.submit` | User | own workload admission |
| `workload.list` | User | read |
| `workload.show` | User | read |
| `workload.stop` | User | own workload transition |
| `workload.complete` | User | own workload transition |
| `workload.usage_brief` | User | read |
| `inference.capabilities` | User | read |
| `inference.request` | User | own admitted workload |

Not routed by `flowerlyd`, so outside this table: IPC `shutdown` (a
destructive control request; it must become Admin when enforcement lands),
`evolutiond`'s `evolution.run_experiment` (execution disabled) and the
`providerd` plugin protocol.
