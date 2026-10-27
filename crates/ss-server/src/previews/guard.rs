//! Addresses a preview fetch is allowed to connect to.
//!
//! Loopback, link-local, private, CGNAT, and multicast addresses are rejected
//! so a message cannot make the server call itself or a cloud metadata address.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `ip` is a public unicast address.
#[must_use]
pub(crate) fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or_else(|| public_v6(ip), public_v4),
    }
}

/// Loopback, including IPv4-mapped `::ffff:127.0.0.1`.
#[must_use]
pub(crate) fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback())
        }
    }
}

/// `true` when `host` is allowed by the optional allow list and is not denied.
#[must_use]
pub(crate) fn host_allowed(host: &str, allow: &[String], deny: &[String]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if deny.iter().any(|rule| host_matches(&host, rule)) {
        return false;
    }
    allow.is_empty() || allow.iter().any(|rule| host_matches(&host, rule))
}

fn host_matches(host: &str, rule: &str) -> bool {
    let rule = rule.trim().trim_end_matches('.').to_ascii_lowercase();
    if let Some(suffix) = rule.strip_prefix("*.") {
        host == suffix || host_ends_with_label(host, suffix)
    } else {
        host == rule
    }
}

fn host_ends_with_label(host: &str, suffix: &str) -> bool {
    host.len() > suffix.len()
        && host.ends_with(suffix)
        && host.as_bytes().get(host.len() - suffix.len() - 1) == Some(&b'.')
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    if ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
    {
        return false;
    }
    let cgnat = a == 100 && (64..=127).contains(&b);
    let reserved = a >= 240;
    !cgnat && !reserved
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments().into_iter().next().unwrap_or(0);
    !ip.is_unspecified()
        && !ip.is_loopback()
        && !ip.is_multicast()
        && (first & 0xfe00) != 0xfc00
        && (first & 0xffc0) != 0xfe80
}

#[cfg(test)]
mod tests {
    use super::{host_allowed, is_loopback, is_public};

    #[test]
    fn private_and_special_addresses_are_rejected() {
        for ip in [
            "0.0.0.0",
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
        ] {
            let ip = ip.parse().unwrap();
            assert!(!is_public(ip), "{ip}");
        }
        assert!(is_loopback("127.0.0.1".parse().unwrap()));
        assert!(is_loopback("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_public("1.1.1.1".parse().unwrap()));
        assert!(is_public("8.8.8.8".parse().unwrap()));
        assert!(is_public("2606:4700:4700::1111".parse().unwrap()));
        assert!(is_public("100.63.0.1".parse().unwrap()));
        assert!(is_public("100.128.0.1".parse().unwrap()));
    }

    #[test]
    fn host_lists_are_exact_unless_starred() {
        let allow = vec!["youtube.com".into(), "*.example.com".into()];
        let deny = vec!["blocked.example.com".into()];
        assert!(host_allowed("youtube.com", &allow, &deny));
        assert!(!host_allowed("www.youtube.com", &allow, &deny));
        assert!(host_allowed("a.example.com", &allow, &deny));
        assert!(!host_allowed("blocked.example.com", &allow, &deny));
        assert!(!host_allowed("evil.test", &allow, &deny));
        assert!(host_allowed("evil.test", &[], &[]));
    }
}
