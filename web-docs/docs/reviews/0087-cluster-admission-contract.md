# #87 — fail-closed peer admission planning

Status 2026-11-07: **pure offline policy prototype**, no networking, peer enrollment, replication or system service. `scripts/cluster-peer-admission.py` checks explicit admin enablement, authenticated encrypted channel assertions, exact peer ID, SHA256-pinned presented key, CIDR allowlist, memory schema compatibility and revocation; the negative test covers wrong identity/key, disallowed address, revocation, version mismatch and missing authorization. Assertions about authenticated transport are inputs and NOT cryptographic proofs.

Never trust mDNS discovery, automatically replicate core state, assume libSQL/Turso multi-writer support, or open a port because Cluster was selected. Required before completion: current libSQL/Turso version-specific validation, actual mTLS identity/key lifecycle, signed source-tracked hot-indicator quarantine, three-node real partition/reconnect/conflict test, measured resource/SLO and security approval.
