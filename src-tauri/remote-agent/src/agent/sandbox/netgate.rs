//! The network policy, applied outside the sandbox to every connection a cell
//! asks for.
//!
//! A host is allowed or refused by name first ([`Gate::decide`]). An allowed
//! name is then resolved here, not in the sandbox, and connected to only at
//! addresses that are public: a name that resolves to loopback, a private or
//! carrier-grade NAT network, a link-local or cloud metadata address is
//! refused unless the policy names that very address (`127.0.0.1:5432`,
//! `localhost:3000`). Otherwise any allowed name could be pointed at the
//! machine's own services, or at the host's by way of an SSH machine's LAN,
//! with one DNS record.
//!
//! Matching is by name only, as it is in every proxy that does not decrypt:
//! allowing `github.com` allows anything reachable through github.com.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::protocol::{NetworkMode, NetworkPolicy};

/// How long one connection attempt may take, across all of a name's addresses.
const CONNECT_BUDGET: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
pub struct Gate {
    policy: NetworkPolicy,
}

impl Gate {
    pub fn new(policy: NetworkPolicy) -> Self {
        Self { policy }
    }

    /// Whether `host:port` may be connected to at all; the reason if not.
    pub fn decide(&self, host: &str, port: u16) -> Result<(), String> {
        let host = normalize_host(host).ok_or_else(|| format!("{host} is not a valid host name"))?;
        if port == 0 {
            return Err("port 0 is not a destination".into());
        }
        if self.policy.deny.iter().any(|pattern| matches(pattern, &host, port)) {
            return Err(format!("{host} is on this sandbox's network deny list"));
        }
        match self.policy.mode {
            NetworkMode::Off => Err("this sandbox has no network access".into()),
            NetworkMode::Open => Ok(()),
            NetworkMode::Allowlist => {
                if self.policy.allow.iter().any(|pattern| matches(pattern, &host, port)) {
                    Ok(())
                } else {
                    Err(format!(
                        "{host} is not on this sandbox's network allowlist (add it in Mework's sandbox settings)"
                    ))
                }
            }
        }
    }

    /// Connects to `host:port` if the policy allows it, at a public address
    /// unless the policy names a non-public one itself.
    pub fn connect(&self, host: &str, port: u16) -> Result<TcpStream, String> {
        self.decide(host, port)?;
        let name = normalize_host(host).expect("decide checked it");
        let explicit = self.names_address_explicitly(&name, port);
        let addresses: Vec<SocketAddr> = if name == "localhost" {
            vec![
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
            ]
        } else {
            let literal = name.trim_start_matches('[').trim_end_matches(']');
            match literal.parse::<IpAddr>() {
                Ok(ip) => vec![SocketAddr::new(ip, port)],
                Err(_) => (name.as_str(), port)
                    .to_socket_addrs()
                    .map_err(|error| format!("cannot resolve {name}: {error}"))?
                    .collect(),
            }
        };
        let usable: Vec<SocketAddr> = addresses
            .into_iter()
            .filter(|address| explicit || is_public(address.ip()))
            .collect();
        if usable.is_empty() {
            return Err(format!(
                "{name} resolves only to local or private addresses, which the sandbox does not connect to unless its allowlist names them"
            ));
        }
        let deadline = Instant::now() + CONNECT_BUDGET;
        let mut last_error = String::new();
        for (index, address) in usable.iter().enumerate() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            // Leave the later addresses some of the budget.
            let share = remaining / (usable.len() - index) as u32;
            match TcpStream::connect_timeout(address, share.max(Duration::from_secs(2)).min(remaining)) {
                Ok(stream) => {
                    let _ = stream.set_nodelay(true);
                    return Ok(stream);
                }
                Err(error) => last_error = format!("{address}: {error}"),
            }
        }
        Err(format!("cannot connect to {name}:{port}: {last_error}"))
    }

    /// Whether an allow entry names this host as `localhost` or an address
    /// literal, which is the only way to reach a non-public address.
    fn names_address_explicitly(&self, host: &str, port: u16) -> bool {
        let literal = host == "localhost"
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .is_ok();
        literal
            && self.policy.mode != NetworkMode::Off
            && self.policy.allow.iter().any(|pattern| {
                let (name, _) = split_pattern(pattern);
                name != "*" && !name.starts_with("*.") && matches(pattern, host, port)
            })
    }
}

/// Lower-cased, without a trailing dot, IPv6 in brackets; `None` for
/// anything that is not a host name or address.
fn normalize_host(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.len() > 253 {
        return None;
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(IpAddr::V6(v6)) = bare.parse::<IpAddr>() {
        return Some(format!("[{v6}]"));
    }
    if bare.parse::<IpAddr>().is_ok() {
        return Some(bare.to_owned());
    }
    let valid = host
        .split('.')
        .all(|label| !label.is_empty() && label.len() <= 63 && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    valid.then_some(host)
}

/// `name[:port]`, with IPv6 names in brackets.
fn split_pattern(pattern: &str) -> (String, Option<u16>) {
    let pattern = pattern.trim().to_ascii_lowercase();
    if let Some(rest) = pattern.strip_prefix('[') {
        if let Some((address, tail)) = rest.split_once(']') {
            let port = tail.strip_prefix(':').and_then(|port| port.parse().ok());
            let name = address
                .parse::<Ipv6Addr>()
                .map(|v6| format!("[{v6}]"))
                .unwrap_or_else(|_| format!("[{address}]"));
            return (name, port);
        }
    }
    match pattern.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') => match port.parse() {
            Ok(port) => (name.trim_end_matches('.').to_owned(), Some(port)),
            Err(_) => (pattern.clone(), None),
        },
        _ => (pattern.trim_end_matches('.').to_owned(), None),
    }
}

/// Whether `pattern` covers `host:port`. `host` is already normalized.
pub fn matches(pattern: &str, host: &str, port: u16) -> bool {
    let (name, wanted_port) = split_pattern(pattern);
    if wanted_port.is_some_and(|wanted| wanted != port) {
        return false;
    }
    let is_address = host.starts_with('[') || host.parse::<IpAddr>().is_ok();
    if name == "*" {
        return true;
    }
    if let Some(suffix) = name.strip_prefix("*.") {
        // Subdomains only, and never an address that happens to end in the
        // same digits.
        return !is_address && host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}"));
    }
    name == host
}

/// Whether an address is on the public internet: not loopback, private,
/// shared (carrier-grade NAT, which Tailscale uses), link-local (cloud
/// metadata), multicast, documentation or reserved.
pub fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let segments = v6.segments();
            // The deprecated IPv4-compatible form, `::a.b.c.d`, reaches the
            // same IPv4 address on stacks that still honour it.
            if segments[..6] == [0, 0, 0, 0, 0, 0] && !v6.is_loopback() && !v6.is_unspecified() {
                let [a, b] = segments[6].to_be_bytes();
                let [c, d] = segments[7].to_be_bytes();
                return is_public_v4(Ipv4Addr::new(a, b, c, d));
            }
            // NAT64 carries an IPv4 address in its last 32 bits.
            if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                let [a, b] = segments[6].to_be_bytes();
                let [c, d] = segments[7].to_be_bytes();
                return is_public_v4(Ipv4Addr::new(a, b, c, d));
            }
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
                || segments[0] == 0x2001 && segments[1] == 0x0db8
                || segments[0] == 0x0100 && segments[1..4] == [0, 0, 0])
        }
    }
}

fn is_public_v4(v4: Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (18..=19).contains(&b))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224
        // Azure's platform endpoint answers every VM like a metadata service.
        || v4 == Ipv4Addr::new(168, 63, 129, 16))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(mode: NetworkMode, allow: &[&str], deny: &[&str]) -> Gate {
        Gate::new(NetworkPolicy {
            mode,
            allow: allow.iter().map(|s| (*s).to_owned()).collect(),
            deny: deny.iter().map(|s| (*s).to_owned()).collect(),
        })
    }

    #[test]
    fn names_are_matched_exactly_by_subdomain_or_by_port() {
        assert!(matches("github.com", "github.com", 443));
        assert!(!matches("github.com", "api.github.com", 443));
        assert!(matches("*.github.com", "api.github.com", 443));
        assert!(!matches("*.github.com", "github.com", 443));
        assert!(!matches("*.github.com", "evilgithub.com", 443));
        assert!(matches("registry.npmjs.org:443", "registry.npmjs.org", 443));
        assert!(!matches("registry.npmjs.org:443", "registry.npmjs.org", 80));
        assert!(matches("*", "anything.example", 1));
        assert!(matches("[::1]:8080", "[::1]", 8080));
        assert!(!matches("*.0.0.1", "127.0.0.1", 80));
        assert!(matches("GitHub.com.", "github.com", 443));
    }

    #[test]
    fn the_mode_and_the_deny_list_decide_first() {
        assert!(gate(NetworkMode::Off, &["*"], &[]).decide("example.com", 443).is_err());
        assert!(gate(NetworkMode::Open, &[], &[]).decide("example.com", 443).is_ok());
        assert!(gate(NetworkMode::Open, &[], &["example.com"]).decide("example.com", 443).is_err());
        let allowlist = gate(NetworkMode::Allowlist, &["*.npmjs.org"], &[]);
        assert!(allowlist.decide("registry.npmjs.org", 443).is_ok());
        let refused = allowlist.decide("evil.example", 443).unwrap_err();
        assert!(refused.contains("allowlist"), "{refused}");
        assert!(allowlist.decide("bad host", 443).is_err());
    }

    #[test]
    fn private_and_metadata_addresses_are_not_public() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.100.100.200",
            "100.126.178.91",
            "0.0.0.0",
            "224.0.0.1",
            "168.63.129.16",
            "::1",
            "fd00:ec2::254",
            "fe80::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
            "::7f00:1",
            "::a00:1",
            "192.88.99.1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in ["1.1.1.1", "140.82.112.3", "2606:4700::1111", "64:ff9b::101:101"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn a_name_that_resolves_to_loopback_is_refused_unless_named_itself() {
        let open = gate(NetworkMode::Open, &[], &[]);
        let error = open.connect("localhost", 9).unwrap_err();
        assert!(error.contains("local or private"), "{error}");
        let error = open.connect("127.0.0.1", 9).unwrap_err();
        assert!(error.contains("local or private"), "{error}");
        // Named explicitly, the address is tried (and here nothing listens).
        let named = gate(NetworkMode::Allowlist, &["127.0.0.1:9"], &[]);
        let error = named.connect("127.0.0.1", 9).unwrap_err();
        assert!(error.contains("cannot connect"), "{error}");
    }

    #[test]
    fn an_explicitly_allowed_loopback_port_is_reached() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let named = gate(NetworkMode::Allowlist, &[&format!("localhost:{port}")], &[]);
        assert!(named.connect("localhost", port).is_ok());
        let wildcard = gate(NetworkMode::Allowlist, &["*"], &[]);
        assert!(wildcard.connect("localhost", port).is_err());
    }
}
