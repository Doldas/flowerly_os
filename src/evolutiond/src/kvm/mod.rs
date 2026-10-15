//! Privileged KVM boundary. Hypervisor operations intentionally unavailable
//! until authorization and isolation policy can be enforced. Do not fall back
//! to executing privileged commands on the host.
pub const PROVISIONED: bool = false;
