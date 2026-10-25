# Command authorization

Status: **Linux group authorization enforced in `flowerlyd`** (#31). Not yet
verified on a real Arch VM boot. Per-user data ownership between different
users is **not** implemented (see [Limits](#limits)).

Source of truth: `src/flowerlyd/src/authorization.rs`. Every IPC command that
`flowerlyd` implements is classified there, and the router dispatches only
commands that are classified and authorized for the kernel-attested peer.
A handler arm missing from the table is unreachable.
`every_handler_command_is_classified` scans the handler sources and fails
when a command is unclassified, or when the table lists a command with no
handler.

## Model

`flowerlyd` runs as its service account (`flowerly`). Two Linux groups
control access:

| Peer | Connect | User commands | Admin commands, IPC `shutdown` | ServiceAccount (PTY) commands |
| --- | --- | --- | --- | --- |
| Service account, not in `flowerly-admin` | yes | yes | **no** | yes |
| Service account in `flowerly-admin` | yes | yes | yes | yes |
| `flowerly-users` member | yes | yes | no | **no** |
| `flowerly-admin` member (other UID) | yes | yes | yes | **no** |
| Anyone else, including root | **no** | — | — | — |

- **Deny by default.** No admin group configured means every Admin command
  and IPC `shutdown` is refused for everyone, including the service account.
  The same UID is never implicitly an administrator.
- **Kernel-verified.** Identity comes from `SO_PEERCRED` (uid, primary gid,
  pid) and `SO_PEERGROUPS` (supplementary groups). Both read the credential
  snapshot the kernel captured at `connect()`, so PID reuse and `/proc`
  visibility cannot affect them. `/proc/<pid>/status` is never consulted.
  If either read fails, the connection is refused. `SO_PEERGROUPS` needs
  Linux 4.13 or later.
- **Group membership** counts as primary or supplementary.
- **JSON is never identity.** `created_by` is overwritten with the
  kernel uid; fields such as `admin`, `uid`, `groups` or `role` are ignored
  by authorization.
- Refusals return the typed error `permission_denied`. Unknown commands
  return `invalid_argument`.

The `getsockopt(SO_PEERGROUPS)` call lives in the small
`src/shared/flowerly-peercred` crate, the only workspace code with `unsafe`
(documented safety contract). `flowerly-core` keeps `#![forbid(unsafe_code)]`.

## Classes

- **User**: Flow, Goal, Session metadata, execution and provenance reads,
  Generation candidates, Compute reads, profiles, workloads and inference.
- **Admin**: system configuration, security-relevant records, shared
  catalogs and Generation activation. IPC `shutdown` is also Admin.
- **ServiceAccount**: commands that drive PTY shells. A shell runs as the
  service account, which owns `state.db`, so a shell grants full authority
  over all Flowerly state. Until shells can be launched as the requesting
  user, these commands are limited to peers running as the service
  account, **even for `flowerly-admin` members**. `session.terminate` stays
  User: terminating a live PTY still requires its control token.

## Configuration

| Setting | TOML | Environment | Effect |
| --- | --- | --- | --- |
| Users group | `users_group` | `AGENTIC_USERS_GROUP` | Socket is group-owned by it with mode 0660; members may connect. Unset: socket 0600, service account only. |
| Admin group | `admin_group` | `AGENTIC_ADMIN_GROUP` | Members may run Admin commands and IPC `shutdown`. Unset: refused for everyone. |

A value is a group name from `/etc/group` or a numeric gid. An unknown or
duplicated name, an invalid gid, or the same group for both settings stops
`flowerlyd` at startup. Groups served only through NSS (LDAP, SSSD) must be
configured by numeric gid. Status reports `admin_authorization`
(`linux_group` or `denied_unconfigured`) and `client_access`
(`service_account_only` or `service_account_and_linux_groups`).

A `flowerly-admin` member with a different UID also needs filesystem access
to the socket, so add administrators to `flowerly-users` as well.

## Installed system (systemd)

`distro/overlay` ships:

- `sysusers.d/flowerly.conf`: `g flowerly-users -` and `g flowerly-admin -`.
- `flowerlyd.service`: `User=flowerly`, `Group=flowerly-users`,
  `AGENTIC_USERS_GROUP=flowerly-users`, `AGENTIC_ADMIN_GROUP=flowerly-admin`,
  `RuntimeDirectoryMode=0750` (traversable by `flowerly-users`) and
  `StateDirectoryMode=0700` (state stays private to `flowerly`).

Grant access with `usermod -aG flowerly-users <user>` and, for
administrators, `usermod -aG flowerly-users,flowerly-admin <user>`. The
change applies to the user's **next login session** and their next IPC
connection.

### Upgrade

1. Install the new binaries, unit and `sysusers.d` file, then run
   `systemd-sysusers` (creates the two groups).
2. Add administrators to `flowerly-admin` (and `flowerly-users`).
3. `systemctl daemon-reload && systemctl restart flowerlyd`.
4. Check `agenticctl status flowerlyd`: `admin_authorization` must be
   `linux_group`.

Until step 2, Admin commands are refused. That is intended: nobody is an
administrator by default.

### Rollback

Reinstall the previous binaries and unit, then `daemon-reload` and restart.
The database schema is unchanged by this feature, so no data migration is
needed. The groups can stay; earlier releases ignore them.

### Recovery when no administrator can connect

Stop the service with `systemctl stop flowerlyd` (SIGTERM is unaffected by
IPC authorization), add a member to `flowerly-admin`, start it again.

## Development mode

A developer running `flowerlyd` as themselves is the service account: User
and PTY commands work as before, but Admin commands and IPC `shutdown` are
refused until an admin group is granted explicitly, for example
`AGENTIC_ADMIN_GROUP=$(id -g) ./scripts/dev-runtime.sh start`. The
black-box suites that use Admin commands pass `AGENTIC_ADMIN_GROUP` set to
the test user's own gid.

## Limits

- **No per-user data ownership.** All admitted peers share one Flowerly
  namespace. A `flowerly-users` member can list, complete or archive Flows,
  Goals and workloads created by another member. `created_by` records the
  real uid but is not yet an authorization boundary. Only add trusted
  users to `flowerly-users` until per-user ownership exists.
- **Connect-time snapshot.** Removing a user from a group takes effect on
  their next connection. Connections are bounded by the 10 s idle deadline.
- **Same-UID processes are trusted for signals.** Any process running as
  `flowerly` can SIGTERM the daemon or read `state.db` directly. The admin
  check stops IPC escalation, not same-UID filesystem access.
- **Other daemons.** `evolutiond` and `providerd` keep the same-UID-only
  socket and IPC `shutdown` for the service account.

## Tests

- `authorization.rs` unit tests: every Admin command is refused for the
  service account and for users-group members, and admitted for primary or
  supplementary admin-group members. Every ServiceAccount command is refused
  for other UIDs, including admins. No admin group means refused for all.
- `flowerly-core` tests: socket mode 0600 or 0660 by policy, peer groups
  delivered to the handler, IPC `shutdown` refused outside the admin group
  with spoofed JSON, and admitted for a member.
- `flowerly-peercred` tests: groups match the process, including the
  `ERANGE` buffer growth path; a non-socket descriptor is an error.
- `scripts/test-admin-authorization.py target/release`: the real supervisor
  refuses all six Admin commands and `shutdown` without a grant or with a
  foreign group, admits them with an explicit grant, and refuses to start
  with an unknown group.
- `--cross-uid` (CI, via sudo): real users and groups. A users member is
  limited to User commands. An admin member runs Admin commands and
  `shutdown` but cannot spawn a PTY. The service account is not an implicit
  admin. A non-member is refused by the kernel check even when the socket is
  made world-writable.

## Inventory

62 commands: 6 Admin, 10 ServiceAccount, 46 User.

| Command | Class | Reason |
| --- | --- | --- |
| `provenance.add` | Admin | arbitrary edges can forge the audit trail |
| `generation.activate` | Admin | changes the active system Generation |
| `model.register` | Admin | system model catalog |
| `model.retire` | Admin | system model catalog |
| `provider.register` | Admin | system provider registry |
| `usage.record` | Admin | writes the shared usage ledger directly |
| `session.spawn` | ServiceAccount | own PTY; guardian and cgroup checks; shell runs as the service account |
| `session.spawn.lookup` | ServiceAccount | own PTY reservation; shell runs as the service account |
| `session.attach` | ServiceAccount | PTY control token; shell runs as the service account |
| `session.detach` | ServiceAccount | PTY attachment token; shell runs as the service account |
| `session.heartbeat` | ServiceAccount | PTY attachment token; shell runs as the service account |
| `session.read` | ServiceAccount | PTY control token; shell runs as the service account |
| `session.follow` | ServiceAccount | PTY control token; shell runs as the service account |
| `session.write` | ServiceAccount | PTY attachment token; shell runs as the service account |
| `session.resize` | ServiceAccount | PTY attachment token; shell runs as the service account |
| `session.status` | ServiceAccount | PTY control token; shell runs as the service account |
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

Not routed by `flowerlyd`, so outside this table: `evolutiond`'s
`evolution.run_experiment` (execution disabled) and the `providerd` plugin
protocol. IPC `shutdown` is handled by the core runner and is Admin (above).
