# Layer 1.5 dependency guard expansion (#93)

The offline boundary check `scripts/test-layer-15-boundaries.py` now
examines the `ownership` and `capability` domain modules and the
crate-root `authorization.rs` module, in addition to the previously
protected `flow`, `goal`, `generation`, `provenance`, `agentic`
and `compute` modules.

New direct `rusqlite` imports and explicit `std::os::unix` or
kernel-path access in those protected files fail the offline preflight.
Existing reviewed Linux and SQLite adapter exceptions remain unchanged.
The change does not move kernel primitives, relax privileged process
checks, introduce a facade, or change runtime privileges.

## Verification and remaining acceptance

The regression suite includes synthetic unauthorized import fixtures for
`ownership`, `capability`, and `authorization`. Run
`python3 scripts/test-layer-15-boundaries.py` and
`bash scripts/preflight-docs.sh`; exact-head CI results are required.

This is **a narrow enforcement increment**, not completion of #93.
The following still require explicit architecture/security review:
crate-level dependency graph and Cargo feature matrix, all adapter
boundaries, reviewed allowlist, service/agentic-storage separation, and
Linux kernel/PTY negative evidence. In particular, no source-level
import check replaces actual kernel/cgroup authority validation.

Per #93: **do not merge without explicit maintainer authorization**.
Status 2026-11-07: draft implementation proposed, no local full preflight
or real kernel acceptance claimed.
