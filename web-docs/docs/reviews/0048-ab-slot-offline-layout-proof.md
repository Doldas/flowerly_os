# #48 — offline A/B partition layout contract

Status 2026-11-07: **offline manifest-to-disk verification**. `python3 scripts/verify-ab-slot-layout.py disk.img disk.img.manifest.json` checks exactly ESP/root-A/root-B/data ordering, unique partition IDs, nonoverlap, bounds and SHA256 of each real disk slice. Negative fixtures cover duplicate identity, overlap and tampered hash; no root, KVM or boot success is claimed.

This strengthens the prototype builder evidence but does NOT establish kernel/initramfs/loader correctness, root witness, power-loss durability, active-slot selection, systemd-boot tries/fallback, cryptographic source trust, or successful Generation activation. ADR-0019 stays Proposed. Required real S7/S8 Arch/KVM rollback and recovery evidence and maintainer decision remain open.
