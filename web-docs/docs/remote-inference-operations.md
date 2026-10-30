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
installs the provider binary. It starts and enables nothing.

| Path | Content |
| --- | --- |
| `/etc/sysusers.d/flowerly-provider.conf` | `flowerly-provider` account and the `flowerly-provider-clients` socket group (owner Q1) |
| `/etc/systemd/system/flowerly-provider-remote@.service` | the confined unit template, unchanged |
| `/etc/systemd/system/flowerly-provider-remote@<instance>.service.d/10-operator.conf` | binary path; `IPAddressAllow=` for a loopback DNS stub only |
| `/etc/flowerly/providers/<instance>.service.toml` | socket name, runtime directory, client group |
| `/etc/flowerly/providers/<instance>.toml` | destination, API style, one allowlisted model, limits; no key |
| `/usr/local/libexec/flowerly/providerd` | the release binary built from the checkout |
| `/usr/local/libexec/flowerly/remote-provider-acceptance.py` | the acceptance client |

The unit runs as `flowerly-provider` with no capabilities, a read-only
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

The approval reference (for example the URL of the owner's approving
comment) is recorded in the evidence. The step re-checks DNS, starts the
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

A reasoning model may spend all 16 tokens before answering and finish with
`length` and an empty or partial reply. That is still a real result, and
it is recorded as such. Any second request needs a new approval.

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
```

Removes everything `install` wrote for the instance, and once no instance
remains, the shared unit template, binary, client, account and groups. The
encrypted key stays for the operator to remove:
`sudo rm /etc/credstore.encrypted/<instance>.key`.

## Not covered

- Enabling flowerlyd's gates (`--remote-providers`, the flowerlyd drop-in,
  adding `flowerly` to the socket group, consent grants). That is a
  separate step needing its own approval.
- Pinning `IPAddressAllow=` to the destination's addresses. The operator's
  host is anycast, so pinning would break when its addresses change.
- Arch VM acceptance of the unit; the contract test verifies it with the
  host's systemd only.
