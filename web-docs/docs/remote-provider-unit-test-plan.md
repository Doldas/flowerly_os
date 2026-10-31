# Remote Provider Unit Test Plan (Arch Linux/KVM)

Plan and harness for accepting the **OS security boundary** of the remote
provider unit `flowerly-provider-remote@.service` (ADR-0022, #263).

| Item | Status |
| --- | --- |
| Socket access architecture | **ACCEPTED** by the owner, with the limitations below (architecture only) |
| PR #268 | **DRAFT, NOT MERGED** |
| KVM harness | **PREPARED** (offline-validated; never executed) |
| KVM acceptance | **NOT RUN** |
| OS confinement | **NOT ACCEPTED** |
| Operational deployment | **NOT AUTHORIZED** |
| Live inference | **NOT AUTHORIZED** |
| Release | **NOT AUTHORIZED** |

The first live test (#268) ran providerd as the owner's user without this
unit, so it accepted the adapter only. The offline tests
(`scripts/test-remote-provider-operator.py`,
`scripts/test-remote-provider-kvm-harness.py`, the providerd unit tests)
passing is **not** this acceptance and must never be reported as it: they
check the unit file, the operator logic, the harness and the evidence
rules, never real kernel credentials, real systemd confinement or an Arch
guest.

## Purpose

Show, on a real Arch Linux kernel and systemd, that the unit:
- confines providerd (identity, capabilities, seccomp, namespaces,
  filesystem, devices, socket families, loopback and link-local egress);
- exposes only its own credential;
- admits socket peers by kernel identity exactly as ADR-0022 says;
- and that providerd's application policy (address, TLS, redirect, size,
  parsing, outcome) holds inside the unit against a hostile mock; and
- that the operator procedure installs, recovers and rolls back safely
  under real root, real accounts and real systemd.

## Phases

| Phase | Command | Effect |
| --- | --- | --- |
| Preparation | `scripts/remote-provider-arch-kvm-acceptance.sh --plan` | prints the plan, commit, tree state, image-lock state |
| Preparation | `... --validate [--evidence-out FILE]` | syntax and configuration checks; can only write a `NOT_RUN` document |
| Preparation | `... --dry-run` | `--validate` plus the exact commands an execution would run, none executed |
| Execution | `... --execute --approved-sha <sha>` | builds, verifies the image, boots one disposable KVM guest, runs the guest checks |
| Evidence | (end of `--execute`) | copies the guest's evidence out, validates it, destroys the guest |

The preparation modes never start QEMU, download, build, use the network,
use `sudo` or change the host. The offline test runs them behind command
shims that fail on any such call. `--execute` has **never** been run.

## Prerequisites (execution only)

- An x86_64 Linux host with a usable `/dev/kvm` for the invoking,
  **non-root** user. There is no TCG fallback: without KVM it refuses.
- Host tools: `git python3 cargo curl gpg sha256sum qemu-img
  qemu-system-x86_64 cloud-localds ssh scp ssh-keygen tar`. Record their
  versions (`qemu-system-x86_64 --version`) with the evidence.
- Crates fetched beforehand (`cargo fetch --locked`, a separate step): the
  build runs with `--offline`.
- A pinned image in `testbed/remote-provider-kvm/arch-image.json` and a
  keyring file holding exactly its signing key
  (`FLOWERLY_KVM_ARCH_KEYRING`).
- Host resources: 4 vCPUs, 4 GiB RAM, a 16 GiB sparse overlay.
- Guest: the pinned Arch Linux cloud image as published, **not upgraded**
  (the guest has no network). It must provide systemd with
  `systemd-creds`, Python 3, OpenSSL, util-linux (`setpriv`, `runuser`,
  `flock`), shadow (`gpasswd`), p11-kit (`trust`), iproute2 and OpenSSH.
  The guest kernel and systemd version are recorded, not chosen.
- The host-built `providerd` and `flowerlyd` must run on the guest's glibc.
  If they do not, every check that needs them is INCONCLUSIVE.

## Image provenance

`arch-image.json` is **UNPINNED**. Nobody has resolved, downloaded or
verified an image for this plan, and no value may be invented. Pinning is
its own reviewed commit:

1. Choose a versioned directory under
   `https://geo.mirror.pkgbuild.com/images/` (`v<YYYYMMDD>.<build>/`),
   never `latest/`.
2. Record `version`, the exact `url` of
   `Arch-Linux-x86_64-cloudimg-<version>.qcow2`, `signature_url`
   (`url` + `.sig`), the published SHA-256 and the 40-hex fingerprint of the
   signing key. Take the fingerprint from Arch Linux's published
   release-engineering keys, cross-checked through a second channel, not
   from the mirror alone.
3. Record `pinned_by` and `pinned_at_utc`.

At execution the harness:
- downloads only that URL into its cache;
- checks the SHA-256 and the signature against a keyring that must hold
  exactly the pinned key;
- never falls back to another image, URL or kernel.

The image name and SHA-256 go into every evidence record. The guest
reports its kernel (`uname -r`) and `systemctl --version`. The cached
image may be reused across runs: it is read-only behind a per-run overlay.
Delete the cache directory to clean it up.

## Network isolation

**No live egress.** The guest's only NIC is QEMU user networking with
`restrict=on`: it cannot reach the host or the internet. The only
forwarding is inbound SSH on `127.0.0.1:<random port>`. The guest also
checks this itself (E2).

**Where the mock lives.** All destinations are on a dummy interface
(`flowerly-mock0`) **inside the guest**
(`testbed/remote-provider-kvm/network.json`):

| Role | Address | providerd default policy | Unit (OS) |
| --- | --- | --- | --- |
| mock HTTPS server | `192.88.99.10:8443` | allowed (public) | not blocked |
| redirect target (sink) | `192.88.99.11:8443` | allowed | not blocked |
| RFC 1918 sink | `10.77.0.10` | refused | not blocked |
| CGNAT sink | `100.77.0.10` | refused | not blocked |
| ULA sink | `fd77:77:77::10` | refused | not blocked |
| link-local sinks | `169.254.77.10`, `fe80::77` | refused | blocked |
| metadata sink | `169.254.169.254:80` | refused | blocked |
| loopback | `127.0.0.1`, `::1` | refused | blocked |
| DNS stub (mock) | `127.0.0.53:53` | n/a (resolver) | allowed by the operator drop-in |

**The address-policy conflict, and how it is resolved.** The earlier
draft of this plan put the mock "on an address reachable from the guest".
On a guest-local loopback or private address, providerd's production
policy refuses that, correctly. It must not be weakened, and no test-only
bypass may be added.

The harness therefore gives the mock an address the **default production
policy classifies as public**: `192.88.99.0/24`, the deprecated 6to4 relay
anycast block (RFC 7526). That address lives only on the guest's dummy
interface. The guest has no route out, so the address never leaves it.
The positive path (B1, B2) then runs:
- the unmodified binary, unit, operator-rendered config and resolver;
- with `allow_loopback = false` and an empty private allowlist.

Two offline checks pin this choice to the real code:
- the providerd unit test
  `kvm_acceptance_network_plan_matches_the_production_address_policy` fails
  if `AddressPolicy::default()` stops treating every role as `network.json`
  expects;
- `scripts/test-remote-provider-kvm-harness.py` checks the operator's DNS
  classifier the same way.

If a future policy refuses `192.88.99.0/24`, both fail, and the harness
must be re-planned rather than the policy relaxed. The administrator
exception `private_destination_cidrs` is a reviewed production feature,
but it is deliberately **not** used here: the default configuration is
the one under test.

**DNS.** The mock answers `*.flowerly.test` (RFC 6761 reserved) with TTL 0
and NXDOMAIN for every other name. The scenario chooses the answer: public,
private, CGNAT, loopback, link-local, metadata or ULA. Rebinding is a fixed
sequence: the first N A answers are public, every later one private, and
AAAA queries do not advance it. `systemd-resolved` is stopped and masked
and `nsswitch.conf` uses `files dns`, so nothing caches. No test depends
on public DNS or an external server.

## Dummy credential and test PKI

- **Credential.** `dummy_credential()` generates a random
  `flowerly-kvm-dummy-key-<hex>` in the guest runner's memory and pipes it
  into the guest's `systemd-creds encrypt`. It is never written in
  plaintext. The mock receives only its SHA-256 and logs whether the bearer
  token matched, never the token. No host credential, key or file enters
  the guest; the bundle is the repository export, two binaries and
  metadata.
- **PKI.** `make-test-pki.sh` creates a fresh test CA, an untrusted CA
  and leaf certificates in the guest for each run, all labelled
  `FLOWERLY TEST ONLY - NOT FOR PRODUCTION`. The CA keys are deleted after
  signing. The test CA is trusted in that guest only.

## Execution guardrails

`--execute` refuses, before any build, download or VM, unless:
- `FLOWERLY_KVM_ACCEPTANCE=execute-in-disposable-guest` is set;
- `--approved-sha` is a full SHA equal to `HEAD`;
- the tree is clean, with no untracked files;
- the caller is not root and not a CI runner;
- the image is pinned, the keyring holds exactly the pinned key, and
  `/dev/kvm` and all host tools are usable.

Each run gets a unique id, its own work directory, overlay, cloud-init
seed, SSH key and marker. The guest runner refuses unless it is root in a
KVM guest whose marker file and hostname match this run, and the bundle
matches its manifest. Accounts, groups, units, DNS and trust-store changes
therefore happen only in that guest.

**These checks are safety guardrails against accidents, not proof that
the owner approved a run.** Approval is recorded as described under
[Authorization](#authorization-required-before-execution).

## Checks

Every check has an id in `testbed/remote-provider-kvm/checks.json`; the
evidence needs exactly one record per id. "Harness" checks validate the
test environment; all others exercise production providerd, the unit or
the operator.

### Environment (harness)

- **E1** QEMU runs with `-accel kvm`; there is no TCG fallback.
- **E2** QEMU uses `restrict=on`. In the guest, connections to the host
  gateway, slirp DNS and an external address all fail.
- **E3** The image matches the pinned SHA-256 and signature.
- **E4** The guest runner found this run's marker, KVM, hostname and
  manifest.

### Part A: confinement probe inside the real unit

The guest runner first runs `scripts/provider-unit-probe.py` as root
**outside** the unit (`--control-out`). That run must reach every
listener and see every path. It also records which paths ordinary
permissions leave open (`dac_open`). For the duration of the probe, the
runner adds `o+x` to `/var/lib/flowerly` and `/run/flowerly`, so
world-readable decoys inside them are open to any user. A runtime drop-in
then replaces **only** `ExecStart=`:

```ini
# /run/systemd/system/flowerly-provider-remote@probe.service.d/90-probe.conf
[Service]
Type=oneshot
Restart=no
ExecStart=
ExecStart=/usr/bin/python3 -I /usr/local/share/flowerly-kvm/provider-unit-probe.py --targets ... --control ...
StandardOutput=truncate:/var/lib/flowerly-kvm/probe-unit.json
```

Outcome classes: `blocked_by_os` (EPERM/EACCES, or SIGSYS for a forked
attempt the seccomp filter killed, EAFNOSUPPORT for a family, EROFS for a
write), `hidden_by_os` (ENOENT where the control run proved the
path exists), `connection_refused`, `timed_out`, `allowed` and
`inconclusive`. `refused_by_application` is reserved for providerd's own
refusals in Part B. A refusal, timeout, missing route or uncontrolled
denial is **never** counted as an OS block. Any required check that is
violated or inconclusive makes the probe exit nonzero.

- **A1** uid and gid (real, effective, saved, fs) are the provider
  account's, with no supplementary group.
- **A2** All capability sets are zero.
- **A3** `NoNewPrivs: 1` and `Seccomp: 2`.
- **A4** Creating a user, mount, net, ipc, uts, pid or cgroup namespace is
  refused (each tried in a forked child).
- **A5** Flowerly state is denied, including a world-readable decoy: an
  OS-level denial must be proven on at least one `dac_open` path.
- **A6** Denied: `/etc/credstore*` (including another credential), shadow
  files, `/root` (including an SSH key), `/home` (including another user's
  world-readable file), the operator ledger and the evidence directory.
- **A7** `$CREDENTIALS_DIRECTORY` holds exactly `probe.key`. The test
  checks its size, never its contents. Its encrypted source is denied.
- **A8** No writes to `/etc`, `/usr/local`, `/var/lib`, `/run` or another
  user's home. A write to its own runtime directory is recorded for
  information.
- **A9** `setuid(0)`, `/proc/1/environ`, `/proc/1/fd`, `/proc/sys` writes,
  `/dev/kvm` and `/dev/mem` are refused.
- **A10** Loopback v4/v6 connects end in `blocked_by_os`, with a live
  listener proven by the control run.
- **A11** Link-local v4/v6 and metadata connects end in `blocked_by_os`,
  with live listeners.
- **A12** `AF_PACKET`, `AF_NETLINK`, raw ICMP, `AF_VSOCK`, `AF_ALG` and
  `AF_BLUETOOTH` sockets are refused.
- **A13** The loopback DNS stub allowed by the operator drop-in is
  reachable.
- **A14** RFC 1918, CGNAT, ULA and the public mock are **reachable** from
  the unit. This records that only the application refuses them. If the
  OS blocked them, the documented limitation would be wrong and the check
  fails for review.
- **A15** With `flowerly-provider` added to an extra group, the **real**
  unit fails at startup (`supplementary group` in its journal) and
  `install` of a second instance refuses.

`systemd-analyze security` for the instance is recorded with the run
facts.

### Part B: real providerd in the unit, against the mock

The real `ExecStart` runs, and the unit is restarted for each scenario.
The scenario client (`guest/provider-client.py`) runs as
`flowerly-provider`. Its prompts carry `FLOWERLY-SENTINEL-PROMPT`, and the
mock answers sentinel prompts with `FLOWERLY-SENTINEL-REPLY`, so any leak
is detectable. "Sink untouched" means the mock logged no connection to
that address.

- **B1** `remote-provider-operator.sh preflight` succeeds. Discovery runs
  over verified TLS to `192.88.99.10` with the default policy, and no chat
  request is sent.
- **B2** `accept` sends exactly one chat request with no tools, and the
  operator evidence passes the leak scan.
- **B3** RFC 1918, **B4** CGNAT, **B5** loopback, **B6** link-local,
  **B7** metadata, **B8** ULA: when the name resolves there, the result is
  `address_refused`/`not_sent`, and neither that sink nor the mock sees a
  connection.
- **B9** Rebinding: discovery sees the public answer; the chat's new
  connection resolves privately and fails as `address_refused`. The
  private sink is untouched.
- **B10** Certificate from an untrusted CA, and **B11** certificate for the
  wrong name: `tls`/`not_sent`, with no HTTP request completed.
- **B12** A `302` to another host fails as `redirect_refused`; the target
  sink is untouched.
- **B13** `401` gives `auth_failed`. The next submit is refused as
  `not_sent` with no request.
- **B14** `429` gives `rate_limited` with `retry_after_ms` 7000.
- **B15** `503` gives `server_error` with outcome `unknown`, and exactly
  one chat request (no automatic retry).
- **B16** Model not listed: `model_unavailable`/`not_sent`, with no chat
  request.
- **B17** A different model in the reply gives `model_mismatch`.
- **B18** A reply over 1 MiB gives `invalid_response`.
- **B19** Malformed JSON gives `invalid_response`.
- **B20** `tool_calls` in the reply give `unsupported_response`.
- **B21** Missing usage succeeds, with usage `unknown` and null counts,
  never 0.
- **B22** A body dropped after `200` gives `transport` with outcome
  `unknown`.
- **B23** A 5 s delay (below the timeout) succeeds, with latency ≥ 5000 ms.
- **B24** A repeated request id with the same arguments produces one chat
  request. Different arguments give `request_id_reused`.
- **B25** A discovery-only drop-in makes `provider.submit` fail as
  `discover_only`/`not_sent`, with no chat request.
- **B26** The unit's journal and every evidence file contain no sentinel
  prompt, dummy key, bearer header, authorization header, private key or
  reply text.
- **B27** flowerlyd with `AGENTIC_USERS_GROUP=flowerly-provider-clients`
  and an enabled gate logs remote inference disabled ("would admit the
  provider process"), never enabled.

### Socket authentication (SO_PEERCRED + SO_PEERGROUPS)

The provider admits a peer by the identity the kernel reports for the
connecting process, read once at accept: `SO_PEERCRED` (uid, primary gid,
pid) plus `SO_PEERGROUPS` (supplementary groups). It never uses
`/etc/group`, the socket mode alone or anything the client sends. A peer
counts as admitted only if the provider's handler answers (an
`unknown_request` for an id nobody submitted). Clients run under
`setpriv --reuid --regid --groups|--clear-groups` or `runuser`, and record
their own `Uid:`/`Gid:`/`Groups:`.

| Id | Connecting process | Expected |
| --- | --- | --- |
| **S1** | `flowerly` with the clients group effective | admitted |
| **S2** | `flowerly` started **before** being added to the group in `/etc/group` | refused: kernel groups decide |
| **S3** | ordinary user, no group | refused at `connect` (0660 socket) |
| **S4** | unrelated user with effective membership via `setpriv --groups` | admitted: effective membership is the boundary |
| **S5** | root **without** the group | refused at the peer check (not at the file mode) |
| **S6** | root **with** the group | admitted; root is trusted by the threat model |
| **S7** | `flowerly-provider` (service account) | admitted |

- **S8** The same request id submitted by a second uid is a different
  job (`created_by` is the kernel uid).
- **S9** Another uid's `poll` and `cancel` fail as `unknown_request`; the
  owner's job is untouched.
- **S10** After flowerlyd gets `SupplementaryGroups=` and is restarted,
  `/proc/<MainPID>/status` `Groups:` includes the clients group, and not
  before the restart (a `daemon-reload` alone is not enough).

**Group membership is not consent.** Effective membership of
`flowerly-provider-clients` authenticates a **process** to the provider
socket. It proves nothing about which human asked for remote inference or
whether they consented. Per-user consent, the administrator gate and the
profile gate are enforced **only in flowerlyd**, against the uid of
flowerlyd's own peer (covered by `scripts/test-remote-inference-gates.py`,
not here). Any admitted process (flowerlyd, or root with the group) can
spend the key without per-user consent. The accepted architecture is
**not** independently validated until S1–S10 have actually run.

### Part C: lifecycle under real root, accounts and systemd

The guest takes a snapshot of accounts, groups and the relevant
directories before `install`. Power cuts are simulated by a guest-only
interposer (`guest/interpose.sh`): it runs the real command, then sends
`SIGKILL` to the operator at a fixed call number.

- **C1** `install` starts and enables nothing.
- **C2** With the credential moved aside, the unit fails to start and no
  connection is made.
- **C3** An invalid configuration (an `http://` base) is refused before any
  change.
- **C4** Rolling back one of two instances keeps the other and every
  shared resource.
- **C5** While a `flowerly-provider` process runs, or `flowerly` is a group
  member, nothing shared changes.
- **C6** Partial rollback keeps what it could not remove; the rerun after
  restoring completes.
- **C7** `install` killed right after `systemd-sysusers`: the documented
  recovery never deletes the pending accounts.
- **C8** `rollback` killed after its first file removal: the rerun
  completes.
- **C9** A foreign binary identical to the shipped one survives
  install/rollback (same inode).
- **C10** A recorded path replaced by a symlink is kept, and its target is
  untouched.
- **C11** A modified instance file is kept, its ledger entry stays, and
  `preflight` refuses.
- **C12** A corrupt ledger refuses with no change.
- **C13** A second command refuses on the operator lock with no change.
- **C14** `disable`: the unit is masked and cannot start, and the gate is
  moved aside.
- **C15** Shared resources are removed by `rollback-shared` only after the
  final consumer is gone.
- **C16** After the final rollback, the guest matches the pre-install
  snapshot.

Offline tests of the same invariants (scratch roots, fake accounts) are
useful but do not prove that real host cleanup is safe; Part C is the only
real-root evidence.

## Pass and fail criteria

- The acceptance **passes** only if the evidence document validates with
  `--phase execution` and `summary.overall` is `PASS`. That requires all
  72 checks to be `PASS` at one commit, from a clean tree, with the image,
  kernel, systemd and binary/unit hashes recorded.
- Any `FAIL` fails the run.
- Any `INCONCLUSIVE` (harness error, unreachable control target, binary
  that does not run) or `NOT_RUN` means **not accepted**. Fix the harness
  and rerun; never edit evidence.
- A refused or timed-out connection never counts as "blocked". Only
  `blocked_by_os` with a live control target does.

## Evidence

Schema: `testbed/remote-provider-kvm/evidence.schema.json`.
Rules: `scripts/remote-provider-kvm-evidence.py`.

Each record carries `schema_version`, `test_suite`, `test_id`,
`exercises`, `git_commit`, `git_tree_clean`, `guest_image`,
`guest_image_sha256`, `guest_kernel`, `systemd_version`,
`providerd_sha256`, `unit_sha256`, `test_started_at_utc`,
`test_finished_at_utc`, `test_status` (`PASS`, `FAIL`, `INCONCLUSIVE`,
`NOT_RUN`), `expected`, `observed`, `classification`, `skip_reason` and
`artifacts`.

The validator rejects:
- `PASS` outside an `execution` document, or without facts or a clean
  tree;
- `NOT_RUN` without a reason;
- missing or duplicate checks, or a summary that disagrees with the
  records;
- anything that looks like a bearer or authorization header, an API key,
  a private key, the sentinel prompt or reply, or the dummy credential.

If the guest's document fails validation, the guest writes `*.INVALID.json`
with the problems instead of the document. The preparation modes can only
produce `NOT_RUN`.

The run directory `target/remote-provider-kvm/<run-id>/report/` keeps:
- the evidence document and its validation result;
- `guest.log`, `serial.log` and `qemu-args.txt`;
- the image signature and its verification output.

## Teardown

On exit, success or failure, the harness:
- kills QEMU;
- deletes the overlay, the cloud-init seed and user-data, the run's SSH
  key, the export and the bundle;
- keeps only `report/`.

The guest, with every account, group, credential and trust anchor it
created, ceases to exist. The cached base image is untouched.

## Independent reproduction

1. Check out the recorded `git_commit` in a clean tree, with the same
   pinned `arch-image.json`.
2. Run `--validate`, then `--execute --approved-sha <commit>` under the
   recorded authorization.
3. Compare per-check status and classification, `guest_image_sha256`,
   `providerd_sha256` and `unit_sha256`. Timings and run ids differ; nothing
   else should.
4. At minimum, rerun Part A and the socket matrix from the same SHA.

## Known limitations

- **No OS-level destination allowlist.** The unit blocks only localhost,
  link-local and multicast. Other destinations rely on providerd's
  resolver and address policy (A14 records this). Pinning
  `IPAddressAllow=` to the destination is not possible for an anycast
  host.
- **Supplementary groups.** The plan assumes `Group=` yields exactly one
  group. A1 and A15 test it; if the guest shows otherwise,
  `--require-isolated-identity` fails closed and the unit needs adjusting.
- **Membership of `flowerly-provider-clients` is authority** to spend the
  key without per-user consent. It is checked at install, preflight and
  accept, but not continuously, and it is never evidence of consent.
- **Root is trusted.** The operator lock serializes this tool's commands.
  Another root process could still race a check and a removal.
- **Mock, not the real service.** The guest proves policy and confinement
  against a hostile mock, not interoperability with the real server
  (adapter-level live evidence is separate, #268).
- **Unupgraded image.** The guest runs the pinned image's own kernel and
  systemd, which may be older than a current Arch install. Results apply
  to the recorded versions.
- **Host-built binaries** must run on the guest's glibc. Operator evidence
  inside the guest reports `git_commit: unknown` (the bundle is an export
  without `.git`); the harness evidence carries the commit and the hashes
  instead.
- The guest orchestration (`guest-acceptance.py`) has passed only static
  and offline bookkeeping tests. Its first execution may hit harness
  defects. Those surface as `INCONCLUSIVE`, never as `PASS`.

## Authorization required before execution

Execution needs a **separate, explicit owner authorization** naming:
1. the exact commit SHA to run (and that its CI is green);
2. the pinned image (version, URL, SHA-256, signing-key fingerprint), as
   reviewed in its own commit;
3. the host that runs it (non-root user with `/dev/kvm`), and that it may
   download that one image and build the commit offline;
4. that the guest may create and delete accounts, groups, units, DNS
   configuration and a trust anchor inside itself only;
5. where the evidence goes (a PR comment and/or an issue).

That authorization does **not** cover merging #268, installing or starting
the unit on any real host, enabling remote inference, live egress,
contacting the real server, using the real key, tagging or releasing.
Accepting the OS boundary after a passing run is a further owner decision.
