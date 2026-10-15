# RFC 0001: Intent as a First-Class Interaction Domain

Status: Proposed

## Problem

Flowerly OS is intent-first, but not every human expression should become a
persistent Goal.

```text
"Show the servers."
```

is an intent to change context.

```text
"Reduce boot time by at least 20%."
```

may become a durable Goal with evidence and success criteria.

Collapsing both into Goal would either make Goals noisy or force interaction
logic into presentation code.

## Proposal

Reserve **Intent** as a first-class interaction-domain object between raw input
and durable Goals:

```text
input / utterance / gesture
          ↓
        Intent
          ↓
   context resolution
      ↙         ↘
 immediate      Goal
 action         creation
```

An Intent may reference:

- active Flow
- selected project/object
- interaction modality
- resolved entities
- requested action
- confidence/ambiguity
- optional resulting Goal

This RFC reserves the boundary now; it does not require an `intentd` daemon.
ADR-0013 applies: process boundaries require independent justification.

## Initial implementation impact

`IntentId` is reserved in `agentic-types` so provenance and future schemas can
refer to intent without changing the global identifier model later.

No persistent Intent table or service is required by the current release.
