//! Parsing of the "target IP" setting and matching the current IP against it.
//!
//! The setting accepts one or more entries separated by commas, semicolons or
//! whitespace. Each entry is either a single address (`203.0.113.10`) or a
//! CIDR network (`198.51.100.0/24`). IPv4 and IPv6 are both supported.

use std::net::IpAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Addr(IpAddr),
    Net(IpAddr, u8),
}

impl Target {
    pub fn matches(&self, ip: &IpAddr) -> bool {
        match self {
            Target::Addr(a) => normalize(a) == normalize(ip),
            Target::Net(net, prefix) => in_network(&normalize(ip), &normalize(net), *prefix),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    /// The IP could not be determined (no network, all providers failed, ...).
    #[default]
    Unknown,
    /// No target configured — nothing to compare against.
    NoTarget,
    Match,
    Mismatch,
}

/// Parses the target setting. An empty / blank string yields an empty list.
pub fn parse_targets(input: &str) -> Result<Vec<Target>, String> {
    let mut out = Vec::new();
    for raw in input.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
        let item = raw.trim();
        if item.is_empty() {
            continue;
        }
        out.push(parse_one(item)?);
    }
    Ok(out)
}

fn parse_one(item: &str) -> Result<Target, String> {
    if let Some((addr, prefix)) = item.split_once('/') {
        let ip: IpAddr = addr
            .trim()
            .parse()
            .map_err(|_| format!("\"{item}\" is not a valid IP address / network"))?;
        let prefix: u8 = prefix
            .trim()
            .parse()
            .map_err(|_| format!("\"{item}\": invalid prefix length"))?;
        let max = if ip.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(format!("\"{item}\": prefix length must be 0..={max}"));
        }
        Ok(Target::Net(ip, prefix))
    } else {
        item.parse::<IpAddr>()
            .map(Target::Addr)
            .map_err(|_| format!("\"{item}\" is not a valid IP address"))
    }
}

pub fn evaluate(ip: Option<&IpAddr>, targets: &[Target]) -> Status {
    match ip {
        None => Status::Unknown,
        Some(_) if targets.is_empty() => Status::NoTarget,
        Some(ip) if targets.iter().any(|t| t.matches(ip)) => Status::Match,
        Some(_) => Status::Mismatch,
    }
}

/// Maps IPv4-mapped IPv6 addresses (::ffff:a.b.c.d) to plain IPv4.
fn normalize(ip: &IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => *ip,
        },
        v4 => *v4,
    }
}

fn in_network(ip: &IpAddr, net: &IpAddr, prefix: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix as u32)
            };
            (u32::from(*a) & mask) == (u32::from(*n) & mask)
        }
        (IpAddr::V6(a), IpAddr::V6(n)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix as u32)
            };
            (u128::from(*a) & mask) == (u128::from(*n) & mask)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn empty_is_no_target() {
        let t = parse_targets("  ").unwrap();
        assert!(t.is_empty());
        assert_eq!(evaluate(Some(&ip("1.2.3.4")), &t), Status::NoTarget);
    }

    #[test]
    fn single_address() {
        let t = parse_targets("203.0.113.10").unwrap();
        assert_eq!(evaluate(Some(&ip("203.0.113.10")), &t), Status::Match);
        assert_eq!(evaluate(Some(&ip("203.0.113.11")), &t), Status::Mismatch);
        assert_eq!(evaluate(None, &t), Status::Unknown);
    }

    #[test]
    fn list_and_cidr() {
        let t = parse_targets("1.1.1.1, 198.51.100.0/24; 2001:db8::/32").unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(evaluate(Some(&ip("1.1.1.1")), &t), Status::Match);
        assert_eq!(evaluate(Some(&ip("198.51.100.200")), &t), Status::Match);
        assert_eq!(evaluate(Some(&ip("198.51.101.1")), &t), Status::Mismatch);
        assert_eq!(evaluate(Some(&ip("2001:db8:1::5")), &t), Status::Match);
        assert_eq!(evaluate(Some(&ip("2001:db9::5")), &t), Status::Mismatch);
    }

    #[test]
    fn prefix_edges() {
        let t = parse_targets("0.0.0.0/0").unwrap();
        assert_eq!(evaluate(Some(&ip("8.8.8.8")), &t), Status::Match);
        let t = parse_targets("8.8.8.8/32").unwrap();
        assert_eq!(evaluate(Some(&ip("8.8.8.8")), &t), Status::Match);
        assert_eq!(evaluate(Some(&ip("8.8.8.9")), &t), Status::Mismatch);
    }

    #[test]
    fn mapped_ipv6_matches_ipv4() {
        let t = parse_targets("10.0.0.1").unwrap();
        assert_eq!(evaluate(Some(&ip("::ffff:10.0.0.1")), &t), Status::Match);
    }

    #[test]
    fn invalid_input() {
        assert!(parse_targets("1.2.3").is_err());
        assert!(parse_targets("1.2.3.4/33").is_err());
        assert!(parse_targets("abc").is_err());
        assert!(parse_targets("1.2.3.4/x").is_err());
    }
}
