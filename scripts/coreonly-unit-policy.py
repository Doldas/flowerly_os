#!/usr/bin/env python3
"""Offline CoreOnly startup policy contract (#88), NOT a daemon switch."""

CORE_SERVICE = "flowerlyd.service"
OPTIONAL_SERVICES = frozenset(("evolutiond.service", "providerd.service"))


def allowed_units(mode):
    if mode == "core_only":
        return frozenset((CORE_SERVICE,))
    if mode == "standalone":
        # Optional units require separately explicit operator admission.
        return frozenset((CORE_SERVICE,))
    if mode == "cluster":
        # Never grant network or replication just from the mode token.
        return frozenset((CORE_SERVICE,))
    raise ValueError("invalid mode; no service activation authorized")


def check_plan(mode, requested):
    allowed = allowed_units(mode)
    return sorted(set(requested) - allowed)


if __name__ == "__main__":
    raise SystemExit("policy prototype only; systemd has not been wired to /etc/flowerly/mode")
