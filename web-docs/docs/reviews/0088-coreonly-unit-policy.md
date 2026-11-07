# #88 — CoreOnly unit eligibility contract

Status 2026-11-07: **offline policy and negative tests only**, NOT wired into the installed systemd runtime. `scripts/coreonly-unit-policy.py` defines conservative allowed units for each installation mode. The only implicitly allowed unit is `flowerlyd.service`; Evolution/provider services require a separately reviewed operator activation path. Tests assert CoreOnly refuses optional services and Cluster cannot gain listeners implicitly.

Flowerly has a unified supervisor rather than separate shipped goald/agenticd services. This proposal does not pretend to disable individual internal modules or to prove CoreOnly boot. Remaining: real daemon startup/shutdown mode gate, systemd integration, missing/corrupt optional memory handling, no-network and process inventory, exact-SHA offline KVM reboot/health/return-to-Standalone acceptance and owner approval. No default installed behavior changes.
