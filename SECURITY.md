# Security Policy

## Reporting a vulnerability

Please report security vulnerabilities privately. Do not open a public issue,
pull request, or discussion for a vulnerability.

Use GitHub's private vulnerability reporting for this repository (the
**Security** tab, "Report a vulnerability"). Include, where possible:

- a description of the issue and its impact;
- the component, version, or commit affected;
- reproduction steps, a proof of concept, and any suggested mitigation.

We will acknowledge the report, investigate, and coordinate disclosure. Please
allow reasonable time before disclosing publicly.

## Private disclosure and response process

Use the repository's **Security → Report a vulnerability** entry point,
which creates a private advisory when private vulnerability reporting is
enabled. If that option is unavailable, contact a repository maintainer through
an established private channel; do **not** include exploit details or secrets
in a public issue, discussion, pull request or CI log.

Provide affected versions/commits, expected security boundary, reproduction
steps, impact, and a safe proof of concept where possible. Never submit live
credentials or third-party personal data.

Maintainers should acknowledge a report when able, assess exploitability,
contain/rotate compromised secrets, coordinate a fix and affected-release
verification, and agree on disclosure timing with the reporter. We do not
promise an unstaffed 24-hour response SLA or publish an unverified fix.
Security advisories and mitigations should link to validated fixes without
revealing sensitive reproduction material prematurely.

## Scope

In scope: the source code, configuration, and packaging in this repository, and
the system images and packages generated from it.

Out of scope: upstream Linux and third-party software, which should be reported
to their respective maintainers.

## Security principles

Flowerly OS treats autonomy and security as systems architecture, not as
opposites. The design direction is:

- **Capability and policy.** Agents have identity, explicit authority, and
  delegated scopes; privilege does not automatically follow a delegated goal.
- **Provenance.** Every meaningful autonomous change is attributable to a goal,
  agent, command, source, test, and generation.
- **Sandboxed evolution.** Aggressive, potentially destructive experiments run
  in a separate KVM Evolution World, not on the stable system.
- **Reversibility.** Important system changes should be versioned,
  transactional, and reversible.
- **No secrets in configuration.** Provider credentials are never mixed into
  core architecture or committed to the repository.

## Commit policy

Never commit secrets, credentials, tokens, private keys, or configuration files
containing them, and never log secrets. If a secret is committed accidentally,
treat it as compromised, rotate it, and remove it from history.

## Supported versions

Flowerly OS is pre-1.0. Security fixes target the actively maintained release
line only. Older development commits and pre-release snapshots have no
independent backport commitment.

| Version | Supported |
| ------- | --------- |
| 0.0.x   | Yes       |
| < 0.0.1 | No        |

## Threat model

The initial threat categories are a compromised model provider, malicious tool
output, prompt injection from external content, privilege escalation, poisoned
packages, corrupted generations, credential leakage, agent confused-deputy
behavior, persistent malicious memory, and unsafe credential crossover between
Flows. A dedicated `docs/THREAT_MODEL.md` will expand on these as mitigations
are implemented.
