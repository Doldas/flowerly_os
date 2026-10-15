# 0009: Product Name Flowerly OS

Date: 2026-10-11
Status: Accepted

## Context

The project began under the working name "Agentic OS", and the architecture
specification and initial code use the `agentic-*` prefix for crates, services
and identifiers. The project now has a product name, "Flowerly OS", and a public
GitHub repository at https://github.com/Doldas/flowerly_os. Renaming everything
to match would churn code, docs and history for little technical benefit.

## Decision

The product is named "Flowerly OS", published at
https://github.com/Doldas/flowerly_os and licensed GPL-3.0-or-later. The product
name is used in documentation, user-facing text and UX. Internal crate,
service and identifier names keep the architecture-specification `agentic-*`
convention (`agentic-types`, `agenticd`, `flowd`, ...) to preserve technical
continuity with the specification and existing code. The Cargo workspace and the
repository layout therefore mix the product name and the `agentic-*` internals
deliberately.

## Consequences

- A clear, memorable product identity without a disruptive mass rename.
- Documentation stays aligned with the grounding architecture specification.
- New contributors must learn that "Flowerly OS" and `agentic-*` refer to the
  same system; this ADR is the canonical explanation.
- The repository URL and license are stated here for all later documents.
