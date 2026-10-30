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
`/var/lib/flowerly-provider-operator/`.

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

1. It validates the whole ledger first. An instance it did not install,
   or a ledger naming any unexpected path or account, stops it before
   anything is removed.
2. It removes this instance's drop-in and configuration, but **only if
   unmodified**: an edited file is kept and reported. It removes this
   instance's socket and lock file, never following a symlink, and never
   the shared runtime directory while it is not empty.
3. The shared unit template, binary, client, sysusers file, account and
   groups are removed only if this tool created them and nothing else
   uses them. They are kept while any of these exist:
   - another instance of this tool;
   - a provider configuration from another installation;
   - an active `flowerly-provider-remote@` unit;
   - a process running as `flowerly-provider`;
   - a member of the group (for example `flowerly` once flowerlyd's gates
     are enabled).

   Anything kept is reported and stays in the ledger, so
   `rollback-shared` can finish once it is unused.

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

## Not covered

- Enabling flowerlyd's gates (`--remote-providers`, the flowerlyd drop-in,
  adding `flowerly` to the socket group, consent grants). That is a
  separate step needing its own approval.
- Pinning `IPAddressAllow=` to the destination's addresses. The operator's
  host is anycast, so pinning would break when its addresses change.
- Arch VM acceptance of the unit. The contract test verifies it with the
  host's systemd only; the plan for a real Arch Linux/KVM run is
  [Remote Provider Unit Test Plan](remote-provider-unit-test-plan.md).
- OS-boundary acceptance. The first live test (#268) ran providerd as the
  owner's user without the unit, so it accepted the adapter only.
