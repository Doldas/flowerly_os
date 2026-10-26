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

## Object ownership (#225): partly enforced

> **The ownership boundary is NOT complete.** Flows, Goals, Sessions,
> Executions (PR 3), provenance and Status (PR 4) and Compute profiles,
> workloads, usage and inference (PR 5) are now owner-scoped (below).
> Generation listings are **not** owner-scoped yet (PR 6), so two
> `flowerly-users` members could still see each other's Generation
> candidates. The shipped configuration therefore still admits only the
> service account (see
> [the temporary restriction](#temporary-restriction-service-account-only-225)).
> `flowerlyd` refuses to start with a users group while any User command is
> pending. Status reports how many are pending in `ownership_pending_commands`.

### Enforced: Flows, Goals, Sessions, Executions, provenance, Status, Compute

Every User command is classified in `DATA_SCOPES` in
`src/flowerlyd/src/authorization.rs` as `OwnerScoped`, `SharedSystem`,
`Stateless` or `Pending("PR n …")`. A unit test keeps that list in step with
the command inventory. For the owner-scoped commands:

- Handlers reach storage only through the owner-scoped API
  (`src/shared/flowerly-core/src/storage/scoped.rs`, `CoreQueryRepository` /
  `CoreCommandRepository`). Every call takes the kernel-attested `Caller`
  explicitly. The shared connection has no global "current user". A source
  scan test fails if a handler calls an unscoped read or mutation.
- The owner filter is part of the SQL of the same statement or `IMMEDIATE`
  transaction as the mutation, so a concurrent change cannot slip between
  the check and the write.
- **Visible:** your own rows, plus the canonical General Flow for Flow
  commands and read-only `system` profiles for profile commands. Not
  visible: other users', `unresolved` and other `system` rows.
- **No existence oracle.** Another user's object is answered exactly like a
  missing one: `not_found` with the same message, for show, list, activate,
  archive, complete, abandon, child and terminate (`execution.show` returns
  `null`, as for a missing id). `permission_denied` is used only where it
  reveals nothing: archiving the shared General, or editing or renaming a
  `system` profile.
- **Cross-object references** must have the same owner, checked in the
  inserting transaction and enforced by 0010 triggers for any other writer:
  a child Goal's parent, an Execution's Goal, and a Session's Flow (which may
  also be General).
- **Foreground** is per uid; see the
  [Flow model](flow-model.md#activation-the-per-user-foreground-and-archiving).
- **Status** is fully caller-scoped (PR 4). It reports the caller's
  `foreground_flow` and the following counts:
  - Flows: own + General.
  - Goals, Sessions and Executions: own.
  - Generations and profiles: own + `system`.
  - Active workloads: own.
  - Provenance edges: visible as below.
  - Models and providers: the shared catalog counts.
- **Provenance (PR 4)** is filtered by **both endpoints** in one SQL
  statement. An endpoint is one of three kinds:
  - *Visible:* the caller's own Flow, Goal, Session, Execution, Generation,
    profile or workload; General; a `system` Generation or profile; a
    catalog model or provider; the caller's own `actor:uid:<n>`.
  - *Hidden:* any other row of those kinds, a missing one, or another uid's
    `actor:uid:<n>`.
  - *Neutral:* kinds with no owner yet (agents, experiments, findings, …)
    and other actor labels.

  An edge is shown only if no endpoint is hidden and at least one is
  visible, so neutral-to-neutral edges are never shown to ordinary users.
  Querying another uid's object returns no edges, exactly like a missing
  reference, and the Status count matches. Audit history is never deleted.
  `provenance.add` stays an audited Admin command.
- **Generation `reason_goal` (PR 4).** `generation.create` accepts only the
  caller's own Goal (`not_found` otherwise). This is checked in the inserting
  transaction and backed by a migration 0011 trigger. `generation.inspect`
  shows `reason_goal: null` when the Goal is not the caller's. Generation
  visibility itself (private candidates) is PR 6.
- **Compute (PR 5).** The model catalog and the provider registry stay
  shared system data (Admin-only mutation).
  - *Profiles* are the caller's own plus read-only `system` profiles.
    Names are unique **per owner** (migration 0012 replaces the global
    unique index), so creating or renaming a profile can no longer reveal
    that another user uses a name. A `base` resolves as its child's owner
    sees it: the owner's own profile of that name first, then a `system`
    one; a `system` profile resolves only `system` bases. Another user's
    profile can never enter a chain. `profile.show` reports `owner`.
  - *Workloads* are the caller's own only. `workload.submit` accepts only a
    profile the caller can see (backed by a migration 0012 trigger), and
    the concurrency ceiling counts the caller's own active workloads.
    `workload.list` lists only the caller's own: `mine: false` (formerly
    everyone's) is now `invalid_argument` instead of being silently
    narrowed. `workload.show`, `stop`, `complete` and `inference.request`
    answer another user's workload like a missing one.
  - *Usage* belongs to the owner of its workload (migration 0012 trigger).
    `workload.usage_brief` and the totals in `workload.show` count only the
    caller's own usage. `usage.record` stays an Admin command.
- **Admin membership never widens** these User commands.
- **Tests:** `storage::scoped` unit tests use real file-backed connections.
  They cover two uids, the same uid concurrently, General, the archive vs
  activate race, ownership transfer, delete, restart, and a failed or
  repeated migration. `scripts/test-user-ownership.py --cross-uid` (CI, root)
  turns the two-UID reproduction into regression assertions with real Linux
  users, including profiles, workloads and usage with a separate admin
  user that registers the model and records usage. On the pre-PR 3 daemon
  it fails on the status-count leak.

### Recorded (PR 2)

- Every owner-scoped table (`flows`, `goals`, `sessions`, `executions`,
  `generations`, `ai_profiles`, `ai_providers`, `inference_workloads`,
  `inference_usage`) has `owner_class` (`user`, `system` or `unresolved`)
  and `owner_uid`. `created_by` stays the immutable creator **audit text**
  and is never used to decide access.
- The Router passes the kernel-attested `Caller` (the connection's uid,
  never request JSON) through the worker queues to every handler. Each
  create path sets `owner = caller.owner()`:
  `flow.create`, `goal.create`/`goal.child`, `session.create`,
  `execution.create`, `generation.create` (candidates are owned by their
  creator), `profile.create`/`import`/`clone`, `workload.submit`.
  `usage.record` and `inference.request` copy the owner of the workload they
  account for. Providers and the General Flow are `system`; PTY Sessions are
  owned by the service account that spawned them.
- **Fail closed.** The column default is `unresolved`, so a write path that
  forgets an owner produces a row visible to nobody. A `CHECK` rejects
  inconsistent pairs (`user` without a uid, a uid without `user`, a uid
  outside `0..=4294967294`).
- **Backfill (migration 0009, one transaction).** Only a well-formed
  `uid:<decimal>` creator becomes a `user` owner. General is `system`;
  providers are `system`; `STABLE`/`SUPERSEDED` Generations are `system`.
  Everything else, including the legacy `user` label, malformed `uid:`
  values, existing Generation candidates and Executions without a resolvable
  Goal owner, is `unresolved` and stays protected. Nothing is reassigned to
  another uid. An admin claim procedure is a later #225 step.
- **Rollback.** There is no down-migration. Back up `state.db` before the
  upgrade and restore it together with the previous binaries. A restore is
  not an ownership operation: it discards everything written after the
  backup.

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
| Users group | `users_group` | `AGENTIC_USERS_GROUP` | Socket is group-owned by it with mode 0660; members may connect. Unset: socket 0600 and service account only, at both the filesystem and the kernel check. **Until #225 is closed, `flowerlyd` refuses to start when it is set** (see below). |
| Admin group | `admin_group` | `AGENTIC_ADMIN_GROUP` | Members may run Admin commands and IPC `shutdown`. Unset: refused for everyone. |

A value is a group name from `/etc/group` or a numeric gid. Until #225 is
closed, setting the users group makes `flowerlyd` refuse to start (see
[the temporary restriction](#temporary-restriction-service-account-only-225)). An unknown or
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
- `flowerlyd` itself **refuses to start** when a users group is configured,
  whether by environment, drop-in or config file, and names #225 in the
  error. This also stops a stale local drop-in on an upgraded system. The
  only exception is the exact test-only acknowledgment
  `AGENTIC_TEST_ONLY_UNENFORCED_OWNERSHIP=members-can-read-each-others-data`,
  which the cross-UID tests use. It logs a `TEST ONLY` warning. The shipped
  unit unsets it, and the offline admission check fails if any shipped file
  sets it. Never set it on a real system. It is removed when #225 closes.

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
3. Verify nothing re-enables multi-user admission (see below), then check
   status as the service account
   (`sudo runuser -u flowerly -- agenticctl --runtime-dir /run/flowerly status flowerlyd`).
   `admin_authorization` must be `linux_group` and `client_access` must be
   `service_account_only`.

Admin commands are refused by default; use the out-of-band procedure above.

**Upgrade check: no leftover multi-user admission.** An installation upgraded
from an earlier `release/2` may still carry a local override. Before and
after restarting, confirm:

```bash
systemctl cat flowerlyd            # no drop-in sets AGENTIC_USERS_GROUP / AGENTIC_CONFIG
systemctl show flowerlyd -p Environment -p UnsetEnvironment
ls -l /run/flowerly/flowerlyd.sock # srw------- flowerly flowerly
```

`client_access` in Status must read `service_account_only`. The shipped unit
also sets `UnsetEnvironment=AGENTIC_USERS_GROUP AGENTIC_CONFIG
AGENTIC_TEST_ONLY_UNENFORCED_OWNERSHIP`, so none of them can be injected
through the service manager's environment (`systemctl set-environment`,
`DefaultEnvironment=`). If a leftover drop-in still configures a users
group, `flowerlyd` fails to start with a #225 error. Remove the drop-in;
do not add the acknowledgment.

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
