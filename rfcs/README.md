# Requests for Comments (RFCs)

RFCs are the process for proposing a design before large implementation work
begins. An RFC is cheaper to change than merged code. Use an RFC when a change:

- adds a new language or toolchain to the project,
- introduces a new persistent object or adds a new daemon/service boundary,
- changes IPC, storage, packaging or security architecture at a systemic level,
- is large enough that reviewers need the design before the diff.

Small, local changes do not need an RFC. When in doubt, propose one.

## Status lifecycle

```text
draft -> proposed -> accepted -> implemented
                  \-> rejected
                  \-> superseded
```

- **draft** — the author is still shaping it; not yet up for review.
- **proposed** — ready for review and discussion.
- **accepted** — agreed in principle; implementation may start.
- **implemented** — the accepted design exists in the repository.
- **rejected** — not going to happen; keep the record and the reasoning.
- **superseded** — replaced by a later RFC; link to the successor.

## Filenames

Use a zero-padded four-digit number and a short slug:

```text
NNNN-title-in-kebab-case.md
```

Numbers are assigned in order. The first RFC is `0001`; `0000-template.md` is the
template and is never assigned. Once an RFC is accepted, its number and filename
do not change.

## Process

1. Copy `0000-template.md` to `NNNN-title.md`.
2. Fill it in and open a pull request that sets the status to `proposed`.
3. Discuss and revise until consensus; keep the PR open while that happens.
4. On acceptance, a maintainer merges and updates the status to `accepted`.
5. Implementation follows. Update the status to `implemented` when it lands.
6. If a decision is consequential and durable, record it as an ADR in `adr/`.
   RFCs explore options; ADRs record the decision that was made.

## Template summary

Every RFC contains: Title, Status, Summary, Motivation, Design, Alternatives,
Drawbacks, Unresolved questions, References. See `0000-template.md`.
