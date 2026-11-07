#!/usr/bin/env python3
"""Pure fail-closed cluster peer eligibility contract, NO networking (#87).

Not a trust implementation. mDNS discovery cannot grant this eligibility.
"""
import dataclasses
import ipaddress


@dataclasses.dataclass(frozen=True)
class PeerPolicy:
    peer_id: str
    pinned_key_sha256: str
    allowed_networks: tuple[str, ...]
    memory_schema: int
    revoked: bool = False


def eligible_peer(policy, peer_id, presented_key_sha256, address, memory_schema, *,
                  encrypted, authenticated, operator_enabled):
    if not operator_enabled or not encrypted or not authenticated or policy.revoked:
        return False
    if not policy.peer_id or peer_id != policy.peer_id:
        return False
    if len(policy.pinned_key_sha256) != 64 or not all(
        c in "0123456789abcdef" for c in policy.pinned_key_sha256
    ):
        return False
    if presented_key_sha256 != policy.pinned_key_sha256:
        return False
    if not isinstance(memory_schema, int) or memory_schema != policy.memory_schema:
        return False
    try:
        ip = ipaddress.ip_address(address)
        return any(ip in ipaddress.ip_network(net, strict=True)
                   for net in policy.allowed_networks)
    except (ValueError, TypeError):
        return False


if __name__ == "__main__":
    raise SystemExit("policy contract only: real three-node TLS replication is not implemented")
