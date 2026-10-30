# Remote Provider Unit Test Plan (Arch Linux/KVM)

Plan for accepting the **OS security boundary** of the remote provider unit
`flowerly-provider-remote@.service` (ADR-0022, #263). The first live test
(#268) ran providerd as the owner's user without this unit, so it accepted
the adapter only. Nothing below has run yet.

## Scope and rules

- **Disposable guest:** a fresh official Arch Linux cloud image under
  QEMU/KVM, as in `scripts/arch-kernel-vm-test.sh`, at one exact commit
  recorded in every result. Nothing runs on a developer or production
  host.
- **No real key and no real destination:**
  - the credential is a dummy encrypted with the guest's own
    `systemd-creds`;
  - the "remote server" is a mock HTTPS server inside the guest network,
    with a test CA installed into the *guest's* trust store;
  - the owner's server is never contacted.
- **Exact SHA:** every result records `git rev-parse HEAD`, a clean-tree
  flag, `systemctl --version`, the kernel release, and the SHA-256 of the
  installed binary and unit.
- **Pass criteria:** every check below passes. A single violation fails
  the run. A refused or timed-out connection never counts as "blocked":
  only `EPERM`/`EACCES` from the unit's own settings does.

## Setup in the guest

1. Build at the tested commit and install with the operator procedure:
   `scripts/remote-provider-operator.sh install probe https://mock.test open-webui gpt-oss:20b`.
   Before that, encrypt a dummy key:
   `printf dummy | systemd-creds encrypt --name=probe.key - /etc/credstore.encrypted/probe.key`.
   Copy `scripts/provider-unit-probe.py` to `/usr/local/libexec/flowerly/`
   (0755, root), and record its SHA-256.
2. Install flowerlyd from the same commit, with its normal unit and a
   seeded `/var/lib/flowerly/state.db`, so there is real state to deny.
3. Create unrelated secrets: `/etc/credstore.encrypted/other.key`,
   `/root/.ssh/id_ed25519`, and a second user's home with a readable file.
4. Run the mock OpenAI-compatible HTTPS server on an address that is
   reachable from the guest. Also run listeners on `127.0.0.1:22`, a
   private address and a link-local address, so a connection that is not
   blocked visibly succeeds.

## Part A: confinement probe

Run `scripts/provider-unit-probe.py` **inside the unit**. A runtime drop-in
replaces only `ExecStart=`, so every other setting of the template applies:

```ini
# /run/systemd/system/flowerly-provider-remote@probe.service.d/90-probe.conf
[Service]
ExecStart=
ExecStart=/usr/bin/python3 -I /usr/local/libexec/flowerly/provider-unit-probe.py
Environment=FLOWERLY_PROBE_INSTANCE=probe FLOWERLY_PROBE_RESOLVER=<loopback stub or empty>
Type=oneshot
```

Capture the result with `systemctl start`, then `journalctl -o cat`. The
probe never reads secret contents.

| Area | Checks (all must be denied) |
| --- | --- |
| Flowerly state | read `/var/lib/flowerly/state.db`, list `/var/lib/flowerly`, `/run/flowerly`, `/var/lib/flowerly-evolution` |
| Unrelated secrets | `/etc/credstore`, `/etc/credstore.encrypted` (including its own `.key` file), `/etc/shadow`, `/etc/gshadow`, `/root`, `/home`, the operator ledger and evidence directories |
| Credentials | `$CREDENTIALS_DIRECTORY` holds exactly `probe.key` |
| Writes | `/etc`, `/usr/local`, `/var/lib`, `/run` outside its runtime directory |
| Privilege | `Cap*` all zero, `NoNewPrivs: 1`, `Seccomp: 2`, uid/gid not 0, no supplementary group; `setuid(0)`, user namespaces, `/proc/1/environ`, `/proc/1/fd`, `/proc/sys` writes, `/dev/kvm`, `/dev/mem` |
| Network, OS level | `127.0.0.1`, `::1`, `169.254.169.254`, `fe80::/10` connects fail with `EPERM`; `AF_PACKET`, `AF_NETLINK`, raw ICMP sockets refused |
| DNS stub | if the host uses a loopback stub, `connect` to it is allowed (the operator drop-in's `IPAddressAllow=`) |

Then rerun the probe in a `--require-isolated-identity` mismatch:
1. Add `flowerly-provider` to another group, for example `flowerly`.
2. Expect the **real** providerd unit to fail at startup with
   `supplementary group`.
3. Expect `install` to refuse on a fresh guest set up the same way.

## Part B: real providerd in the unit

Use the real `ExecStart` against the guest's mock server:

1. **Address policy (application level).** The unit does not block
   RFC 1918, CGNAT or arbitrary public destinations at the OS level; the
   application does. Point `api_base` at:
   - a name resolving to a private address;
   - a name resolving to loopback;
   - a name that changes from public to private between lookups
     (rebinding, using a guest DNS server under test control).

   Each must fail as `address_refused` / `not_sent`, and the guest's
   listener must see **no** connection.
2. **TLS:** a certificate from a CA outside the guest trust store, or
   with the wrong name, must fail as `tls`, `not_sent`, with no
   plaintext fallback.
3. **Redirect:** a `302` to another host must fail as `redirect_refused`,
   and the other host must see nothing.
4. **Discovery-only preflight:** `remote-provider-operator.sh preflight
   probe` must refuse `provider.submit` with `discover_only`.
5. **Acceptance against the mock:** `accept probe <attestation>` must
   send exactly one chat request to the mock. The evidence must hold no
   prompt, text, bearer header or key, and the journal scan must be clean.
6. **Socket authentication boundary.** The provider admits a peer by
   the identity the kernel reports for the connecting process:
   `SO_PEERCRED` (uid, primary gid, pid) plus `SO_PEERGROUPS`
   (supplementary groups), read once at accept. It does **not** use
   `/etc/group`, the socket file's mode or anything the client sends. Test
   each case with exact credentials, using
   `setpriv --reuid=<u> --regid=<g> --groups=<list> --` around a minimal
   socket client. Record the peer identity the provider logged and the
   client's `/proc/self/status` `Uid:`/`Gid:`/`Groups:` lines:

   | Connecting process | Expected |
   | --- | --- |
   | `flowerly`, `flowerly-provider-clients` among its effective groups | admitted |
   | `flowerly` listed in `/etc/group` but started **before** joining (stale groups) | refused: the kernel groups decide, not the file |
   | ordinary user, no group | refused at `connect` (0660 socket) |
   | ordinary user, group gained only via `setpriv --groups` | admitted: shows that **effective** membership is the boundary |
   | root (uid 0) **without** the group | refused at the peer check even though root bypasses the 0660 mode |
   | root **with** the group | admitted; root is trusted by the plan's threat model |
   | `flowerly-provider` itself (service account) | admitted |

   A second admitted peer cannot poll, cancel or read flowerlyd's jobs
   (`created_by` is the peer's kernel uid).
7. **Effective process groups.** While the real unit runs, record
   `/proc/<MainPID>/status` (`Uid:`, `Gid:`, `Groups:`, `CapEff:`,
   `NoNewPrivs:`) using `systemctl show -p MainPID`. Also record the same
   lines for flowerlyd's main process after `flowerly` joined
   `flowerly-provider-clients` and flowerlyd was restarted. Expected:
   - the provider's `Groups:` is empty or only its primary gid;
   - flowerlyd's `Groups:` includes the clients group only after the
     restart, so a stale process is not counted as a member.
8. **Group membership is not consent.** Effective membership of
   `flowerly-provider-clients` authenticates a **process** to the provider
   socket. It proves nothing about which human asked for remote inference
   or whether they consented. Per-user consent, the administrator gate and
   the profile gate are enforced **only in flowerlyd**, against the uid of
   flowerlyd's own peer. They are covered by the black-box test
   `scripts/test-remote-inference-gates.py`, not by this unit test. This
   plan must record that any process admitted here (flowerlyd, or root
   with the group) can spend the key without per-user consent. The
   install-time membership check limits who that is; it is not consent.
9. **Shared-group refusal:** start flowerlyd with `AGENTIC_USERS_GROUP` set
   to `flowerly-provider-clients`. The journal must show remote inference
   disabled with the "would admit the provider process" reason.

## Part C: lifecycle

1. Run `disable probe`: the unit stops, is masked and cannot be started.
   If present, the gate policy is moved aside and flowerlyd reports
   remote inference disabled.
2. Run `rollback probe` with a second instance installed: the second
   instance, the shared binary, the account and the groups remain.
3. Roll back the last instance while a process runs as `flowerly-provider`
   (and, separately, while another user runs a copy of `providerd` from
   `/usr/local/libexec/flowerly`). Expected:
   - the instance files go;
   - **every** shared file, the account, the groups and the shared ledger
     stay byte-identical;
   - `rollback-shared` removes them once the process has gone.
4. Rollback after `flowerly` joined the socket group: nothing shared changes.
5. Edit the instance config, then roll back. Expected:
   - the file stays and the ledger keeps its entry;
   - preflight refuses;
   - after restoring the file, a rerun completes.
6. Run `rollback` while `preflight` holds the lock: it refuses with no
   change. Kill a rollback with `kill -9` midway; the documented rerun
   completes it.
7. After the final rollback, nothing the ledger recorded remains. The
   pre-existing accounts, groups and files are byte-identical, compared
   against a snapshot taken before `install`.

## Evidence

Attach one JSON file per part. Each holds:
- the exact SHA;
- versions;
- every probe check with its expected and observed result;
- the mock server's request log (paths and counts only);
- `systemd-analyze security` for the instance.

An independent reviewer reruns at least Part A from the same SHA.

## Known limits this plan does not remove

- **No OS-level destination allowlist.** The unit blocks only localhost,
  link-local and multicast. Other destinations rely on providerd's
  resolver and address policy. Pinning `IPAddressAllow=` to the
  destination is not possible for an anycast host.
- **Unproven assumption about supplementary groups.** systemd applies the
  account's supplementary groups. The plan assumes `Group=` yields exactly
  one group; if the guest shows otherwise, `--require-isolated-identity`
  fails closed and the unit needs adjusting.
- **Membership of `flowerly-provider-clients` is authority** to spend the
  key without per-user consent. It is checked at install, preflight and
  accept, but not continuously. Group membership is never evidence of a
  user's consent (Part B, step 8).
- **Root is trusted.** The operator lock serializes this tool's commands
  and refuses directories others can write. Another root process could
  still change a file between the tool's check and its removal.
