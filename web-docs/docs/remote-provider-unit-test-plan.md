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
- Host tools: `git python3 cargo curl gpg qemu-img
  qemu-system-x86_64 cloud-localds ssh scp ssh-keygen tar`. Record their
  versions (`qemu-system-x86_64 --version`) with the evidence.
- Crates fetched beforehand (`cargo fetch --locked`, a separate step): the
  build runs with `--offline`.
- A pinned image in `testbed/remote-provider-kvm/arch-image.json` and a
  public keyring file (`FLOWERLY_KVM_ARCH_KEYRING`) holding exactly the
  pinned primary key with its approved signing subkeys, and nothing else.
- Host resources: 4 vCPUs, 4 GiB RAM, a 16 GiB sparse overlay.
- Guest: the pinned Arch Linux cloud image as published, **not upgraded**
  (the guest has no network). It must provide systemd with
  `systemd-creds`, Python 3, OpenSSL, util-linux (`setpriv`, `runuser`,
  `flock`), shadow (`gpasswd`), p11-kit (`trust`), iproute2 and OpenSSH.
  The guest kernel and systemd version are recorded, not chosen.
- The host-built `providerd` and `flowerlyd` must run on the guest's glibc.
  If they do not, every check that needs them is INCONCLUSIVE.

## Image provenance

`arch-image.json` pins Arch Linux cloud image `20261015.611680` (see
[Pinned image](#pinned-image) below). The pin records verified provenance
only: it is not an operational security assessment, and the Arch/KVM
acceptance is still `NOT_RUN`. No value may be invented. Changing the pin
is its own reviewed commit:

1. Choose a versioned directory under
   `https://geo.mirror.pkgbuild.com/images/` (`v<YYYYMMDD>.<build>/`),
   never `latest/`.
2. Record `version`, the exact `url` of
   `Arch-Linux-x86_64-cloudimg-<version>.qcow2`, `signature_url`
   (`url` + `.sig`) and the SHA-256 from the signed checksum file.
3. Record the signing key as Arch publishes it: `primary_key_fingerprint`
   (the 40-hex primary) and `signing_subkey_fingerprints` (the subkeys
   approved to sign images). Arch's arch-boxes key is a certify-only
   primary with a signing subkey, so the signer is the subkey. Take both
   from Arch's own publication, cross-checked through a second channel,
   never from the mirror alone.
4. Record `pinned_by` and `pinned_at_utc` (the reviewer and the time of the
   approving commit, never filled in on anyone's behalf).

At execution the harness:
- before any download, checks with `scripts/arch-image-verify.py keyring`
  that the keyring, imported into a throwaway GnuPG home, holds exactly one
  primary key, the pinned one, usable, with every approved signing subkey
  usable and signing-capable; other subkeys of that primary are allowed,
  other primary keys and secret key material are not;
- downloads only the pinned URL into its cache;
- verifies with `scripts/arch-image-verify.py verify`: the pinned SHA-256,
  and the detached signature from GnuPG's machine-readable status
  (`--status-file`), never its human-readable output or exit status alone:
  exactly one signature, one `GOODSIG` and one well-formed `VALIDSIG` whose
  signer is an approved subkey and whose primary is the pinned primary, a
  binary-document signature with a SHA-2/SHA-3 hash, not dated in the
  future. Any `BADSIG`, `ERRSIG`, `EXPSIG`, `EXPKEYSIG`, `REVKEYSIG`,
  `KEYEXPIRED`, `KEYREVOKED`, `SIGEXPIRED`, `NO_PUBKEY` or `FAILURE`
  rejects it. Trust comes from the lock, not from GnuPG's trust database,
  a keyserver or a successful `gpg --verify`;
- records the primary and signing-subkey fingerprints separately, with the
  digests and any problems, in `report/image-verification.json` and the
  host facts (never key material);
- never falls back to another image, URL or kernel.

An expired or revoked key fails closed, even for a signature made while it
was valid: re-pin to a newer image, or refresh the key from Arch, in a
reviewed commit.

### Pinned image

Resolved on 2026-10-31. The image was downloaded and verified once under a
limited owner authorization; the evidence was independently reviewed (the
reviewer re-verified the signed checksum metadata, not the image bytes).
`pinned_by` and `pinned_at_utc` in `arch-image.json` record the approval.

| Field | Pinned value |
| --- | --- |
| `version` | `20261015.611680` (newest versioned release on 2026-10-31) |
| `url` | `https://geo.mirror.pkgbuild.com/images/v20261015.611680/Arch-Linux-x86_64-cloudimg-20261015.611680.qcow2` (576 680 960 bytes) |
| `sha256` | `08754e28d91b1680086c6211ef4df13559e766fff7cdb4006489b14b5d281f08` |
| `signature_url` | `url` + `.sig` |
| `primary_key_fingerprint` | `1B9A16984A4E8CB448712D2AE0B78BF4326C6F8F` (`arch-boxes <arch-boxes@archlinux.org>`, ed25519, certify only, created 2022-05-29) |
| `signing_subkey_fingerprints` | `["656E4C5AC1CC3B86E539D97E343635A6859A9174"]` (ed25519, sign, **expires 2027-05-28**) |

Verified (observed cryptographic facts):
- The downloaded image is 576 680 960 bytes with that SHA-256, and its
  detached signature `….qcow2.sig` verifies over those bytes with
  `scripts/arch-image-verify.py` (signer `656E…9174`, primary `1B9A…6F8F`).
- The rest of this list was first established from metadata:
- The checksum file `….qcow2.SHA256` names that SHA-256. Its detached
  signature `….qcow2.SHA256.sig` verifies with
  `scripts/arch-image-verify.py` (VALIDSIG signer `656E…9174`, primary
  `1B9A…6F8F`, signed 2026-10-15 12:07:03 UTC).
- The image signature `….qcow2.sig` is an EdDSA/SHA-512 signature by
  issuer `656E…9174`, made at the same time.
- The checksum file and image signature are byte-identical on
  `geo.`, `mirror.` and `fastly.mirror.pkgbuild.com`.
- Key sources: the arch-boxes README on Arch's GitLab
  (`gitlab.archlinux.org/archlinux/arch-boxes`, "Every release is signed by
  our CI with the following key") and keyserver.ubuntu.com give the same
  primary and subkey. The GitHub copy of the README is a mirror of the same
  repository, not an independent channel.

Not established (open; the pin does not resolve them):
- An independent trust root: the key is not in `archlinux-keyring`, not
  published by WKD, and no certification by an Arch master or developer
  key was found. The Arch infrastructure repository's copy
  (`misc/keys/arch-boxes.asc`) was not obtained (404 on `master`). Trust
  currently rests on TLS to Arch's GitLab plus the matching keyserver copy;
  a reviewer should re-derive the fingerprints from Arch's GitLab
  themselves.
- The exact build recipe: arch-boxes `master` build scripts were read, not
  the commit of pipeline 611680.
- The guest's package versions. Arch's dated archive for 2026-10-15
  suggests linux 7.2.9, systemd 262, glibc 2.44 and python 3.14.8, but the
  image was built from a live mirror, so the archive does not prove what
  the image contains. The run records the kernel and systemd version.
- The subkey expires 2027-05-28; after that this pin fails closed.
- Any OS isolation property: that needs the Arch/KVM acceptance, which is
  `NOT_RUN`.

The image name and SHA-256 go into every evidence record. The guest
reports its kernel (`uname -r`) and `systemctl --version`. The cached
image may be reused across runs: it is read-only behind a per-run overlay.
Delete the cache directory to clean it up.

## Network isolation

**No live egress.** The guest's only NIC is QEMU user networking with
`restrict=on`: it cannot reach the host or the internet. The only
forwarding is inbound SSH on `127.0.0.1:<random port>`. E2 proves this
from evidence on both sides of QEMU (see [E2](#e2-egress-isolation)),
never from unanswered probes alone. After E2, the guest also deletes its
default routes and checks that every mock address is a `local` route, so
nothing addressed to the mock can be forwarded.

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
policy classifies as public**: `192.88.99.10`, from `192.88.99.0/24`. That
address lives only on the guest's dummy interface. The kernel delivers it
locally, and the guest has no route out, so the address never leaves it.

**What `192.88.99.0/24` is.**
- RFC 3068 (2001) assigned it as the 6to4 relay anycast prefix, with
  `192.88.99.1` as the relay address. Third parties announced it in
  global routing to run public 6to4 relays.
- RFC 7526 (2015, BCP 196) deprecated that anycast mechanism. It asked
  IANA to record the prefix as deprecated and not to reassign it except by
  a future IETF standards action. In the IANA IPv4 special-purpose address
  registry it is listed as "Deprecated (6to4 Relay Anycast)". Reviewers
  should check that entry's current attribute columns (source,
  destination, forwardable, globally reachable) directly; this plan does
  not restate them from memory.
- It is therefore neither private nor documentation space, and no current
  service is expected to be assigned there.

**Why production accepts it.** providerd's `net.rs` refuses:
- loopback, unspecified, broadcast and multicast;
- link-local;
- documentation (`192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`);
- `0.0.0.0/8`, `240.0.0.0/4`, the benchmarking range `198.18.0.0/15` and
  `192.0.0.0/24`;
- private RFC 1918 and CGNAT `100.64.0.0/10`, unless the administrator
  pins an exception.

`192.88.99.0/24` is in none of these lists, so the default policy treats
it as public, like any other unicast address. Python's `ipaddress` agrees
(`is_global` is true); the operator's DNS check uses that.

**Security ambiguity, stated.** The harness now depends on production
*not* refusing a deprecated special-purpose block. Refusing it would be a
reasonable production hardening, since no legitimate inference server
should be there, and the harness must not argue against that. If such a
change lands, the Rust test fails, and the harness moves to alternative B
below; the policy is never relaxed for the test. For independent review,
the offline alternatives are:

| Option | Positive path runs | Cost |
| --- | --- | --- |
| A (current) | the default policy, unmodified config | relies on `192.88.99.0/24` staying accepted |
| B | the reviewed production exception `private_destination_cidrs = ["10.77.0.10/32"]`, mock on that private address | tests the exception rather than the default; needs an instance rendered with that field (a new operator option or a second, hand-installed instance) |
| C | an ordinary global unicast address on the dummy interface | the address belongs to someone else; harmless offline, but semantically worse than a block that will not be reassigned |

The recommendation is A now. Whether production should refuse deprecated
special-purpose blocks is a separate question for review; it is not
changed here.
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
- the image is pinned, the keyring holds exactly the pinned primary key
  with its approved signing subkeys, and `/dev/kvm` and all host tools are
  usable.

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

The guest decides only E4. For E1, E2 and E3 it records its half as
`INCONCLUSIVE` (`awaiting_host`), or as `FAIL` when its own observation
already disproves the claim. The host then finalizes those three from
facts only it can see (see [Evidence](#evidence)). The guest alone can
never PASS them.

- **E1** In the guest, `systemd-detect-virt` reports `kvm` (TCG reports
  `qemu`: FAIL). On the host, QEMU's running command line, read back from
  `/proc/<pid>/cmdline`, has exactly one `-accel`, which is `kvm`, and
  `tcg` appears nowhere.
- **E2** Egress isolation, below.
- **E3** On the host, the downloaded image's SHA-256 equals the pinned one,
  the signature verifies from GnuPG's status, the signer is an approved
  signing subkey of the pinned primary, and the keyring holds exactly that
  primary. `verify-bound` re-derives this from the recorded fingerprints
  and digests; a record claiming a verification its own facts contradict
  is `INVALID`.
- **E4** The guest runner found this run's marker, KVM, hostname and a
  bundle that matches its manifest.

#### E2: egress isolation

Unanswered TCP probes prove nothing: a refused connection, a timeout or a
missing listener looks the same whether or not `restrict=on` is in force.
The commit reviewed at `a505c87` passed E2 on exactly that basis; this
section replaces it. E2 now combines four separate pieces of evidence:

1. **Verified QEMU configuration (host).** On the running command line
   there is exactly one network option. It is `-nic user,...,restrict=on`
   with a single `hostfwd=tcp:127.0.0.1:<port>-:22`, no `guestfwd`, and no
   other `-netdev`, `-net` or `-device`.
2. **Guest routing (guest).** There is exactly one default route, and it
   goes via the slirp gateway `10.0.2.2`, so the attempt below really
   reaches QEMU. Without that route, a failure says nothing about QEMU.
3. **Controlled attempt (both sides).**
   - Before the guest starts, the host runs a canary listener on its own
     `127.0.0.1:<port>`: exactly where slirp maps `10.0.2.2:<port>` when
     networking is **not** restricted. The host proves the canary live
     with one control connection before the guest run and one after.
   - The guest first proves its probing method works, with a connection
     to a guest-local listener. It then connects to `10.0.2.2:<port>`.
4. **Permitted versus forbidden paths.** The permitted path is inbound SSH
   through the single forward. The forbidden path is guest to host
   (and, through the same slirp mechanism, guest to anywhere), represented
   by the canary. Any canary connection beyond the host's two control
   connections counts as the guest reaching the host.

| Outcome | When |
| --- | --- |
| FAIL | the guest's attempt connected; the canary saw any extra connection; the running QEMU line is not restricted as above |
| INCONCLUSIVE | the guest-local control failed; no single default route via `10.0.2.2`; no attempt was made; the canary was not proven live before **and** after; the QEMU line could not be read |
| PASS | the guest's attempt did not connect, the canary saw only the host's two control connections, the canary was live before and after, and the running QEMU line is restricted |

The internet is never contacted, even to test that it is unreachable.
Guest-to-internet traffic would use the same slirp code path as the
canary, and `restrict=on` stops both. The offline test simulates
non-isolated configurations (refused probes, silent probes, no route) that
the old E2 passed, and shows that the new logic makes them FAIL or
INCONCLUSIVE.

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

- The acceptance **passes** only if
  `scripts/remote-provider-kvm-evidence.py verify-bound acceptance.json`
  returns the verdict `PASS`. That requires:
  - all 72 checks `PASS`, with E1-E3 finalized from host facts;
  - one approved commit, the same at the start and the end, with a clean
    tree throughout;
  - one run id;
  - the verified image digest and signature;
  - binary and unit hashes that match the host build.
- A guest evidence document on its own is never acceptance (its E1-E3 stay
  `INCONCLUSIVE`), and neither is a preparation document or a partial run.
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

Each guest check returns a verdict that maps explicitly:
- `True` → `PASS`;
- `False` → `FAIL`;
- the string `INCONCLUSIVE` → `INCONCLUSIVE`.

Any other value or result shape is a harness defect and is recorded as
`INCONCLUSIVE` (`invalid_verdict`), never `PASS`. It is also not `FAIL`,
because the check observed nothing. A raised exception is `INCONCLUSIVE`
too.

The guest exits 0 only when every check it decides is `PASS` and E1-E3
are `INCONCLUSIVE`/`awaiting_host`: the one state the host can finalize
into an acceptance candidate. The guest document's own `overall` is
therefore never `PASS`.

### Host/guest evidence chain

| Link | Recorded by | How it is bound |
| --- | --- | --- |
| approved commit | host gates (HEAD = `--approved-sha`, clean tree); HEAD and tree re-read at the end | every record's `git_commit`; `head_at_start` = `head_at_end` = approved; clean at both ends |
| run id | host (random, per run) | cloud-init marker, guest hostname, guest document, host facts and acceptance record must all agree |
| image | host: computed SHA-256, signature outcome, keyring primary and subkey fingerprints, signer primary and subkey fingerprints, lock file | E3; every record's `guest_image_sha256` = verified digest |
| binaries and unit | host: hashes of the built `providerd`/`flowerlyd`, the template from the approved commit, the bundle manifest | the guest verifies the bundle manifest (E4); every record's `providerd_sha256` and `unit_sha256` = host build |
| actual KVM | guest: `systemd-detect-virt`; host: QEMU's running command line | E1 |
| egress isolation | guest observations; host canary and QEMU network line | E2 |
| completeness | catalog | exactly one record per check; `NOT_RUN` or `INCONCLUSIVE` anywhere means not PASS |

`bind` writes `acceptance.json`. It contains:
- the guest's original document and its digest;
- the host facts;
- the finalized document;
- the verdict.

`verify-bound` re-derives the finalization from the guest's original
records and the host facts, then rechecks every link. If any link
disagrees, or the finalized document has been edited, the verdict is
`INVALID`.

The run directory `target/remote-provider-kvm/<run-id>/report/` keeps:
- `acceptance.json`, `verification.json`, `host-facts.json` and the
  guest's evidence document;
- `guest.log`, `serial.log`, `qemu-args.txt` and `qemu-cmdline.txt`;
- the image signature and `image-verification.json`.

## SSH to the guest

Trust-on-first-use is not used. For each run the harness generates the
guest's ed25519 SSH host key itself and puts it in cloud-init
(`ssh_keys`, with `ssh_deletekeys: true` and `ssh_genkeytypes: []`). It
writes the public half to a run-local `known_hosts` under the alias
`flowerly-kvm-<run id>`. Every `ssh` and `scp` uses
`StrictHostKeyChecking=yes`, `HostKeyAlias=flowerly-kvm-<run id>` and
`HostKeyAlgorithms=ssh-ed25519`. Anything else answering on the forwarded
port fails host-key verification.

The private host key exists only in the run directory (mode 0600 under
`umask 077`) and in the seed image, and both are deleted at teardown.
Anyone who can read the invoking user's files during the run could read
it; that user already controls the VM.

## Teardown

On exit, success or failure, the harness:
- kills QEMU;
- deletes the overlay, the cloud-init seed and user-data, the run's SSH
  client key and the guest's SSH host key pair, the export and the bundle;
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
- **Evidence is not signed.** The binding makes inconsistencies and edits
  of the finalized document detectable. Someone who controls the runner
  could still fabricate a self-consistent guest document and host facts.
  Trust rests on the recorded authorization, the run's logs and an
  independent rerun from the same commit and image.
- **E2 covers slirp's restrict mechanism, not every egress path in
  principle.** It verifies the running QEMU network configuration and
  that the guest cannot reach the host through it. The internet is not
  contacted to test reachability. Another host process connecting to the
  canary's port during the run would make E2 FAIL; that is conservative,
  never a false PASS.
- The older release runner `scripts/arch-kernel-vm-test.sh`, which is not
  part of this harness, still uses `StrictHostKeyChecking=no`.

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
