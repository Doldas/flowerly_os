# 0010: Unix-Socket JSON IPC for Local Services

Date: 2026-10-11  
Status: Accepted

## Context

Flowerly OS services need a local control protocol that is inspectable, versionable,
easy to exercise from tests, and independent of any AI provider. The project does
not yet need a distributed RPC framework.

## Decision

Local services use Unix domain sockets with protocol-versioned, length-prefixed
JSON envelopes.

The current wire contract is:

```text
u32 little-endian payload length
UTF-8 JSON Envelope {
    version,
    id,
    payload
}
```

Frames are bounded in size. The protocol is local-only. Distributed or remote
transport is a separate future concern and must not leak into the local control
plane merely for architectural symmetry.

## Consequences

- Messages remain inspectable with ordinary tools.
- Protocol evolution is explicit through the envelope version.
- Unix permissions and Linux process identity remain available as real security
  boundaries.
- JSON is not assumed to be the forever format; changing it requires evidence,
  compatibility planning, and an ADR.
- No generic distributed RPC layer is introduced during bootstrap.
