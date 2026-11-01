# Command authorization

Status: **Linux group authorization and per-user data ownership are enforced
in `flowerlyd`** (#31, [#225](https://github.com/Doldas/flowerly_os/issues/225)).
In 0.0.4, multi-user access is:

1. **implemented**: every User command is owner-scoped by kernel-attested uid;
2. **tested**: cross-UID suites with real Linux users, and real systemd in
   the Arch guest, for the default, opt-in and disabled configurations;
3. **enabled by an administrator**, explicitly, with the maintained
   [multi-user drop-in](#opt-in-multi-user-access-225);
4. **not enabled by default**: the shipped unit is
   [service-account-only](#default-service-account-only).

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
apply only when a users group is configured, which only the opt-in drop-in
does. The service account row in `flowerly-admin` describes the check, not a
supported configuration: never add `flowerly` to `flowerly-admin`.

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

## Object ownership (#225): enforced

> **Every User command is owner-scoped** (PRs 3–6, below), and Status
> reports `ownership_pending_commands: 0`. The audited Admin
> `ownership.inspect/claim/release` (PR 7) handle legacy `unresolved` data.
> PR 8 makes multi-user admission available as an administrator opt-in
> (`MULTI_USER_ADMISSION_RESTORED`); the shipped default stays
> service-account-only. `flowerlyd` still **refuses to start with a users
> group while any User command is `Pending`**, so a future unscoped command
> fails closed even when the opt-in drop-in is installed. There is no
> override.

### Enforced: Flows, Goals, Sessions, Executions, provenance, Status, Compute, Generations

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
  shows `reason_goal: null` when the Goal is not the caller's.
- **Generations (PR 6).** `generation.list` and `generation.inspect` show
  the caller's own Generations plus `system` ones (the STABLE/SUPERSEDED
  history). Another uid's or an `unresolved` legacy candidate is `not_found`
  like a missing id. `generation.create` accepts only a visible `parent`,
  checked in the transaction and backed by a migration 0013 trigger.
  - **`generation.promote {id, reason}` (Admin, audited)** makes a
    `CANDIDATE` or `TESTED` private (or `unresolved`) Generation a `system`
    one. One `IMMEDIATE` transaction changes the owner and appends an
    `admin_audit` row: admin uid, command, object, previous and new owner,
    reason (1..512 bytes) and `applied`. It also writes a provenance edge to
    `actor:uid:<admin>`, which only that admin sees. Nothing is converted
    silently.
  - **Redaction contract.** A promoted Generation shares its id, status,
    parent, source commit, manifest, experiment and creation time.
    `reason_goal` is kept but shown only to the Goal's owner, and its
    provenance edge stays hidden from everyone else.
  - **`generation.activate`** activates only `system` Generations. A
    private candidate is `conflict` ("promote it first"), so it can never
    become the system STABLE directly.
  - **`admin_audit`** (migration 0013) is append-only: triggers refuse
    UPDATE and DELETE for every writer. Refusals are journaled
    (`flowerly::admin_audit`), not stored. PR 7 reuses the table.
- **Ownership administration (PR 7, Admin, audited).** These commands are
  the only way to cross or change ownership of user data. Each one needs a
  `reason` (1..512 bytes) and appends one `admin_audit` row per object, in
  the same `IMMEDIATE` transaction as the change. `ref` is a Flow, Goal,
  Session, Execution, Generation, profile, workload or usage id. Model and
  provider ids are `invalid_argument`, and a missing id is `not_found`.
  - **Ownership component.** Ownership moves as a unit. The unit is the
    object plus every object linked to it, transitively and in both
    directions, through a same-owner reference that has the same owner:
    Goal parent, Flow parent, Execution Goal, Session Flow, Generation
    reason Goal and parent, workload profile, and usage workload. `system`
    objects such as General bound the component and never move. A linked
    object with another non-system owner blocks the move unless it already
    has the target owner. Before commit, the transaction re-checks every
    reference of the moved objects.
  - **`ownership.inspect {ref, reason}`** returns `owner`, `created_by`,
    `component`, `foreign_links` and the object's earlier `audit` records.
    It returns ownership metadata only, never content. The read is audited
    with an unchanged owner.
  - **`ownership.claim {ref, to_uid, reason, expect}`** gives an
    `unresolved` component to `user:<to_uid>`. It does not check that
    `to_uid` exists.
  - **`ownership.release {ref, reason, expect}`** makes a user's component
    `unresolved` (protected) again. A foreground Flow among the moved
    objects falls back to General.
  - `expect` must equal the current component size, otherwise the command
    is `conflict` and nothing moves. The same applies to a blocking link,
    a non-movable owner (`system`, already claimed or already unresolved)
    and a per-owner profile-name clash. Components are capped at 10 000
    objects. A release followed by a claim restores the same component.
    Backup restore is not an ownership operation.
- **CLI.** A member of the admin group runs these as
  `agenticctl generation promote <id> --reason <text>`,
  `agenticctl ownership inspect <ref> --reason <text>`,
  `agenticctl ownership claim <ref> --to-uid <uid> --reason <text> --expect <n>`
  and `agenticctl ownership release <ref> --reason <text> --expect <n>`.
  Take `<n>` from the `component` list `ownership inspect` returns. The CLI
  only forwards the request; the daemon enforces every rule above. Anyone
  else gets `PermissionDenied` and a non-zero exit.
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
  user that registers the model and records usage, and an audited claim and
  release of a legacy `unresolved` Goal tree. On the pre-PR 3 daemon
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
| Users group | `users_group` | `AGENTIC_USERS_GROUP` | Socket is group-owned by it with mode 0660; members may connect. Unset: socket 0600 and service account only, at both the filesystem and the kernel check. `flowerlyd` refuses to start with it while any User command is not owner-scoped. |
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
  `RuntimeDirectoryMode=0700`, `StateDirectoryMode=0700`, `UMask=0077`,
  `UnsetEnvironment=AGENTIC_USERS_GROUP AGENTIC_CONFIG AGENTIC_DATABASE_PATH
  FLOWERLY_PTY_ISOLATION FLOWERLY_PTY_CGROUP_ROOT`.
- `/usr/share/flowerly/systemd/flowerlyd-multi-user.conf`: the maintained
  opt-in drop-in, **staged inactive**. Nothing under a `*.service.d`
  directory is shipped.

### Default: service-account-only

The 0.0.4 default admits **only the `flowerly` service account**:

- The socket is 0600 and `/run/flowerly` is 0700, both owned by `flowerly`.
  `state.db` is 0600 in a 0700 state directory.
- The kernel check also refuses every other UID, including `flowerly-admin`
  members, even if the modes were loosened.
- The service manager cannot enable admission: `UnsetEnvironment=` removes
  `AGENTIC_USERS_GROUP` and `AGENTIC_CONFIG` from `systemctl
  set-environment`, `DefaultEnvironment=` **and** any drop-in that only sets
  `Environment=AGENTIC_USERS_GROUP`. Only the complete drop-in below, which
  resets that list, opts in.
- The service manager cannot repoint the database either (#237).
  `AGENTIC_DATABASE_PATH` is unset in both configurations. `ProtectSystem=strict`
  alone does not prevent this: `/dev/shm`, `/run/flowerly` and any file under
  `/var/lib/flowerly` stay writable, and the daemon would silently start on an
  empty or volatile database. `flowerlyd` logs the effective database and warns
  when it is not `<state_dir>/state.db`. For tests or recovery against another
  database, run the binary directly as the service account, never through
  the manager environment.
- The service manager cannot weaken PTY containment (#240).
  `FLOWERLY_PTY_ISOLATION` and `FLOWERLY_PTY_CGROUP_ROOT` are unset in both
  configurations. The shipped unit has no `Delegate=`, so PTY spawning stays
  disabled (`PTY delegation unavailable; spawning disabled`). An injected
  `FLOWERLY_PTY_ISOLATION=development` would otherwise have spawned Sessions
  without cgroup containment. `flowerlyd` warns whenever development isolation
  is active.
- No socket unit, active drop-in, `EnvironmentFile`, config file or
  `SupplementaryGroups` is shipped. `scripts/test-shipped-ipc-admission.py`
  fails the build if one appears or if the default weakens.

**Operational impact.** Ordinary users cannot use `agenticctl` against the
system daemon, and Admin commands are unavailable: the service account is
not an administrator, and no other UID can connect. Administration needs the
opt-in below.

### Opt-in multi-user access (#225)

Enable it only after deciding that `flowerly-users` members should use the
system daemon. Each member then sees and changes only their own Flows, Goals,
Sessions, Executions, provenance, profiles, workloads, usage and Generation
candidates, plus the shared `system` objects (General, the catalogs, system
Generations and profiles).

**Enable** (as root):

```bash
install -D -m 0644 /usr/share/flowerly/systemd/flowerlyd-multi-user.conf \
    /etc/systemd/system/flowerlyd.service.d/50-multi-user.conf
systemctl daemon-reload && systemctl restart flowerlyd
usermod -aG flowerly-users alice        # each user, then log in again
```

The drop-in sets exactly:

| Directive | Value | Why |
| --- | --- | --- |
| `Group=` | `flowerly-users` | primary group: the socket is created group `flowerly-users`, and systemd creates `/run/flowerly` group `flowerly-users` |
| `RuntimeDirectoryMode=` | `0750` | members can traverse `/run/flowerly`, nobody else can |
| `UnsetEnvironment=` (reset), `UnsetEnvironment=AGENTIC_CONFIG AGENTIC_DATABASE_PATH FLOWERLY_PTY_ISOLATION FLOWERLY_PTY_CGROUP_ROOT` | | lets the drop-in's own users group through; a manager-injected config file, database path or PTY isolation override stays impossible |
| `Environment=` | `AGENTIC_USERS_GROUP=flowerly-users` | overrides any manager-injected value |

The state directory stays 0700 and `state.db` 0600 with `UMask=0077`. systemd
re-owns the state directory to group `flowerly-users`, but no group bits are
set, so it stays private to `flowerly`. Changing the socket mode alone is
not enough and is not supported: the runtime directory and the daemon's
users group must agree.

**Verify:**

```bash
systemctl show flowerlyd -p Group -p RuntimeDirectoryMode -p Environment -p UnsetEnvironment
ls -ld /run/flowerly /run/flowerly/flowerlyd.sock /var/lib/flowerly /var/lib/flowerly/state.db
# drwxr-x--- flowerly flowerly-users  /run/flowerly
# srw-rw---- flowerly flowerly-users  flowerlyd.sock
# drwx------ flowerly ...             /var/lib/flowerly
# -rw------- flowerly ...             state.db
agenticctl --runtime-dir /run/flowerly status flowerlyd   # as a member
```

Status must show `client_access: service_account_and_linux_groups` and
`ownership_pending_commands: 0`.

**Disable (rollback to service-account-only):**

```bash
rm /etc/systemd/system/flowerlyd.service.d/50-multi-user.conf
systemctl daemon-reload && systemctl restart flowerlyd
```

Data is kept. Members' objects stay theirs and become reachable again if the
drop-in is reinstalled. systemd re-owns the state directory to group
`flowerly`. If you downgrade `flowerlyd` below #225 PR 8, remove the drop-in
first: older binaries refuse to start with a users group.

### Administration

Use a real human account that is a member of **both** `flowerly-users` and
`flowerly-admin`, with the opt-in drop-in installed:

```bash
usermod -aG flowerly-users,flowerly-admin dana
agenticctl --runtime-dir /run/flowerly <admin command>   # as dana
```

- `flowerly-admin` alone does not admit a connection. `flowerly-users` alone
  does not authorize Admin commands.
- The kernel-attested uid of the human administrator appears in the
  `flowerlyd` journal record of every Admin decision (target
  `flowerly::admin_audit`: command, peer uid/gid/pid, `service_account`,
  outcome; never arguments), and as `actor_uid` in `admin_audit` rows for
  ownership operations.
- An administrator cannot obtain a PTY: ServiceAccount commands stay limited
  to the `flowerly` UID.
- Never add `flowerly` to `flowerly-admin` (sysusers, `usermod` or
  `SupplementaryGroups=`), and never give the daemon or the PTY guardians
  the admin group.

The temporary break-glass procedure used before 0.0.4 (running a client as
the service account with `runuser -G flowerly-admin`) is **no longer a
supported workflow**. Such a connection is journaled as the service account,
so it cannot identify the operator.

### Upgrade

1. Back up `state.db`. Install the new binaries, unit, sysusers file and
   `/usr/share/flowerly/systemd/flowerlyd-multi-user.conf`, then run
   `systemd-sysusers`.
2. `systemctl daemon-reload && systemctl restart flowerlyd`.
3. Check status as the service account
   (`sudo runuser -u flowerly -- agenticctl --runtime-dir /run/flowerly status flowerlyd`):
   `admin_authorization` must be `linux_group`, and `client_access` must be
   `service_account_only` unless you opted in.

**Upgrade check: no stale overrides.** An installation upgraded from an
earlier `release/2` may carry a local drop-in. `systemctl cat flowerlyd`
must show no drop-in other than `50-multi-user.conf`, and none may set
`AGENTIC_CONFIG`, `AGENTIC_DATABASE_PATH`, `FLOWERLY_PTY_*`, `EnvironmentFile=` or
`SupplementaryGroups=`. The former
test-only variable `AGENTIC_TEST_ONLY_UNENFORCED_OWNERSHIP` is no longer
read by `flowerlyd`; remove any drop-in that sets it.

### Rollback

Disable the opt-in first, then reinstall the previous binaries and unit and
restore the `state.db` backup taken before the upgrade, if the previous
binaries predate the ownership migrations (there is no down-migration). The
groups can stay.

### Recovery

`systemctl stop flowerlyd` is unaffected by IPC authorization (SIGTERM).
Never add `flowerly` to `flowerly-admin` to regain convenience: it runs PTY
shells and owns `state.db`.

## Development mode

A developer running `flowerlyd` as themselves is the service account: User
and PTY commands work as before, but Admin commands and IPC `shutdown` are
refused until an admin group is granted explicitly, for example
`AGENTIC_ADMIN_GROUP=$(id -g) ./scripts/dev-runtime.sh start`. The
black-box suites that use Admin commands pass `AGENTIC_ADMIN_GROUP` set to
the test user's own gid.

## Limits

- **PTY shells run as the service account.** With the opt-in drop-in the
  daemon's primary group, and therefore a PTY shell's, is `flowerly-users`.
  PTY commands stay limited to the service account, and shells keep the
  0077 umask. Until shells run as the requesting user, no other UID gets a
  shell.
- **Same-UID trust.** A process running as `flowerly` can read `state.db`
  directly and so bypasses per-user ownership. Ownership protects users
  from each other, not from the service account.
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
  refuses every Admin command and `shutdown` without a grant or with a
  foreign group, admits them with an explicit grant, refuses to start with
  an unknown group, and starts with an owner-scoped users group.
- `--cross-uid` (CI and the Arch guest, via sudo): real users and groups.
  A users member is limited to User commands. A human administrator (users
  and admin groups) runs Admin commands and `shutdown`, is journaled by
  their own uid, and cannot spawn a PTY. An admin-only member and a
  non-member are refused by the kernel check even when the socket is made
  world-writable. The service account is not an implicit admin. It then
  starts `flowerlyd` exactly as the shipped unit configures it, alone and
  merged with the opt-in drop-in, and checks admission, socket and runtime
  modes and state privacy for both.
- `scripts/test-systemd-multi-user.py` (root, disposable host only: the Arch
  guest of every release push or dispatch, and hosted PR runners): real
  systemd. It installs the shipped files, starts `flowerlyd` with
  `systemctl`, and checks:
  - the **default**, also with an injected manager environment
    (`AGENTIC_USERS_GROUP`, `AGENTIC_CONFIG`, `AGENTIC_DATABASE_PATH` into
    `/dev/shm`, `FLOWERLY_PTY_ISOLATION=development`, the removed test-only
    variable) and a stale partial drop-in. The daemon must keep
    `/var/lib/flowerly/state.db` open and PTY spawning disabled, never
    uncontained;
  - the **opt-in** drop-in installed as documented, running the complete
    two-user ownership suite against the real socket, including restart
    persistence and audited Admin operations by a human administrator;
  - **disabling** it again, with data intact.
- Admin audit: both test modes assert an `admin command` journal record for
  every Admin decision and `shutdown`, refused or not, and that arguments
  (including forged identity values) are never logged.
- `scripts/test-shipped-ipc-admission.py` (offline, `docs` job and
  preflight): the shipped unit, sysusers and overlay admit only the service
  account. Each known bypass (users group, `EnvironmentFile`,
  `SupplementaryGroups`, group-owned socket, traversable runtime directory,
  readable state, loose umask, config file, `m flowerly …`, a `.socket` unit
  or an active drop-in) is rejected. The drop-in, merged with systemd's
  rules, must give exactly the opt-in policy, and each weakening of it is
  rejected.

## Inventory

66 commands: 10 Admin, 10 ServiceAccount, 46 User.

| Command | Class | Reason |
| --- | --- | --- |
| `provenance.add` | Admin | arbitrary edges can forge the audit trail |
| `generation.activate` | Admin | changes the active system Generation |
| `generation.promote` | Admin | audited: makes a private candidate a shared system Generation |
| `ownership.inspect` | Admin | audited: reads another owner's ownership metadata |
| `ownership.claim` | Admin | audited: gives unresolved data to a user |
| `ownership.release` | Admin | audited: makes a user's data unresolved (protected) |
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
| `inference.result` | User | own remote attempt |
| `inference.consent_grant` | User | own consent |
| `inference.consent_revoke` | User | own consent |
| `inference.consent_list` | User | own consent |

Not routed by `flowerlyd`, so outside this table: `evolutiond`'s
`evolution.run_experiment` (execution disabled) and the `providerd` plugin
protocol. IPC `shutdown` is handled by the core runner and is Admin (above).
