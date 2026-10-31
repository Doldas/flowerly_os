# Remote Inference Operations

Operator procedure for one remote inference provider instance under
[ADR-0022](adr/0022-remote-inference-egress-policy.md) (#263). Nothing here
runs in CI, ships in an image or is enabled by default.

!!! warning "Owner approval"
    `preflight` contacts the destination for model discovery only. `accept`
    sends one real prompt. Run either only with the owner's explicit
    approval for that host and that step. Neither enables flowerlyd's remote
    gates: no Flowerly user can reach the provider through this procedure.

All steps use `scripts/remote-provider-operator.sh` from a clean checkout of
the reviewed commit. The evidence records that commit and whether the tree
was clean.

## What gets installed

`install` renders these files from `distro/templates/remote-inference/` and
installs the provider binary. It starts and enables nothing. Every
decision is made before the first change. `install` refuses when:
- a shared file exists that this tool did not install and that differs
  from what it would install;
- a shared file is this tool's but comes from a different build;
- the socket group has a member other than `flowerly`;
- the provider account is a member of any other group.

What it created is recorded in a root-only ledger,
`/var/lib/flowerly-provider-operator/`. Rollback removes only what the
ledger proves `install` created, even after an interruption. If another
installation creates an identical file or the same account in the
meantime, rollback still leaves it alone:

- **Files** are created in five steps:
  1. a `pending` ledger entry is written;
  2. a staged copy `.<name>.flowerly-new` is written next to the target;
  3. the staged copy is hard-linked to the target, which fails if the
     target exists, so nothing is ever replaced;
  4. the entry becomes `file`;
  5. the staged copy is removed.

  A `file` entry proves the link succeeded. For a `pending` entry, the
  target counts as created by this tool only if it is the **same inode**
  as the staged copy. Nobody else can produce that, so an identical file
  of another installation is always kept.
- **Accounts and groups** that did not exist before are recorded
  `pending-user` or `pending-group` before `systemd-sysusers` runs, and
  confirmed after it. An account whose entry is still pending after an
  interruption cannot be told apart from one someone else created
  meanwhile. Rollback therefore **reports it and never deletes it**. The
  operator checks that it is unused and removes it by hand. That window
  lies between `systemd-sysusers` and the confirmation.

Every subcommand except `render` holds one exclusive lock,
`/run/flowerly-provider-operator.lock` (`flock`), for its whole run. A
second command refuses at once and changes nothing. The kernel releases
the lock when the process ends, so an interrupted command never leaves a
stale lock. `disable` is the exception: it masks the unit **before** taking
the lock, because an emergency stop must not wait for a running command
and masking only restricts. It then waits for the lock to move the gate
policy aside.

Every directory on a path the tool writes or removes, from `/` down, must
be a real directory, not a symlink, owned by root and not writable by
group or others. Otherwise the command refuses before its first change.
Only root can then redirect a path between a check and the change. The
lock serializes this tool's own commands; other root processes stay
trusted.

| Path | Content |
| --- | --- |
| `/etc/sysusers.d/flowerly-provider.conf` | `flowerly-provider` account and the `flowerly-provider-clients` socket group (owner Q1) |
| `/etc/systemd/system/flowerly-provider-remote@.service` | the confined unit template, unchanged |
| `/etc/systemd/system/flowerly-provider-remote@<instance>.service.d/10-operator.conf` | binary path; `IPAddressAllow=` for a loopback DNS stub only |
| `/etc/flowerly/providers/<instance>.service.toml` | socket name, runtime directory, client group |
| `/etc/flowerly/providers/<instance>.toml` | destination, API style, one allowlisted model, limits; no key |
| `/usr/local/libexec/flowerly/providerd` | the release binary built from the checkout |
| `/usr/local/libexec/flowerly/remote-provider-acceptance.py` | the acceptance client |

The unit runs `providerd --require-isolated-identity` as `flowerly-provider`.
It refuses to start as uid or gid 0, or with any supplementary group
besides its primary one. It has no capabilities, a read-only
system, no home directories, no access to `/var/lib/flowerly`,
`/run/flowerly` or the credential stores, and only `AF_UNIX`, `AF_INET` and
`AF_INET6` sockets. `IPAddressDeny=` blocks localhost, link-local and
multicast. If the host's resolver is a loopback stub (systemd-resolved at
127.0.0.53), the drop-in allows that one address so DNS works; the
provider's own address policy still refuses every loopback destination.
The offline contract test keeps the unit's `systemd-analyze security`
exposure at or below 2.0.

## 1. Install the key

Use a dedicated, least-privilege key that can be revoked on its own. Type it
at the prompt; never pass it as an argument, paste it into a chat, issue or
PR, or store it in a file:

```sh
sudo systemd-creds encrypt --name=<instance>.key - /etc/credstore.encrypted/<instance>.key
```

Only systemd can decrypt it, into the unit's `$CREDENTIALS_DIRECTORY`. The
operator script checks that the file exists and never reads it.

## 2. Install the instance

```sh
cargo build --release -p providerd
sudo scripts/remote-provider-operator.sh install <instance> https://<host> <openai|open-webui> <model>
```

`install` refuses to overwrite existing files. It validates the instance
name, the `https://host[:port]` origin, the API style and the model id.

## 3. Discovery-only preflight

```sh
sudo scripts/remote-provider-operator.sh preflight <instance>
```

1. Resolves the destination again and requires every address to be public
   (owner Q2). A private, loopback, link-local, NAT64 or documentation
   address stops the procedure before the unit starts.
2. Starts the unit with a runtime-only drop-in adding
   `providerd --discover-only`. In that mode `provider.submit` is refused
   (`permission_denied`, class `discover_only`, outcome `not_sent`) before
   anything is sent, so no prompt can leave the host.
3. Runs `provider.discover`, which reports only the allowlisted models
   the server lists.
4. Stops the unit, removes the drop-in and writes the evidence.

## 4. Single fixed-prompt acceptance

Only after the owner approves this step for this host:

```sh
sudo scripts/remote-provider-operator.sh accept <instance> <approval-reference>
```

The approval reference, for example the URL of the owner's approving
comment, is recorded in the evidence as `approval_reference`. It is the
**operator's attestation** that an approval exists. Nothing verifies it,
and it is not cryptographic proof that the owner authorized the request:
anyone with root can type any reference. Authorization rests on the
owner's approval in the issue or PR it points to. A reviewer checks that
the reference resolves to such an approval for this host, this step and
this commit. The step re-checks DNS, starts the
unit and checks discovery again. Only if the model is served and the
instance is not in discovery-only mode does it submit exactly one fixed
prompt:

| Field | Value |
| --- | --- |
| prompt | `Reply with exactly: flowerly-ready` |
| `max_tokens` | 16 |
| poll window | 150 s; a request still pending is reported, not cancelled or retried |

The client reports the state, outcome, finish reason, usage (reported or
unknown), latency, whether the text matched and its length, and whether a
second poll confirms the provider released the text. It never prints the
text. The unit is stopped afterwards, so the instance never stays up.

`max_tokens` is a **request to the server, not an enforced cap**. In the
first live test (2026-10-30, Open WebUI 0.11.4, `gpt-oss:20b`), 16 were
requested and the server reported 44 completion tokens, while the visible
reply was 14 bytes. Reasoning tokens are probably counted, but the
server's behavior is not documented here.

The provider reports `max_tokens_requested` and `max_tokens_exceeded`
(`null` when usage is unknown) with every result. Flowerly makes **no claim
of a remote token or spending cap**: only its own bounds hold (1 MiB
response body, 32 KiB text, request timeout). A cost limit would need
server-side enforcement and further evidence. Any second request needs a
new approval.

## 5. Evidence

Each step writes one JSON file under `/var/log/flowerly-provider-acceptance/`
(root, 0750). It holds the issue, step, instance, approval reference,
time, commit and tree state, systemd version, unit exposure, SHA-256 of the
binary, client, unit, drop-in and config, and the step reports.

The unit's journal for the step is scanned for the prompt, a bearer
header or an `sk-` token. The journal is not copied. If any of these is
found, or a report carries text, no evidence file is written and the step
fails, so it can be investigated. The evidence is designed to be attached
to the PR as-is.

## Emergency disable

```sh
sudo scripts/remote-provider-operator.sh disable <instance>
```

Stops and masks the unit, so nothing can start it. If an administrator
enabled flowerlyd's gate, the gate policy `/etc/flowerly/remote-providers.toml`
is moved aside, which disables remote inference, and flowerlyd is
restarted if running. To also invalidate the key, revoke it on the server.

## Rollback

```sh
sudo scripts/remote-provider-operator.sh rollback <instance>
sudo scripts/remote-provider-operator.sh rollback-shared   # only if asked to retry
```

Rollback removes only what the ledger says `install` created, so it cannot
remove anything belonging to another instance or installation.

1. **Validate before changing anything.** It checks the whole ledger and
   every directory on the paths it names (see above). An instance it did
   not install, a ledger naming any unexpected path or account, or a
   symlinked or writable directory stops it before anything changes.
2. **Instance files.** It removes this instance's drop-in and
   configuration, but **only if unmodified**. It removes this instance's
   socket and lock file, never following a symlink, and never the shared
   runtime directory while it is not empty.
3. **Partial rollback is recoverable.** Each removed file leaves the
   ledger at once, through an atomic rewrite. A file that cannot be
   removed (edited, replaced by a symlink) is kept, **stays in the
   ledger**, and the command exits non-zero. While any entry remains:
   - shared resources are not touched;
   - the instance cannot be reinstalled, preflighted or accepted;
   - rerunning `rollback` is safe and changes nothing more.

   Restore the file's installed content, or remove it yourself, then rerun
   `rollback <instance>`.
4. **Shared resources: every usage check first.** The shared unit
   template, binary, client, sysusers file, account and groups are
   considered only after the instance ledger is gone. Before **any** of
   them is changed, all of these checks run:
   - another instance of this tool;
   - a provider configuration from another installation;
   - an active `flowerly-provider-remote@` unit;
   - a process running as `flowerly-provider`;
   - a process executing the shared binary, whoever runs it;
   - a member other than the provider of a group this tool created (for
     example `flowerly` once flowerlyd's gates are enabled);
   - **any check that cannot complete** (`pgrep`, `systemctl list-units`
     or `/proc` failing), which counts as "in use".

   If any check finds a user, **nothing shared is changed** and the shared
   ledger stays as it was. Other installations are expected, so rollback
   exits 0. Anything else exits non-zero; run `rollback-shared` once it is
   gone. Only when every check passes are the shared items removed, each
   only if this tool created it and it is unmodified. Each leaves the
   ledger as it goes.

**After an interruption** (power loss, kill) at any point:
1. If `/var/lib/flowerly-provider-operator/instances/<instance>` exists,
   rerun `rollback <instance>`.
2. Then rerun `rollback-shared` if `/var/lib/flowerly-provider-operator/shared`
   exists.

The offline contract test interrupts install and rollback after every
single change they make and checks two things:

- **Recovery:** the steps above restore the pre-install state byte for
  byte and delete each account and group at most once. Where an account
  is reported instead of deleted, the test removes it by hand as the
  operator would, then compares.
- **Other installations are spared:** after each interrupted install,
  another installation creates byte-identical files and the same
  accounts wherever this install had not. The test checks that recovery
  removes or changes **none** of them.

The encrypted key stays for the operator to remove:
`sudo rm /etc/credstore.encrypted/<instance>.key`.

## Identity and group membership

| Principal | Groups | Can reach |
| --- | --- | --- |
| `flowerly-provider` (provider) | primary `flowerly-provider-clients` only; `--require-isolated-identity` refuses any other | its own credential, its runtime directory, the network through its address policy |
| `flowerly` (flowerlyd) | joins `flowerly-provider-clients` only when gates are enabled | the provider socket; its own state |
| any other member of `flowerly-provider-clients` | **must not exist**; `install` refuses | would spend the remote key without consent |

- The provider cannot read `/var/lib/flowerly` (0700 `flowerly`, and
  `InaccessiblePaths=` in the unit) or `/run/flowerly`. It is not in
  flowerlyd's users or admin group. flowerlyd **disables remote
  inference** if either group is `flowerly-provider-clients`, since that
  would admit the provider to flowerlyd.
- On the provider socket, the core runner replaces `created_by` with the
  peer's kernel uid. A second group member is therefore a different
  actor: it cannot poll, cancel or read flowerlyd's jobs (tested).
- Membership of the socket group is still the authority to spend the
  key. Never add a human user to it.

### Socket access model

**Status: architecture accepted by the owner on 2026-10-30, with the
limitations below** ([ADR-0022](adr/0022-remote-inference-egress-policy.md#socket-access-architecture-accepted-2026-10-30-architecture-only)).

This is **architecture acceptance only**:
- the OS security boundary and the unit's confinement are **not**
  accepted;
- operational use, deployment and release are **not** accepted.

Real Arch Linux/KVM security-boundary validation is required before
operational acceptance. Until then the model is the design the
implementation enforces, not a validated boundary.

1. **Provider-level authority.** Any process that the kernel reports at
   `connect` as uid `flowerly-provider`, or as holding
   `flowerly-provider-clients` among its effective groups
   (`SO_PEERCRED` plus `SO_PEERGROUPS`), may make the provider spend the
   remote key. That includes root while it holds the group.
2. **No consent at the socket.** Group membership authenticates a
   process, not a user, and is not evidence of any user's consent.
3. **Per-user consent only in flowerlyd.** It is checked against the
   uid of flowerlyd's own kernel-attested peer. So are the administrator
   gate and the profile gate. All three exist only in flowerlyd.
4. **Who holds the group.** The only intended member is `flowerly`,
   once gates are enabled. The provider refuses any extra group of its
   own (`--require-isolated-identity`). `install`, `preflight` and
   `accept` refuse any other member, but nothing re-checks membership
   continuously.
5. **Not protected against root.** A root process can join the group
   and spend the key. Root is outside this model's protection.

## Not covered

- Enabling flowerlyd's gates (`--remote-providers`, the flowerlyd drop-in,
  adding `flowerly` to the socket group, consent grants). That is a
  separate step needing its own approval.
- Pinning `IPAddressAllow=` to the destination's addresses. The operator's
  host is anycast, so pinning would break when its addresses change.
- **Arch/KVM isolation acceptance has not been run.** It is separate from,
  and not implied by, the offline tests. The offline contract test checks
  the unit with the host's `systemd-analyze` and the operator logic
  against scratch roots without root. It does not show that the
  confinement holds in a real guest. The plan for that run is
  [Remote Provider Unit Test Plan](remote-provider-unit-test-plan.md).

### Arch/KVM acceptance harness (prepared, not run)

| Item | Status |
| --- | --- |
| KVM harness | PREPARED: offline-validated only |
| KVM acceptance | NOT RUN |
| OS confinement | NOT ACCEPTED |
| Operational deployment, live inference, release | NOT AUTHORIZED |

The harness is `scripts/remote-provider-arch-kvm-acceptance.sh`. Its guest
side lives in `testbed/remote-provider-kvm/`.

Preparation commands, safe on any machine:

```sh
scripts/remote-provider-arch-kvm-acceptance.sh --plan      # what would run; image-lock state
scripts/remote-provider-arch-kvm-acceptance.sh --validate  # syntax, configuration, evidence rules
scripts/remote-provider-arch-kvm-acceptance.sh --dry-run   # plus the exact commands, none executed
```

They never start a VM, download, build, use the network or `sudo`, and
they only ever report `NOT_RUN`.

Execution (`--execute --approved-sha <sha>` with
`FLOWERLY_KVM_ACCEPTANCE=execute-in-disposable-guest`) refuses on:
- a wrong SHA or a dirty tree;
- root or a CI runner;
- an unpinned or half-pinned image lock;
- a missing keyring or `/dev/kvm`.

Those refusals are guardrails, not evidence of approval. A run needs its
own owner authorization, described in the plan's
[Authorization](remote-provider-unit-test-plan.md#authorization-required-before-execution)
section.

The guest has no route out. The mock server, its DNS and the dummy
credential exist only inside it. The real server and the real key are
never involved.

A run counts only through its bound record: `verify-bound` on
`acceptance.json` must return `PASS`. A guest evidence document alone, a
preparation document or a partial run never does.
- OS-boundary acceptance. The first live test (#268) ran providerd as the
  owner's user without the unit, so it accepted the adapter only.

## Residual risks

Known and accepted by none of the tests above:

- **Remote tokens.** `max_tokens` is not a remote token or spending cap:
  the server exceeded it in the live test.
- **Destinations.** There is no OS-level allowlist; destinations other
  than localhost, link-local and multicast rely on providerd's resolver
  and address policy.
- **Socket group.** Membership is authority to spend the key without
  per-user consent (see the socket access model). It is checked at
  install, preflight and accept, not continuously.
- **Root.** Root is trusted. The operator lock serializes only this
  tool's commands. Another root process could change a file between the
  tool's check and its removal, or join the socket group.
- **Accounts after an interrupted install.** If install is interrupted
  between `systemd-sysusers` and the ledger confirmation, the accounts
  are reported for manual removal, not removed.
- **Approval reference.** It is an operator attestation, not verified
  proof of the owner's authorization.
- **Not yet run.**
  - Arch/KVM isolation acceptance (unit confinement, socket
    authentication with real credentials, effective process groups,
    lifecycle) has not been run. Its harness is prepared; the image is
    not pinned.
  - flowerlyd's gates have never been enabled against a real provider.
  - The only live evidence is one adapter-level request made outside the
    unit.
