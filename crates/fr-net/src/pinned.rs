//! Public-address policy and the pinned resolver for web fetch (dsh
//! `web-fetch-http/src/network.ts` face, 1:1).
//!
//! One DNS answer set is validated before any connection is made, and the
//! per-request agent's resolver serves only that set, so the connection
//! cannot re-resolve the hostname to a private address (DNS rebinding).
//! The URL hostname stays intact in the URI, so HTTP Host and TLS SNI
//! keep the domain semantics. Public-ness mirrors dsh's `ipaddr.js` range
//! tables (unicast only; IPv4-mapped IPv6 is classified by its embedded
//! IPv4 address) and the NAT64 discovery follows RFC 7050's reserved
//! `ipv4only.arpa` hostname. The transport half of this module is
//! [`crate::http::UreqFetch::pinned`].

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};

/// RFC 6052 prefix lengths that may carry an IPv4 destination through NAT64.
const RFC6052_PREFIX_LENGTHS: [u8; 6] = [32, 40, 48, 56, 64, 96];
/// RFC 7050's reserved discovery hostname.
const IPV4ONLY_DISCOVERY_HOST: &str = "ipv4only.arpa";
/// The two well-known addresses behind the discovery host.
const IPV4ONLY_SENTINELS: [Ipv4Addr; 2] =
    [Ipv4Addr::new(192, 0, 0, 170), Ipv4Addr::new(192, 0, 0, 171)];

/// One policy failure while validating a DNS answer set; the provider maps
/// each variant to its wire error code and verbatim message.
#[derive(Debug)]
pub enum PinError {
    /// The lookup produced no addresses.
    NoAddresses {
        /// The hostname that resolved to nothing.
        hostname: String,
    },
    /// Some address in the set is not public unicast.
    NonPublic {
        /// The hostname whose answer set was refused.
        hostname: String,
    },
    /// Some address translates through a discovered NAT64 prefix to a
    /// non-public IPv4 destination.
    Nat64NonPublic {
        /// The hostname whose answer set was refused.
        hostname: String,
    },
    /// The NAT64 discovery lookup itself failed.
    Discovery(std::io::Error),
}

impl std::fmt::Display for PinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAddresses { hostname } => {
                write!(f, "hostname \"{hostname}\" resolved to no addresses")
            }
            Self::NonPublic { hostname } => write!(
                f,
                "URL hostname \"{hostname}\" resolves to a non-public IP address"
            ),
            Self::Nat64NonPublic { hostname } => write!(
                f,
                "URL hostname \"{hostname}\" resolves through NAT64 to a non-public IPv4 address"
            ),
            Self::Discovery(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PinError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Discovery(error) => Some(error),
            _ => None,
        }
    }
}

/// Return whether an address is globally reachable unicast. IPv4-mapped
/// IPv6 is classified by its embedded IPv4 address.
#[must_use]
pub fn is_public_ip_address(input: &str) -> bool {
    strip_ipv6_brackets(input)
        .parse::<IpAddr>()
        .is_ok_and(is_public)
}

/// Whether a hostname is an IP literal that [`resolve_public_addresses`]
/// would refuse.
///
/// A proxied hop skips those checks, but a literal needs no resolution:
/// handing it to a proxy on this machine would reach exactly the loopback
/// or private service the checks keep out of reach.
#[must_use]
pub fn is_non_public_ip_literal(hostname: &str) -> bool {
    strip_ipv6_brackets(hostname)
        .parse::<IpAddr>()
        .is_ok_and(|ip| !is_public(ip))
}

/// Resolve a hostname once and reject the complete answer set if any
/// destination is not public. The returned addresses are the only ones the
/// transport may use.
///
/// # Errors
///
/// [`PinError`] when the lookup fails, produces nothing, or contains a
/// non-public (or NAT64-translated non-public) destination.
pub fn resolve_public_addresses(hostname: &str) -> Result<Vec<IpAddr>, PinError> {
    resolve_public_addresses_with(hostname, system_lookup)
}

/// [`resolve_public_addresses`] with the lookup implementation injected
/// (the shape tests use to pin the policy without process DNS).
///
/// # Errors
///
/// [`PinError`] under the same conditions as [`resolve_public_addresses`].
pub fn resolve_public_addresses_with<F>(hostname: &str, lookup: F) -> Result<Vec<IpAddr>, PinError>
where
    F: Fn(&str) -> std::io::Result<Vec<IpAddr>>,
{
    let unbracketed = strip_ipv6_brackets(hostname);
    let literal = unbracketed.parse::<IpAddr>().ok();
    let resolved = match literal {
        Some(ip) => vec![ip],
        None => lookup(unbracketed).map_err(PinError::Discovery)?,
    };

    if resolved.is_empty() {
        return Err(PinError::NoAddresses {
            hostname: hostname.to_owned(),
        });
    }

    let has_ipv6 = resolved.iter().any(|ip| matches!(ip, IpAddr::V6(_)));
    let prefixes = if has_ipv6 {
        discover_nat64_prefixes(&lookup)?
    } else {
        Vec::new()
    };

    for ip in &resolved {
        if !is_public(*ip) {
            return Err(PinError::NonPublic {
                hostname: hostname.to_owned(),
            });
        }
        if let Some(translated) = translated_ipv4_address(*ip, &prefixes)
            && !is_public(IpAddr::V4(translated))
        {
            return Err(PinError::Nat64NonPublic {
                hostname: hostname.to_owned(),
            });
        }
    }
    Ok(resolved)
}

/// One discovered NAT64 prefix (RFC 6052 bytes plus its length).
struct Nat64Prefix {
    bytes: Vec<u8>,
    length: u8,
}

/// Discover the active DNS64 prefix set using RFC 7050's reserved hostname.
fn discover_nat64_prefixes<F>(lookup: F) -> Result<Vec<Nat64Prefix>, PinError>
where
    F: Fn(&str) -> std::io::Result<Vec<IpAddr>>,
{
    let discovered = lookup(IPV4ONLY_DISCOVERY_HOST).map_err(PinError::Discovery)?;
    let mut prefixes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for ip in discovered {
        let IpAddr::V6(v6) = ip else { continue };
        let bytes = v6.octets();
        for length in RFC6052_PREFIX_LENGTHS {
            let Some(embedded) = embedded_ipv4_address(&bytes, length) else {
                continue;
            };
            if !IPV4ONLY_SENTINELS.contains(&embedded) {
                continue;
            }
            let prefix_bytes = bytes[..usize::from(length) / 8].to_vec();
            let key: Vec<String> = std::iter::once(length.to_string())
                .chain(prefix_bytes.iter().map(ToString::to_string))
                .collect();
            if seen.insert(key.join(".")) {
                prefixes.push(Nat64Prefix {
                    bytes: prefix_bytes,
                    length,
                });
            }
        }
    }
    Ok(prefixes)
}

/// Return the RFC 6052-embedded IPv4 address when an address matches a
/// discovered prefix.
fn translated_ipv4_address(ip: IpAddr, prefixes: &[Nat64Prefix]) -> Option<Ipv4Addr> {
    let IpAddr::V6(v6) = ip else {
        return None;
    };
    let bytes = v6.octets();
    for prefix in prefixes {
        if !bytes.starts_with(&prefix.bytes) {
            continue;
        }
        if let Some(embedded) = embedded_ipv4_address(&bytes, prefix.length) {
            return Some(embedded);
        }
    }
    None
}

/// Extract one IPv4 address from an RFC 6052 IPv6 layout.
fn embedded_ipv4_address(bytes: &[u8; 16], prefix_length: u8) -> Option<Ipv4Addr> {
    if prefix_length == 96 {
        return Some(Ipv4Addr::new(bytes[12], bytes[13], bytes[14], bytes[15]));
    }
    if bytes[8] != 0 {
        return None;
    }
    let prefix_bytes = usize::from(prefix_length) / 8;
    let before_reserved_octet = 8 - prefix_bytes;
    let mut ipv4 = [0u8; 4];
    ipv4[..before_reserved_octet]
        .copy_from_slice(&bytes[prefix_bytes..prefix_bytes + before_reserved_octet]);
    let after_reserved_octet = 4 - before_reserved_octet;
    ipv4[before_reserved_octet..].copy_from_slice(&bytes[9..9 + after_reserved_octet]);
    Some(Ipv4Addr::from(ipv4))
}

/// The production lookup: the system resolver over `(host, 0)`, addresses
/// in the order the OS hands them back (dsh `lookup(host, { all: true,
/// order: 'verbatim' })`).
fn system_lookup(host: &str) -> std::io::Result<Vec<IpAddr>> {
    Ok((host, 0).to_socket_addrs()?.map(|addr| addr.ip()).collect())
}

/// WHATWG URL retains brackets around IPv6 hostnames; IP parsers do not.
#[must_use]
pub fn strip_ipv6_brackets(hostname: &str) -> &str {
    hostname
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(hostname)
}

/// Whether the address falls in any `ipaddr.js` special range (`SpecialRanges`,
/// 2.2.0, verbatim): unicast is everything else.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map_or_else(|| is_public_v6(v6), is_public_v4),
    }
}

fn is_public_v4(addr: Ipv4Addr) -> bool {
    let [a, b, c, _] = addr.octets();
    !(a == 0
        || addr.is_broadcast()
        || a & 0xf0 == 0xe0
        || (a, b) == (169, 254)
        || a == 127
        || a == 100 && b & 0xc0 == 64
        || a == 10
        || a == 172 && b & 0xf0 == 16
        || (a, b) == (192, 168)
        || (a, b, c) == (192, 0, 0)
        || (a, b, c) == (192, 0, 2)
        || (a, b, c) == (192, 88, 99)
        || a == 198 && b & 0xfe == 18
        || (a, b, c) == (198, 51, 100)
        || (a, b, c) == (203, 0, 113)
        || a & 0xf0 == 0xf0
        || (a, b, c) == (192, 175, 48)
        || (a, b, c) == (192, 31, 196)
        || (a, b, c) == (192, 52, 193))
}

fn is_public_v6(addr: Ipv6Addr) -> bool {
    let p = addr.segments();
    !(addr.is_unspecified()
        || p[0] & 0xffc0 == 0xfe80
        || p[0] & 0xff00 == 0xff00
        || p == [0, 0, 0, 0, 0, 0, 0, 1]
        || p[0] & 0xfe00 == 0xfc00
        || p == [0, 0, 0, 0, 0, 0xffff, 0, 0]
        || (p[0] == 0x100 && p[1] == 0 && p[2] == 0 && p[3] == 0)
        || p == [0, 0, 0, 0, 0xffff, 0, 0, 0]
        || (p[0] == 0x64 && p[1] == 0xff9b && p[2] == 0 && p[3] == 0 && p[4] == 0 && p[5] == 0)
        || p[0] == 0x2002
        || (p[0] == 0x2001 && p[1] == 0)
        || (p[0] == 0x2001 && p[1] == 2 && p[2] == 0)
        || (p[0] == 0x2001 && p[1] == 3)
        || (p[0] == 0x2001 && p[1] == 4 && p[2] == 0x112)
        || (p[0] == 0x2620 && p[1] == 0x4f && p[2] == 0x8000)
        || (p[0] == 0x2001 && p[1] & 0xfff0 == 0x10)
        || (p[0] == 0x2001 && p[1] & 0xfff0 == 0x20)
        || (p[0] == 0x2001 && p[1] & 0xfff0 == 0x30)
        || (p[0] == 0x2001 && p[1] & 0xfe00 == 0)
        || (p[0] == 0x2001 && p[1] == 0xdb8))
}

/// The per-request resolver serving one validated address set (dsh
/// `createPinnedLookup`): no network resolution happens, the pinned set is
/// the only answer.
#[derive(Debug)]
pub struct PinnedResolver {
    addresses: Vec<IpAddr>,
}

impl PinnedResolver {
    /// Pin one validated address set.
    #[must_use]
    pub const fn new(addresses: Vec<IpAddr>) -> Self {
        Self { addresses }
    }
}

impl Resolver for PinnedResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let (Some(scheme), Some(authority)) = (uri.scheme(), uri.authority()) else {
            return Err(ureq::Error::HostNotFound);
        };
        // The policy layer only admits http/https; the scheme default
        // ports for that pair, spelled out locally (ureq's `SchemeExt`
        // helper is private API).
        let default_port = match scheme.as_str() {
            "http" => Some(80),
            "https" => Some(443),
            _ => None,
        };
        let Some(port) = authority.port_u16().or(default_port) else {
            return Err(ureq::Error::HostNotFound);
        };
        let pinned = self.addresses.iter().map(|ip| SocketAddr::new(*ip, port));
        let mut result = self.empty();
        for addr in config.ip_family().keep_wanted(pinned).take(16) {
            result.push(addr);
        }
        if result.is_empty() {
            Err(ureq::Error::HostNotFound)
        } else {
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Fetch as _;
    use std::io::Write as _;

    /// The unicast policy covers the IPv4 and IPv6 special ranges verbatim
    /// (ipaddr.js 2.2.0 `SpecialRanges`), unwraps IPv4-mapped IPv6 to its
    /// embedded IPv4 classification, and refuses unparseable text.
    #[test]
    fn ip_policy_pins_public_unicast_only() {
        let public = [
            "8.8.8.8",
            "1.0.0.1",
            "9.9.9.9",
            "172.32.0.1",
            "100.128.0.1",
            "198.20.0.1",
            "2606:4700:4700::1111",
            "2600:1000:a5c0:a801:a::",
            "::ffff:8.8.8.8",
            "fec0::1",
        ];
        for address in public {
            assert!(is_public_ip_address(address), "{address} must be public");
        }
        let non_public = [
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.0.5",
            "0.0.0.0",
            "0.1.2.3",
            "100.64.0.1",
            "100.127.255.255",
            "224.0.0.1",
            "239.1.2.3",
            "240.0.0.1",
            "255.255.255.255",
            "192.0.0.170",
            "192.0.2.1",
            "192.88.99.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.1",
            "203.0.113.1",
            "192.175.48.1",
            "192.31.196.1",
            "192.52.193.1",
            "::1",
            "::",
            "fe80::1",
            "febf::1",
            "fc00::1",
            "fd12::1",
            "ff02::1",
            "100::1",
            "::ffff:0:0",
            "::ffff:127.0.0.1",
            "64:ff9b::1.2.3.4",
            "2002::1",
            "2001::1",
            "2001:2::",
            "2001:3::",
            "2001:4:112::",
            "2620:4f:8000::",
            "2001:10::",
            "2001:1f::",
            "2001:30::",
            "2001:100::",
            "2001:db8::1",
            "not-an-ip",
            "",
        ];
        for address in non_public {
            assert!(
                !is_public_ip_address(address),
                "{address} must not be public"
            );
        }
    }

    /// The literal shortcut brackets an IPv6 literal like the resolution
    /// path does, and only fires on parseable non-public literals.
    #[test]
    fn non_public_literal_detection_covers_brackets() {
        assert!(is_non_public_ip_literal("127.0.0.1"));
        assert!(is_non_public_ip_literal("[::1]"));
        assert!(is_non_public_ip_literal("10.0.0.1"));
        assert!(!is_non_public_ip_literal("8.8.8.8"));
        assert!(!is_non_public_ip_literal("example.com"));
        assert!(!is_non_public_ip_literal("10.0.0.256"));
        assert!(!is_non_public_ip_literal(""));
    }

    /// The RFC 6052 layouts extract the embedded IPv4 destination for
    /// every prefix length and refuse a non-zero reserved octet (the
    /// layout for 96 is the exception: the IPv4 bytes start at octet 12).
    #[test]
    fn embedded_ipv4_extraction_pins_all_rfc6052_layouts() {
        for length in RFC6052_PREFIX_LENGTHS {
            let mut bytes = [0u8; 16];
            let prefix_bytes = usize::from(length) / 8;
            if length == 96 {
                bytes[12..16].copy_from_slice(&[9, 9, 9, 9]);
            } else {
                for byte in bytes.iter_mut().take(prefix_bytes) {
                    *byte = 0x20;
                }
                let before_reserved_octet = 8 - prefix_bytes;
                let ipv4 = [9, 9, 9, 9];
                bytes[prefix_bytes..prefix_bytes + before_reserved_octet]
                    .copy_from_slice(&ipv4[..before_reserved_octet]);
                bytes[9..9 + (4 - before_reserved_octet)]
                    .copy_from_slice(&ipv4[before_reserved_octet..]);
            }
            assert_eq!(
                embedded_ipv4_address(&bytes, length),
                Some(Ipv4Addr::new(9, 9, 9, 9)),
                "prefix length {length} must extract the embedded address"
            );
        }
        let mut poisoned_reserved_octet = [0u8; 16];
        poisoned_reserved_octet[..8].fill(0x20);
        poisoned_reserved_octet[8] = 1;
        assert_eq!(
            embedded_ipv4_address(&poisoned_reserved_octet, 32),
            None,
            "a non-zero reserved octet must refuse the layout"
        );
    }

    /// The policy answer for hostname resolution: no addresses and any
    /// non-public member refuse the whole set with the exact dsh messages;
    /// a public set passes verbatim.
    #[test]
    fn resolve_rejects_empty_and_non_public_answers() {
        let empty =
            resolve_public_addresses_with("example.invalid", |_| Ok(Vec::new())).unwrap_err();
        assert!(matches!(empty, PinError::NoAddresses { .. }));
        assert_eq!(
            empty.to_string(),
            "hostname \"example.invalid\" resolved to no addresses"
        );

        let private = resolve_public_addresses_with("example.invalid", |_| {
            Ok(vec![IpAddr::from([10, 0, 0, 7])])
        })
        .unwrap_err();
        assert!(matches!(private, PinError::NonPublic { .. }));
        assert_eq!(
            private.to_string(),
            "URL hostname \"example.invalid\" resolves to a non-public IP address"
        );

        let public = resolve_public_addresses_with("example.invalid", |_| {
            Ok(vec![
                IpAddr::from([8, 8, 8, 8]),
                IpAddr::from([
                    0x26, 0x00, 0x10, 0x00, 0xa5, 0xc0, 0xa8, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0,
                ]),
            ])
        })
        .unwrap();
        assert_eq!(public.len(), 2);
    }

    /// The NAT64 discovery arm: an `ipv4only.arpa` answer whose RFC 6052
    /// embedding carries a sentinel discovers its prefix, and a later
    /// answer under that prefix translating to a non-public IPv4
    /// destination refuses the whole set with the exact dsh message.
    #[test]
    fn nat64_discovery_refuses_non_public_translation() {
        let discovery = IpAddr::from([
            0x26, 0x00, 0x10, 0x00, 0xa5, 0xc0, 0x00, 0x00, 0x00, 0xaa, 0, 0, 0, 0, 0, 0,
        ]);
        let answers = move |host: &str| {
            if host == IPV4ONLY_DISCOVERY_HOST {
                return Ok(vec![discovery]);
            }
            Ok(vec![IpAddr::from([
                0x26, 0x00, 0x10, 0x00, 0xa5, 0xc0, 0xa8, 0x01, 0x00, 0x0a, 0, 0, 0, 0, 0, 0,
            ])])
        };
        let refused = resolve_public_addresses_with("example.invalid", answers).unwrap_err();
        assert!(matches!(refused, PinError::Nat64NonPublic { .. }));
        assert_eq!(
            refused.to_string(),
            "URL hostname \"example.invalid\" resolves through NAT64 to a non-public IPv4 address"
        );
    }

    /// A NAT64 prefix whose translations stay public passes the whole set.
    #[test]
    fn nat64_discovery_allows_public_translation() {
        let discovery = IpAddr::from([
            0x26, 0x00, 0x10, 0x00, 0xa5, 0x08, 0x08, 0x08, 0x00, 0x08, 0, 0, 0, 0, 0, 0,
        ]);
        let answers = move |host: &str| {
            if host == IPV4ONLY_DISCOVERY_HOST {
                return Ok(vec![discovery]);
            }
            Ok(vec![IpAddr::from([
                0x26, 0x00, 0x10, 0x00, 0xa5, 0x08, 0x08, 0x08, 0x00, 0x08, 0, 0, 0, 0, 0, 0,
            ])])
        };
        let public = resolve_public_addresses_with("example.invalid", answers).unwrap();
        assert_eq!(public.len(), 1);
    }

    /// The pinned resolver serves exactly the validated set at the URI's
    /// port, honors the config's family filter, and refuses an empty set
    /// the same way the default resolver refuses an unresolvable host.
    #[test]
    fn pinned_resolver_serves_only_the_validated_set() {
        let resolver = PinnedResolver::new(vec![
            IpAddr::from([8, 8, 8, 8]),
            IpAddr::from([
                0x20, 0x01, 0x48, 0x60, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x88,
            ]),
        ]);
        let uri: ureq::http::Uri = "http://example.invalid:8080/x".parse().unwrap();

        let any = resolver
            .resolve(&uri, &ureq::config::Config::default(), probe_timeout())
            .unwrap();
        assert_eq!(any.len(), 2);
        assert!(any.iter().all(|addr| addr.port() == 8080));

        let v4_only = ureq::Agent::config_builder()
            .ip_family(ureq::config::IpFamily::Ipv4Only)
            .build();
        let filtered = resolver.resolve(&uri, &v4_only, probe_timeout()).unwrap();
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].is_ipv4());

        let empty = PinnedResolver::new(Vec::new())
            .resolve(&uri, &ureq::config::Config::default(), probe_timeout())
            .unwrap_err();
        assert!(matches!(empty, ureq::Error::HostNotFound));
    }

    /// The pinned transport walks the full ureq path against a loopback
    /// server while the URL hostname stays intact: the request reaches the
    /// pinned loopback address carrying the domain Host header.
    #[test]
    fn pinned_transport_roundtrips_loopback_with_domain_host() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let mut buf = [0u8; 1024];
            let read = std::io::Read::read(&mut socket, &mut buf).unwrap_or(0);
            let text = String::from_utf8_lossy(&buf[..read]);
            assert!(
                text.contains(&format!("host: example.invalid:{port}")),
                "the URL hostname must stay intact: {text}"
            );
            let response =
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello";
            socket.write_all(response.as_bytes()).unwrap();
        });

        let fetch = crate::http::UreqFetch::pinned(vec![IpAddr::from([127, 0, 0, 1])]);
        let mut response = fetch
            .fetch(crate::http::HttpRequest {
                method: crate::http::HttpMethod::Get,
                url: format!("http://example.invalid:{port}/hello"),
                headers: Vec::new(),
                body: None,
            })
            .unwrap();
        assert_eq!(response.status, 200);
        let mut body = String::new();
        response.body.read_to_string(&mut body).unwrap();
        assert_eq!(body, "hello");
    }

    fn probe_timeout() -> ureq::unversioned::transport::NextTimeout {
        ureq::unversioned::transport::NextTimeout::default()
    }
}

// Rust guideline compliant 2026-11-01
