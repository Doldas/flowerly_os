# Flowerly OS — User Model

The user model describes what the system knows about a person, so the
environment can adapt to them. It is built on a single rule: never silently turn
an inference into a fact. Everything is local by default.

## Three categories

The model separates information by how it was obtained:

```text
EXPLICIT   the user stated it directly (a fact)
OBSERVED   the system observed behavior
INFERRED   the system formed a hypothesis
```

An inferred statement is never promoted to an explicit fact without user
confirmation. Inferred records carry confidence and can be invalidated.

## Current schema

The initial durable tables exist in `crates/agentic-storage/migrations/0001_init.sql`:

```text
user_facts         id, statement, created_at
user_observations  id, statement, source, created_at
user_inferences    id, statement, confidence, invalidated, created_at
```

These correspond to explicit, observed and inferred information. Today they are
schema only: the tables exist, but there is no service yet that populates or
reasons over them. `memoryd` (the future durable-memory service) is **planned**.

## De-identified ambient observation model (planned)

The ambient interaction model — see
[`AGENTIC_OS_UPDATE_01_AMBIENT_INTERACTION.md`](AGENTIC_OS_UPDATE_01_AMBIENT_INTERACTION.md)
— extends the user model with ambient observation. The intended structure is:

- **threads** — a coherent interaction context over time (a conversation, a
  recurring workflow, a project), grouping related observations.
- **observations** — de-identified signals derived from interaction: usage
  patterns, repeated workflows, timing, modality (voice, pen, touch, keyboard).
- **inferences** — hypotheses derived from observations, with confidence and an
  invalidation state, never treated as facts.

Ambient observation is explicitly opt-in and de-identified before it is retained.
Raw audio, screen content and keystrokes are not durable by default; only the
minimal derived signal needed to improve the relationship is kept, locally.

## Consent and privacy first

Privacy is a first-class design constraint, not a setting hidden in a menu:

- **Local by default:** the user model lives on the user's machine. Nothing is
  sent to a provider, cloud or cluster unless the user explicitly enables it.
- **Consent:** ambient capture requires clear, revocable consent. The microphone
  and attention states are always visible and controllable.
- **De-identification:** observed data is reduced to what is needed; identities
  and raw content are not retained by default.
- **Transparency:** inferred statements expose their sources and confidence, and
  can be inspected, corrected or deleted.
- **Reversibility:** adaptation derived from the model should be reversible.

## Implementation status

- **Implemented:** the `user_facts`, `user_observations` and `user_inferences`
  tables and the explicit/observed/inferred separation in the schema.
- **Planned:** `memoryd` and the ambient services, thread grouping,
  de-identification pipeline, confidence/invalidation lifecycle, consent
  management, and adaptation proposals that feed back into the environment.
