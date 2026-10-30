//! Network policy for the remote backend (ADR-0022 §3).
//!
//! - TLS: rustls (ring provider) verified against the system trust store.
//!   Nothing here can disable verification.
//! - The client never follows redirects, ignores proxy environment
//!   variables, and only resolves the configured host.
//! - Address filtering happens inside the resolver the connection uses, so
//!   the addresses checked are exactly the addresses connected to (no
//!   check-then-reresolve DNS rebinding window).
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use super::config::RemoteConfig;

/// An address block, `addr/prefix`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Parse an administrator's private-destination exception. Only blocks
    /// wholly inside RFC 1918, CGNAT or IPv6 ULA space, at least as narrow
    /// as /16 (IPv4) or /48 (IPv6), are accepted: an exception can never
    /// open loopback, link-local, metadata or public space (owner Q2).
    pub fn parse_private(raw: &str) -> Result<Self, String> {
        let (addr, prefix) = raw
            .split_once('/')
            .ok_or_else(|| format!("`{raw}` must be written as address/prefix"))?;
        let network: IpAddr = addr
            .parse()
            .map_err(|_| format!("`{raw}` has an invalid address"))?;
        let prefix: u8 = prefix
            .parse()
            .map_err(|_| format!("`{raw}` has an invalid prefix"))?;
        let (max, min) = match network {
            IpAddr::V4(_) => (32, 16),
            IpAddr::V6(_) => (128, 48),
        };
        if prefix > max || prefix < min {
            return Err(format!("`{raw}` prefix must be /{min}../{max}"));
        }
        let cidr = Self { network, prefix };
        if cidr.masked(network) != network {
            return Err(format!("`{raw}` has host bits set"));
        }
        if !is_private(network) {
            return Err(format!(
                "`{raw}` is not inside RFC 1918, CGNAT or IPv6 ULA space"
            ));
        }
        Ok(cidr)
    }

    fn masked(self, ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V4(v4) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                IpAddr::V4(Ipv4Addr::from(u32::from(v4) & mask))
            }
            IpAddr::V6(v6) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask))
            }
        }
    }

    pub fn contains(self, ip: IpAddr) -> bool {
        ip.is_ipv4() == self.network.is_ipv4() && self.masked(ip) == self.network
    }
}

#[derive(Debug, Clone, Default)]
pub struct AddressPolicy {
    /// Private destinations (RFC 1918, CGNAT, IPv6 ULA) are refused unless
    /// inside one of these administrator-approved blocks. Empty by default.
    pub private_allowlist: Vec<Cidr>,
    /// Loopback: only for loopback mock servers in this crate's tests.
    pub allow_loopback: bool,
}

fn unmapped(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn is_private(ip: IpAddr) -> bool {
    match unmapped(ip) {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private() || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

/// Addresses no destination may ever reach, exception or not.
fn never_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_link_local()
                || v4.is_documentation()
                || a == 0
                || a >= 240
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b == 0 && c == 0)
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xffc0) == 0xfe80 // link-local
                || (first == 0x2001 && v6.segments()[1] == 0x0db8) // documentation
                || (first == 0x0064 && v6.segments()[1] == 0xff9b) // NAT64 to arbitrary v4
                || v6.to_ipv4().is_some() // IPv4-compatible (deprecated)
        }
    }
}

/// Refuse addresses a provider destination must never reach.
pub fn address_allowed(ip: IpAddr, policy: &AddressPolicy) -> bool {
    let ip = unmapped(ip);
    if ip.is_loopback() {
        return policy.allow_loopback;
    }
    if never_allowed(ip) {
        return false;
    }
    !is_private(ip)
        || policy
            .private_allowlist
            .iter()
            .any(|cidr| cidr.contains(ip))
}

type Lookup = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>
        + Send
        + Sync,
>;

fn system_lookup() -> Lookup {
    Arc::new(|host: String| {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .map(|addr| addr.ip())
                .collect())
        })
    })
}

/// Resolves only the configured host and drops every refused address. It
/// runs for every new connection, so a later answer that rebinds the name
/// to a refused address is filtered exactly like the first one.
struct FilteringResolver {
    host: String,
    policy: AddressPolicy,
    lookup: Lookup,
}

impl FilteringResolver {
    async fn allowed(
        &self,
        requested: String,
    ) -> Result<Vec<IpAddr>, Box<dyn std::error::Error + Send + Sync>> {
        if requested != self.host {
            return Err(
                format!("resolution of `{requested}` refused: not the configured host").into(),
            );
        }
        let allowed: Vec<IpAddr> = (self.lookup)(requested)
            .await?
            .into_iter()
            .filter(|ip| address_allowed(*ip, &self.policy))
            .collect();
        if allowed.is_empty() {
            return Err(AddressRefused.into());
        }
        Ok(allowed)
    }
}

impl Resolve for FilteringResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let requested = name.as_str().to_ascii_lowercase();
        let resolver = FilteringResolver {
            host: self.host.clone(),
            policy: self.policy.clone(),
            lookup: Arc::clone(&self.lookup),
        };
        Box::pin(async move {
            let allowed = resolver.allowed(requested).await?;
            Ok(Box::new(allowed.into_iter().map(|ip| SocketAddr::new(ip, 0))) as Addrs)
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
        if !address_allowed(ip, &policy) {
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
            lookup: system_lookup(),
        }))
        .build()
        .map_err(|error| format!("building HTTP client: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn strict() -> AddressPolicy {
        AddressPolicy::default()
    }

    fn pinned(blocks: &[&str]) -> AddressPolicy {
        AddressPolicy {
            private_allowlist: blocks
                .iter()
                .map(|b| Cidr::parse_private(b).unwrap())
                .collect(),
            allow_loopback: false,
        }
    }

    fn ip(raw: &str) -> IpAddr {
        raw.parse().unwrap()
    }

    #[test]
    fn special_ranges_are_refused_even_with_an_exception() {
        let wide = pinned(&["10.0.0.0/16", "192.168.0.0/16", "fd00::/48"]);
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
            assert!(!address_allowed(ip(refused), &wide), "{refused} allowed");
        }
        for public in ["188.114.96.1", "1.1.1.1", "2a06:98c1:3120::1"] {
            assert!(address_allowed(ip(public), &strict()), "{public}");
        }
        assert!(address_allowed(
            ip("127.0.0.1"),
            &AddressPolicy {
                private_allowlist: Vec::new(),
                allow_loopback: true
            }
        ));
    }

    /// Owner Q2: private egress is denied by default and an exception is
    /// limited to the approved block.
    #[test]
    fn private_destinations_need_a_pinned_exception() {
        for private in [
            "10.0.0.5",
            "172.16.0.1",
            "192.168.1.50",
            "100.64.0.1",
            "fd00::5",
            "::ffff:10.0.0.5",
        ] {
            assert!(
                !address_allowed(ip(private), &strict()),
                "{private} allowed by default"
            );
        }
        let lan = pinned(&["192.168.1.50/32"]);
        assert!(address_allowed(ip("192.168.1.50"), &lan));
        assert!(address_allowed(ip("::ffff:192.168.1.50"), &lan));
        for outside in ["192.168.1.1", "192.168.1.51", "10.0.0.5", "fd00::5"] {
            assert!(
                !address_allowed(ip(outside), &lan),
                "{outside} outside the exception allowed"
            );
        }
        let ula = pinned(&["fd42:42:42::/48"]);
        assert!(address_allowed(ip("fd42:42:42::9"), &ula));
        assert!(!address_allowed(ip("fd42:42:43::9"), &ula));
    }

    #[test]
    fn exceptions_cannot_open_public_or_special_space() {
        for refused in [
            "0.0.0.0/0",
            "10.0.0.0/8",
            "192.168.0.0/15",
            "127.0.0.0/16",
            "169.254.0.0/16",
            "8.8.8.0/24",
            "192.168.1.1/24",
            "fd00::/8",
            "fe80::/64",
            "::/0",
            "192.168.1.1",
            "192.168.1.1/33",
            "nonsense/16",
        ] {
            assert!(Cidr::parse_private(refused).is_err(), "{refused} accepted");
        }
        assert!(Cidr::parse_private("192.168.1.0/24").is_ok());
        assert!(Cidr::parse_private("100.64.5.0/24").is_ok());
    }

    /// A lookup that answers from a script, one answer per call.
    fn scripted(answers: Vec<Vec<&'static str>>) -> Lookup {
        let answers = Arc::new(Mutex::new(answers.into_iter()));
        Arc::new(move |_host: String| {
            let next = answers.lock().unwrap().next().unwrap_or_default();
            Box::pin(async move { Ok(next.into_iter().map(|raw| raw.parse().unwrap()).collect()) })
        })
    }

    fn resolver(policy: AddressPolicy, lookup: Lookup) -> FilteringResolver {
        FilteringResolver {
            host: "provider.example".to_owned(),
            policy,
            lookup,
        }
    }

    async fn resolve(resolver: &FilteringResolver, name: &str) -> Result<Vec<IpAddr>, String> {
        resolver
            .allowed(name.to_owned())
            .await
            .map_err(|error| error.to_string())
    }

    /// DNS rebinding: every connection re-resolves through the filter, so a
    /// later answer pointing at metadata, loopback or an unapproved private
    /// address is refused even after an earlier public answer.
    #[tokio::test]
    async fn rebinding_to_a_refused_address_is_filtered_on_every_resolution() {
        let rebinding = resolver(
            strict(),
            scripted(vec![
                vec!["188.114.96.1"],
                vec!["169.254.169.254"],
                vec!["127.0.0.1"],
                vec!["192.168.1.50"],
                vec!["10.0.0.1", "188.114.97.1"],
            ]),
        );
        assert_eq!(
            resolve(&rebinding, "provider.example").await.unwrap(),
            [ip("188.114.96.1")]
        );
        for _ in 0..3 {
            let refused = resolve(&rebinding, "provider.example").await.unwrap_err();
            assert!(
                refused.contains("refused by the address policy"),
                "{refused}"
            );
        }
        // A mixed answer keeps only the permitted address.
        assert_eq!(
            resolve(&rebinding, "provider.example").await.unwrap(),
            [ip("188.114.97.1")]
        );

        // With a pinned exception, rebinding inside private space still
        // cannot leave the approved block.
        let lan = resolver(
            pinned(&["192.168.1.50/32"]),
            scripted(vec![
                vec!["192.168.1.50"],
                vec!["192.168.1.1"],
                vec!["10.0.0.5"],
            ]),
        );
        assert_eq!(
            resolve(&lan, "provider.example").await.unwrap(),
            [ip("192.168.1.50")]
        );
        assert!(resolve(&lan, "provider.example").await.is_err());
        assert!(resolve(&lan, "provider.example").await.is_err());
    }

    #[tokio::test]
    async fn only_the_configured_host_is_resolved() {
        let calls = Arc::new(Mutex::new(0));
        let counted = Arc::clone(&calls);
        let lookup: Lookup = Arc::new(move |_host: String| {
            *counted.lock().unwrap() += 1;
            Box::pin(async { Ok(vec!["188.114.96.1".parse().unwrap()]) })
        });
        let resolver = resolver(strict(), lookup);
        for other in [
            "evil.example",
            "provider.example.evil.example",
            "169.254.169.254",
        ] {
            assert!(resolve(&resolver, other).await.is_err(), "{other}");
        }
        assert_eq!(*calls.lock().unwrap(), 0, "a foreign name reached DNS");
        assert!(resolve(&resolver, "provider.example").await.is_ok());
    }

    #[tokio::test]
    async fn the_system_lookup_is_filtered_too() {
        // localhost resolves only to loopback, which the policy refuses.
        let resolver = FilteringResolver {
            host: "localhost".to_owned(),
            policy: strict(),
            lookup: system_lookup(),
        };
        let error = resolver
            .allowed("localhost".to_owned())
            .await
            .expect_err("loopback refused");
        assert!(error.downcast_ref::<AddressRefused>().is_some(), "{error}");
    }

    #[test]
    fn environment_trust_overrides_are_refused() {
        assert!(trust_overrides_absent(|_| false).is_ok());
        for name in TRUST_OVERRIDES {
            let refused = trust_overrides_absent(|set| set == name).unwrap_err();
            assert!(refused.contains(name), "{refused}");
        }
    }
}
