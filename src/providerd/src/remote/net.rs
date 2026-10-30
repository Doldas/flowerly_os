//! Network policy for the remote backend (ADR-0022 §3).
//!
//! - TLS: rustls (ring provider) verified against the system trust store.
//!   Nothing here can disable verification.
//! - The client never follows redirects, ignores proxy environment
//!   variables, and only resolves the configured host.
//! - Address filtering happens inside the resolver the connection uses, so
//!   the addresses checked are exactly the addresses connected to (no
//!   check-then-reresolve DNS rebinding window).
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use super::config::RemoteConfig;

#[derive(Debug, Clone, Copy)]
pub struct AddressPolicy {
    /// RFC 1918, CGNAT and IPv6 ULA: an explicit per-provider opt-in for a
    /// self-hosted LAN server.
    pub allow_private: bool,
    /// Loopback: only for loopback mock servers in this crate's tests.
    pub allow_loopback: bool,
}

/// Refuse addresses a provider destination must never reach.
pub fn address_allowed(ip: IpAddr, policy: AddressPolicy) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_allowed(v4, policy),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(mapped) => v4_allowed(mapped, policy),
            None => v6_allowed(v6, policy),
        },
    }
}

fn v4_allowed(ip: Ipv4Addr, policy: AddressPolicy) -> bool {
    let [a, b, c, _] = ip.octets();
    if ip.is_loopback() {
        return policy.allow_loopback;
    }
    let never = ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_link_local()
        || ip.is_documentation()
        || a == 0
        || a >= 240
        || (a == 198 && (b == 18 || b == 19))
        || (a == 192 && b == 0 && c == 0);
    if never {
        return false;
    }
    let private = ip.is_private() || (a == 100 && (64..128).contains(&b));
    !private || policy.allow_private
}

fn v6_allowed(ip: Ipv6Addr, policy: AddressPolicy) -> bool {
    if ip.is_loopback() {
        return policy.allow_loopback;
    }
    let first = ip.segments()[0];
    let never = ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xffc0) == 0xfe80 // link-local
        || (first == 0x2001 && ip.segments()[1] == 0x0db8) // documentation
        || (first == 0x0064 && ip.segments()[1] == 0xff9b) // NAT64 to arbitrary v4
        || ip.to_ipv4().is_some(); // IPv4-compatible (deprecated)
    if never {
        return false;
    }
    let unique_local = (first & 0xfe00) == 0xfc00;
    !unique_local || policy.allow_private
}

/// Resolves only the configured host and drops every refused address.
struct FilteringResolver {
    host: String,
    policy: AddressPolicy,
}

impl Resolve for FilteringResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let requested = name.as_str().to_ascii_lowercase();
        let host = self.host.clone();
        let policy = self.policy;
        Box::pin(async move {
            if requested != host {
                return Err(format!(
                    "resolution of `{requested}` refused: not the configured host"
                )
                .into());
            }
            let resolved: Vec<SocketAddr> = tokio::net::lookup_host((requested.as_str(), 0))
                .await?
                .collect();
            let allowed: Vec<SocketAddr> = resolved
                .into_iter()
                .filter(|addr| address_allowed(addr.ip(), policy))
                .collect();
            if allowed.is_empty() {
                return Err(AddressRefused.into());
            }
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
}

/// Every resolved address of the configured host violates the policy.
#[derive(Debug)]
pub struct AddressRefused;

impl std::fmt::Display for AddressRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("every resolved address is refused by the address policy")
    }
}

impl std::error::Error for AddressRefused {}

/// Environment variables through which `rustls-native-certs` would replace
/// the system trust store.
const TRUST_OVERRIDES: [&str; 2] = ["SSL_CERT_FILE", "SSL_CERT_DIR"];

/// Load the system trust store (Arch: `ca-certificates`). An empty store
/// is a startup error, never a reason to skip verification, and so is any
/// environment override of the store: trust is not configurable from the
/// environment.
pub fn system_roots() -> Result<rustls::RootCertStore, String> {
    trust_overrides_absent(|name| std::env::var_os(name).is_some())?;
    let loaded = rustls_native_certs::load_native_certs();
    let mut roots = rustls::RootCertStore::empty();
    let (added, _ignored) = roots.add_parsable_certificates(loaded.certs);
    if added == 0 {
        return Err(
            "no usable system trust roots; refusing to start without TLS verification".to_owned(),
        );
    }
    Ok(roots)
}

fn trust_overrides_absent(is_set: impl Fn(&str) -> bool) -> Result<(), String> {
    match TRUST_OVERRIDES.iter().find(|name| is_set(name)) {
        Some(name) => Err(format!(
            "{name} is set; refusing to let the environment replace the system trust store"
        )),
        None => Ok(()),
    }
}

/// Build the only HTTP client the remote backend uses.
pub fn build_client(
    config: &RemoteConfig,
    roots: rustls::RootCertStore,
    policy: AddressPolicy,
) -> Result<reqwest::Client, String> {
    // IP literals bypass the resolver, so check them here.
    if let Ok(ip) = config.host.parse::<IpAddr>() {
        if !address_allowed(ip, policy) {
            return Err(format!(
                "api_base address {ip} is refused by the address policy"
            ));
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|error| format!("TLS configuration: {error}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .referer(false)
        .connect_timeout(config.connect_timeout)
        .timeout(config.request_timeout)
        .pool_max_idle_per_host(config.max_concurrency as usize)
        .user_agent(concat!("flowerly-providerd/", env!("CARGO_PKG_VERSION")))
        .dns_resolver(Arc::new(FilteringResolver {
            host: config.host.clone(),
            policy,
        }))
        .build()
        .map_err(|error| format!("building HTTP client: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRICT: AddressPolicy = AddressPolicy {
        allow_private: false,
        allow_loopback: false,
    };
    const LAN: AddressPolicy = AddressPolicy {
        allow_private: true,
        allow_loopback: false,
    };

    #[test]
    fn address_policy_refuses_internal_and_special_ranges() {
        for refused in [
            "127.0.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "169.254.169.254",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "::127.0.0.1",
        ] {
            let ip: IpAddr = refused.parse().unwrap();
            assert!(!address_allowed(ip, LAN), "{refused} allowed");
        }
        for private in [
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "fd00::1",
            "::ffff:10.0.0.1",
        ] {
            let ip: IpAddr = private.parse().unwrap();
            assert!(
                !address_allowed(ip, STRICT),
                "{private} allowed without opt-in"
            );
            assert!(address_allowed(ip, LAN), "{private} refused with opt-in");
        }
        for public in ["93.184.216.34", "1.1.1.1", "2606:4700::1111"] {
            assert!(address_allowed(public.parse().unwrap(), STRICT), "{public}");
        }
        assert!(address_allowed(
            "127.0.0.1".parse().unwrap(),
            AddressPolicy {
                allow_private: false,
                allow_loopback: true
            }
        ));
    }

    #[test]
    fn environment_trust_overrides_are_refused() {
        assert!(trust_overrides_absent(|_| false).is_ok());
        for name in TRUST_OVERRIDES {
            let refused = trust_overrides_absent(|set| set == name).unwrap_err();
            assert!(refused.contains(name), "{refused}");
        }
    }

    #[tokio::test]
    async fn the_resolver_only_answers_for_the_configured_host() {
        let resolver = FilteringResolver {
            host: "localhost".to_owned(),
            policy: STRICT,
        };
        let other = resolver.resolve("example.com".parse().unwrap()).await;
        assert!(other.is_err());
        // localhost resolves only to loopback, which this policy refuses.
        let refused = resolver.resolve("localhost".parse().unwrap()).await;
        let error = refused.err().expect("loopback refused");
        assert!(error.downcast_ref::<AddressRefused>().is_some(), "{error}");
    }
}
