# Command authorization

Status: **Linux group authorization enforced in `flowerlyd`** (#31). Not yet
verified on a real Arch VM boot. Per-user data ownership between different
users is **not** implemented, so the shipped configuration is temporarily
**service-account-only** (see
[Temporary restriction](#temporary-restriction-service-account-only-225)
and [#225](https://github.com/Doldas/flowerly_os/issues/225)).

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
| `flowerly-users` and `flowerly-admin` member (other UID) | yes | yes | yes | **no** |
| `flowerly-admin` member only (other UID) | **no** | — | — | — |
| Anyone else, including root | **no** | — | — | — |

Admission is decided by the service account or users group alone. Admin
group membership authorizes Admin commands for a peer that is already
admitted; it never admits a peer by itself (#225). The rows for other UIDs
apply only when a users group is configured, which the shipped unit
currently does not do.

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
| Users group | `users_group` | `AGENTIC_USERS_GROUP` | Socket is group-owned by it with mode 0660; members may connect. Unset: socket 0600 and service account only, at both the filesystem and the kernel check. **Leave unset until #225 is closed.** |
| Admin group | `admin_group` | `AGENTIC_ADMIN_GROUP` | Members may run Admin commands and IPC `shutdown`. Unset: refused for everyone. |

A value is a group name from `/etc/group` or a numeric gid. An unknown or
duplicated name, an invalid gid, or the same group for both settings stops
`flowerlyd` at startup. Groups served only through NSS (LDAP, SSSD) must be
configured by numeric gid. Status reports `admin_authorization`
(`linux_group` or `denied_unconfigured`) and `client_access`
(`service_account_only` or `service_account_and_linux_groups`).

A `flowerly-admin` member with a different UID must also be in the users
group to connect.

## Installed system (systemd)

`distro/overlay` ships:

- `sysusers.d/flowerly.conf`: `g flowerly-users -` and `g flowerly-admin -`,
  with no membership lines. The `flowerly` service account is never a member
  of `flowerly-admin`.
- `flowerlyd.service`: `User=flowerly`, `Group=flowerly`,
  `AGENTIC_ADMIN_GROUP=flowerly-admin`, **no** `AGENTIC_USERS_GROUP`,
  `RuntimeDirectoryMode=0700`, `StateDirectoryMode=0700`, `UMask=0077`.

### Temporary restriction: service-account-only (#225)

Different users admitted through `flowerly-users` could read and change each
other's Flows, Goals, Sessions and profiles (reproduced with two real UIDs
on #225). Until per-user ownership is enforced and verified, the shipped
unit admits **only the `flowerly` service account**:

- The socket is 0600 and `/run/flowerly` is 0700, both owned by `flowerly`.
- The kernel check also refuses every other UID, including `flowerly-admin`
  members, even if the modes were loosened.
- No socket unit, drop-in, `EnvironmentFile`, config file or
  `SupplementaryGroups` is shipped that could re-enable admission;
  `scripts/test-shipped-ipc-admission.py` fails the build if one appears.
- **Do not** re-enable `AGENTIC_USERS_GROUP` with a local drop-in before
  #225 is closed.

**Operational impact.** Ordinary users cannot use `agenticctl` against the
system daemon. Admin commands are refused by default, because the service
account is not an administrator. The group-based implementation stays in
place and is restored after #225.

**Break-glass administration (temporary, root only).** Owner-approved on
2026-10-25 for authorized maintenance only, while multi-user admission is
disabled. A privileged local administrator runs **one** client process as
the service account, with the admin group added to that process only:

```bash
sudo runuser -u flowerly -g flowerly -G flowerly-admin -- \
    agenticctl --runtime-dir /run/flowerly <admin command>
```

Rules:

- **Who may use it.** Only an already privileged local administrator, for
  authorized maintenance. It is not an ordinary-user path and not remote
  access.
- **No persistent privilege.** Never add `flowerly` to `flowerly-admin`
  (sysusers, `usermod` or `SupplementaryGroups=`). Never give the daemon or
  PTY guardians the admin group. The extra group exists only in the invoked
  client process and ends with it.
- **Identity seen by Flowerly.** IPC sees the effective service-account UID
  (`flowerly`), not the human administrator. Flowerly's own records,
  including `created_by` and provenance, therefore **cannot identify the
  original operator**.
- **Required audit trail.** Run the command through `sudo` as a named
  administrator, never from a shared root shell. `sudo` records the invoking
  user, time and full command line in the system journal
  (`journalctl _COMM=sudo`).
  - `flowerlyd` writes one `admin command` record per Admin decision
    (target `flowerly::admin_audit`) with command, peer uid/gid/pid,
    `service_account` and outcome (`denied`, `succeeded`, `failed`, or
    `accepted` for `shutdown`). Arguments are never logged.
  - Correlate the two records by time and command. Both together are the
    audit trail; neither alone is.
- **Risks.**
  - Any process running as `flowerly` with that extra group is an
    administrator for that connection.
  - A mistyped command runs with full Admin authority.
  - The journal must be protected and retained by the host's policy.
- **Lifetime.** Remove or redesign this procedure when proper
  administrative access and per-user isolation land (#225).

The `--cross-uid` CI test exercises the procedure and checks that the
daemon records it as the service account, with refusals and outcomes
journaled.

### Upgrade

1. Install the new binaries, unit and `sysusers.d` file, then run
   `systemd-sysusers` (creates the two groups).
2. `systemctl daemon-reload && systemctl restart flowerlyd`.
3. Check status as the service account
   (`sudo runuser -u flowerly -- agenticctl --runtime-dir /run/flowerly status flowerlyd`).
   `admin_authorization` must be `linux_group` and `client_access` must be
   `service_account_only`.

Admin commands are refused by default; use the out-of-band procedure above.

### Rollback

Reinstall the previous binaries and unit, then `daemon-reload` and restart.
The database schema is unchanged by this feature, so no data migration is
needed. The groups can stay; earlier releases ignore them.

### Recovery

`systemctl stop flowerlyd` is unaffected by IPC authorization (SIGTERM). For
Admin commands, use the out-of-band procedure. Never add `flowerly` to
`flowerly-admin` to regain convenience: it runs PTY shells and owns
`state.db`.

## Development mode

A developer running `flowerlyd` as themselves is the service account: User
and PTY commands work as before, but Admin commands and IPC `shutdown` are
refused until an admin group is granted explicitly, for example
`AGENTIC_ADMIN_GROUP=$(id -g) ./scripts/dev-runtime.sh start`. The
black-box suites that use Admin commands pass `AGENTIC_ADMIN_GROUP` set to
the test user's own gid.

## Limits

- **No per-user data ownership** ([#225](https://github.com/Doldas/flowerly_os/issues/225),
  0.0.4 P0 blocker). All admitted peers share one Flowerly namespace: a
  `flowerly-users` member can list, complete or archive another member's
  Flows, Goals and workloads. That is why the shipped unit is
  service-account-only for now.
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
  made world-writable. It then starts `flowerlyd` exactly as the shipped
  unit configures it. Under that configuration:
  - a users member and an other-UID admin are refused at the filesystem
    and, with the modes opened, by the kernel check;
  - the service account is not an administrator;
  - the out-of-band procedure admits Admin commands.
- Admin audit: both test modes assert an `admin command` journal record for
  every Admin decision and `shutdown`, refused or not, and that arguments
  (including forged identity values) are never logged.
- `scripts/test-shipped-ipc-admission.py` (offline, `docs` job and
  preflight): the shipped unit, sysusers and overlay admit only the service
  account. Each known bypass (users group, `EnvironmentFile`,
  `SupplementaryGroups`, group-owned socket, traversable runtime directory,
  config file, `m flowerly …`, a `.socket` unit or a drop-in) is rejected.

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
