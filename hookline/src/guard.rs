//! Deciding whether a destination URL is safe to request.
//!
//! A webhook sender makes HTTP requests to addresses its *users* choose, from
//! inside your network. That is the definition of server-side request forgery,
//! and a homegrown sender is usually one `reqwest::get(user_url)` with none of
//! the checks below. The ones that get exploited in practice:
//!
//! * `http://169.254.169.254/` — the cloud metadata service. On a default EC2
//!   or GCE instance this hands out credentials.
//! * `http://127.0.0.1:6379/` — a Redis, Elasticsearch or admin port that is
//!   unauthenticated because it only listens on loopback.
//! * `http://[::ffff:127.0.0.1]/` — the same address written as an
//!   IPv4-mapped IPv6 address, which slips past a check that only knows about
//!   dotted quads.
//! * A hostname that resolves to a public address when validated and a private
//!   one when connected: **DNS rebinding**. Checking the URL is not enough;
//!   the address that is *connected to* has to be the one that was checked,
//!   which is why [`Destination`] carries resolved addresses and the client
//!   is pinned to them.
//! * A public URL that redirects to a private one. Redirects are not followed.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use url::Url;

/// What a deployment is willing to send to.
///
/// The default refuses everything worth refusing: plaintext HTTP, and any
/// address that is not on the public internet.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    /// Allow plain `http://`. Off by default: a webhook carries a signature
    /// over a payload that is often not meant to be public.
    pub allow_http: bool,
    /// Allow loopback, private and link-local destinations.
    ///
    /// Off by default, and the single most important line in this file. On for
    /// local development and for a deployment whose consumers genuinely are
    /// inside the same network.
    pub allow_private: bool,
    /// Ports that may be used. Empty means any port.
    pub allowed_ports: Vec<u16>,
    /// Hostnames that are always refused, matched on the host and any parent
    /// domain, so `evil.example.com` is caught by `example.com`.
    pub denied_hosts: Vec<String>,
}

impl Policy {
    /// A policy for tests and local development: anything goes.
    pub fn permissive() -> Policy {
        Policy {
            allow_http: true,
            allow_private: true,
            ..Policy::default()
        }
    }
}

/// Why a URL was refused. The message is shown to whoever configured the
/// endpoint, so each says what to do about it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Rejected {
    NotAUrl(String),
    Scheme(String),
    NoHost,
    Credentials,
    Port(u16),
    DeniedHost(String),
    /// The host resolved to an address that is not routable on the public
    /// internet.
    PrivateAddress {
        host: String,
        addr: IpAddr,
        why: &'static str,
    },
    Unresolvable(String),
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejected::NotAUrl(e) => write!(f, "not a valid URL: {}", e),
            Rejected::Scheme(s) => write!(
                f,
                "the scheme `{}` is not allowed; use https (http requires allow_http)",
                s
            ),
            Rejected::NoHost => f.write_str("the URL has no host"),
            Rejected::Credentials => {
                f.write_str("the URL contains a username or password; put credentials in a header")
            }
            Rejected::Port(p) => write!(f, "port {} is not in the allowed list", p),
            Rejected::DeniedHost(h) => write!(f, "`{}` is on the deny list", h),
            Rejected::PrivateAddress { host, addr, why } => write!(
                f,
                "`{}` resolves to {}, which is {}; set allow_private to send there",
                host, addr, why
            ),
            Rejected::Unresolvable(h) => write!(f, "`{}` does not resolve", h),
        }
    }
}

impl std::error::Error for Rejected {}

/// A URL that passed the policy, together with the addresses it resolved to.
///
/// The addresses are carried rather than re-resolved, because re-resolving is
/// exactly the DNS rebinding hole: the name could answer differently the
/// second time. The HTTP client connects to these and nothing else.
#[derive(Clone, Debug)]
pub struct Destination {
    pub url: Url,
    pub host: String,
    pub port: u16,
    pub addrs: Vec<SocketAddr>,
}

/// Check a URL's shape against the policy, without touching DNS.
///
/// Called when an endpoint is created, so a bad URL is a 400 at that moment
/// rather than a delivery failure hours later.
pub fn check_url(raw: &str, policy: &Policy) -> Result<Url, Rejected> {
    let url = Url::parse(raw).map_err(|e| Rejected::NotAUrl(e.to_string()))?;

    match url.scheme() {
        "https" => {}
        "http" if policy.allow_http => {}
        other => return Err(Rejected::Scheme(other.to_string())),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Rejected::Credentials);
    }
    let host = url.host_str().ok_or(Rejected::NoHost)?.to_string();
    if host.is_empty() {
        return Err(Rejected::NoHost);
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| Rejected::Scheme(url.scheme().to_string()))?;
    if !policy.allowed_ports.is_empty() && !policy.allowed_ports.contains(&port) {
        return Err(Rejected::Port(port));
    }
    let lowered = host.to_ascii_lowercase();
    for denied in &policy.denied_hosts {
        let denied = denied.to_ascii_lowercase();
        if lowered == denied || lowered.ends_with(&format!(".{}", denied)) {
            return Err(Rejected::DeniedHost(host));
        }
    }
    // A literal address in the URL can be judged now; a name needs DNS.
    if let Ok(ip) = lowered.trim_matches(['[', ']']).parse::<IpAddr>() {
        if !policy.allow_private {
            if let Some(why) = forbidden(ip) {
                return Err(Rejected::PrivateAddress {
                    host,
                    addr: ip,
                    why,
                });
            }
        }
    }
    Ok(url)
}

/// Check a URL and resolve it, refusing every address the policy forbids.
///
/// Blocking: call it from a blocking context. Every resolved address is
/// checked, not just the first — a name that answers with one public and one
/// private address must not get through on the strength of the public one.
pub fn resolve(raw: &str, policy: &Policy) -> Result<Destination, Rejected> {
    let url = check_url(raw, policy)?;
    let host = url.host_str().ok_or(Rejected::NoHost)?.to_string();
    let port = url.port_or_known_default().unwrap_or(443);

    let addrs: Vec<SocketAddr> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|_| Rejected::Unresolvable(host.clone()))?
        .collect();
    if addrs.is_empty() {
        return Err(Rejected::Unresolvable(host));
    }
    if !policy.allow_private {
        for addr in &addrs {
            if let Some(why) = forbidden(addr.ip()) {
                return Err(Rejected::PrivateAddress {
                    host,
                    addr: addr.ip(),
                    why,
                });
            }
        }
    }
    Ok(Destination {
        url,
        host,
        port,
        addrs,
    })
}

/// Why an address is not somewhere a webhook should go, or `None` if it is
/// ordinary public internet.
pub fn forbidden(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => forbidden_v4(v4),
        IpAddr::V6(v6) => {
            // An IPv4-mapped address is an IPv4 address wearing a hat.
            // `[::ffff:127.0.0.1]` is loopback and has to be judged as one.
            if let Some(v4) = to_ipv4_mapped(v6) {
                return forbidden_v4(v4);
            }
            forbidden_v6(v6)
        }
    }
}

fn forbidden_v4(ip: Ipv4Addr) -> Option<&'static str> {
    let [a, b, c, _] = ip.octets();
    Some(match () {
        _ if ip.is_unspecified() => "the unspecified address",
        _ if ip.is_loopback() => "loopback",
        _ if ip.is_link_local() => "link-local, where cloud metadata lives",
        _ if ip.is_private() => "a private network",
        _ if ip.is_broadcast() => "the broadcast address",
        _ if ip.is_multicast() => "multicast",
        _ if ip.is_documentation() => "reserved for documentation",
        // Carrier-grade NAT, 100.64.0.0/10.
        _ if a == 100 && (64..128).contains(&b) => "carrier-grade NAT space",
        // IETF protocol assignments, 192.0.0.0/24.
        _ if a == 192 && b == 0 && c == 0 => "reserved for IETF protocol assignments",
        // 6to4 relay anycast, 192.88.99.0/24.
        _ if a == 192 && b == 88 && c == 99 => "the deprecated 6to4 relay range",
        // Benchmarking, 198.18.0.0/15.
        _ if a == 198 && (b == 18 || b == 19) => "reserved for benchmarking",
        // 240.0.0.0/4, reserved for future use.
        _ if a >= 240 => "reserved",
        _ => return None,
    })
}

fn forbidden_v6(ip: Ipv6Addr) -> Option<&'static str> {
    let segments = ip.segments();
    Some(match () {
        _ if ip.is_unspecified() => "the unspecified address",
        _ if ip.is_loopback() => "loopback",
        _ if ip.is_multicast() => "multicast",
        // Unique local addresses, fc00::/7.
        _ if segments[0] & 0xfe00 == 0xfc00 => "a unique local address",
        // Link-local unicast, fe80::/10.
        _ if segments[0] & 0xffc0 == 0xfe80 => "link-local",
        // Documentation, 2001:db8::/32.
        _ if segments[0] == 0x2001 && segments[1] == 0x0db8 => "reserved for documentation",
        // Discard-only, 100::/64.
        _ if segments[0] == 0x0100 && segments[1..4] == [0, 0, 0] => "the discard-only range",
        // NAT64 well-known prefix, 64:ff9b::/96 — an IPv4 address in disguise.
        _ if segments[0] == 0x0064 && segments[1] == 0xff9b => "the NAT64 prefix",
        _ => return None,
    })
}

/// The IPv4 address inside an IPv4-mapped IPv6 address.
///
/// `Ipv6Addr::to_ipv4_mapped` is stable, but doing it by hand keeps the
/// mapping visible next to the reason it matters.
fn to_ipv4_mapped(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    (s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0xffff).then(|| {
        Ipv4Addr::new(
            (s[6] >> 8) as u8,
            (s[6] & 0xff) as u8,
            (s[7] >> 8) as u8,
            (s[7] & 0xff) as u8,
        )
    })
}

/// A DNS resolver that applies the policy, for the HTTP client to use.
///
/// Filtering here rather than before the request closes the last gap. Checking
/// a name and then handing the *name* to the client leaves a window in which
/// the answer can change between the check and the connection; a resolver that
/// refuses forbidden addresses is consulted by the connection itself, so the
/// address that is checked is the address that is dialled, always.
pub struct Resolver {
    policy: Policy,
}

impl Resolver {
    pub fn new(policy: Policy) -> Resolver {
        Resolver { policy }
    }
}

impl reqwest::dns::Resolve for Resolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let allow_private = self.policy.allow_private;
        Box::pin(async move {
            // Port zero: reqwest replaces it with the one from the URL.
            let lookup = tokio::task::spawn_blocking(move || {
                (host.as_str(), 0u16)
                    .to_socket_addrs()
                    .map(|it| it.collect::<Vec<_>>())
                    .map_err(|_| Rejected::Unresolvable(host.clone()))
                    .and_then(|addrs| {
                        if addrs.is_empty() {
                            return Err(Rejected::Unresolvable(host.clone()));
                        }
                        if !allow_private {
                            // Every address, not just the first: a name that
                            // answers with one public and one private address
                            // must not get through on the public one.
                            for addr in &addrs {
                                if let Some(why) = forbidden(addr.ip()) {
                                    return Err(Rejected::PrivateAddress {
                                        host: host.clone(),
                                        addr: addr.ip(),
                                        why,
                                    });
                                }
                            }
                        }
                        Ok(addrs)
                    })
            })
            .await
            .map_err(|e| {
                Box::new(std::io::Error::other(e.to_string())) as crate::guard::BoxError
            })?;

            let addrs = lookup.map_err(|e| Box::new(e) as crate::guard::BoxError)?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    fn strict() -> Policy {
        Policy::default()
    }

    #[test]
    fn only_https_by_default() {
        assert!(check_url("https://example.com/hook", &strict()).is_ok());
        assert!(matches!(
            check_url("http://example.com/hook", &strict()),
            Err(Rejected::Scheme(_))
        ));
        assert!(check_url(
            "http://example.com/hook",
            &Policy {
                allow_http: true,
                ..strict()
            }
        )
        .is_ok());
        for bad in [
            "file:///etc/passwd",
            "gopher://example.com",
            "ftp://example.com",
        ] {
            assert!(
                matches!(check_url(bad, &strict()), Err(Rejected::Scheme(_))),
                "{}",
                bad
            );
        }
    }

    #[test]
    fn credentials_in_the_url_are_refused() {
        // They end up in logs, in the audit trail, and in screenshots.
        assert_eq!(
            check_url("https://user:pass@example.com/", &strict()),
            Err(Rejected::Credentials)
        );
        assert_eq!(
            check_url("https://user@example.com/", &strict()),
            Err(Rejected::Credentials)
        );
    }

    #[test]
    fn the_metadata_service_is_unreachable() {
        // The single most exploited SSRF target there is.
        let e = check_url("https://169.254.169.254/latest/meta-data/", &strict());
        assert!(matches!(e, Err(Rejected::PrivateAddress { .. })), "{:?}", e);
    }

    #[test]
    fn private_and_loopback_literals_are_refused() {
        for host in [
            "127.0.0.1",
            "127.1.2.3",
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "172.31.255.255",
            "0.0.0.0",
            "169.254.1.1",
            "100.64.0.1",
            "192.0.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "192.0.2.1",
        ] {
            let url = format!("https://{}/hook", host);
            assert!(
                matches!(
                    check_url(&url, &strict()),
                    Err(Rejected::PrivateAddress { .. })
                ),
                "{} was allowed",
                host
            );
            assert!(check_url(&url, &Policy::permissive()).is_ok(), "{}", host);
        }
    }

    #[test]
    fn the_ipv6_spellings_of_loopback_are_also_refused() {
        // Each of these is the same machine, and a check that only understands
        // dotted quads lets all of them through.
        for host in [
            "[::1]",
            "[::ffff:127.0.0.1]",
            "[::ffff:7f00:1]",
            "[fe80::1]",
            "[fc00::1]",
            "[fd12:3456::1]",
            "[::]",
            "[64:ff9b::7f00:1]",
            "[2001:db8::1]",
        ] {
            let url = format!("https://{}/hook", host);
            assert!(
                matches!(
                    check_url(&url, &strict()),
                    Err(Rejected::PrivateAddress { .. })
                ),
                "{} was allowed",
                host
            );
        }
    }

    #[test]
    fn ordinary_public_addresses_are_allowed() {
        for host in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "[2606:4700::1111]",
            "example.com",
        ] {
            let url = format!("https://{}/hook", host);
            assert!(check_url(&url, &strict()).is_ok(), "{} was refused", host);
        }
    }

    #[test]
    fn the_deny_list_covers_subdomains() {
        let policy = Policy {
            denied_hosts: vec!["internal.example".into()],
            ..strict()
        };
        assert!(matches!(
            check_url("https://internal.example/x", &policy),
            Err(Rejected::DeniedHost(_))
        ));
        assert!(matches!(
            check_url("https://api.internal.example/x", &policy),
            Err(Rejected::DeniedHost(_))
        ));
        // ... but not a name that merely ends with the same letters.
        assert!(check_url("https://notinternal.example/x", &policy).is_ok());
    }

    #[test]
    fn ports_can_be_restricted() {
        let policy = Policy {
            allowed_ports: vec![443],
            ..strict()
        };
        assert!(check_url("https://example.com/x", &policy).is_ok());
        assert_eq!(
            check_url("https://example.com:8443/x", &policy),
            Err(Rejected::Port(8443))
        );
        // With no list, any port is fine.
        assert!(check_url("https://example.com:8443/x", &strict()).is_ok());
    }

    #[test]
    fn resolution_checks_every_address_it_gets() {
        // localhost resolves to 127.0.0.1 and often ::1 as well; both are
        // forbidden, and the check must not stop at the first.
        let e = resolve("https://localhost/hook", &strict());
        assert!(matches!(e, Err(Rejected::PrivateAddress { .. })), "{:?}", e);
        let ok = resolve("https://localhost/hook", &Policy::permissive());
        assert!(ok.is_ok(), "{:?}", ok.err());
        assert!(!ok.unwrap().addrs.is_empty());
    }

    #[test]
    fn a_name_that_does_not_resolve_says_so() {
        let e = resolve("https://no-such-host.invalid/hook", &Policy::permissive());
        assert!(matches!(e, Err(Rejected::Unresolvable(_))), "{:?}", e);
    }

    #[test]
    fn the_ipv4_mapped_unwrapping_is_exact() {
        assert_eq!(
            to_ipv4_mapped("::ffff:127.0.0.1".parse().unwrap()),
            Some(Ipv4Addr::new(127, 0, 0, 1))
        );
        assert_eq!(
            to_ipv4_mapped("::ffff:8.8.8.8".parse().unwrap()),
            Some(Ipv4Addr::new(8, 8, 8, 8))
        );
        assert_eq!(to_ipv4_mapped("2606:4700::1111".parse().unwrap()), None);
        assert_eq!(to_ipv4_mapped("::1".parse().unwrap()), None);
    }
}
