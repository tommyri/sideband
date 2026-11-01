//! Addresses a preview fetch is allowed to connect to.
//!
//! Loopback, link-local, private, CGNAT, benchmarking, documentation, and
//! multicast addresses are rejected so a message cannot make the server call
//! itself or a cloud metadata address. IPv6 forms that embed an IPv4 address
//! (mapped, compatible, NAT64, 6to4) are judged by that address; Teredo is
//! rejected because its embedded address is obfuscated.

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
    let [_, _, c, _] = ip.octets();
    let this_network = a == 0;
    let cgnat = a == 100 && (64..=127).contains(&b);
    let protocol = a == 192 && b == 0 && c == 0;
    let relay = a == 192 && b == 88 && c == 99;
    let benchmarking = a == 198 && (b == 18 || b == 19);
    let reserved = a >= 240;
    !this_network && !cgnat && !protocol && !relay && !benchmarking && !reserved
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let embedded = |high: u16, low: u16| {
        let [h1, h2] = high.to_be_bytes();
        let [l1, l2] = low.to_be_bytes();
        Ipv4Addr::new(h1, h2, l1, l2)
    };
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return false;
    }
    // ::a.b.c.d, the deprecated IPv4-compatible form.
    if s[..6] == [0; 6] {
        return false;
    }
    // 64:ff9b::/96 well-known NAT64 prefix, and the local-use 64:ff9b:1::/48.
    if s[0] == 0x64 && s[1] == 0xff9b {
        return s[2..6] == [0; 4] && public_v4(embedded(s[6], s[7]));
    }
    // 2002::/16 6to4 carries the IPv4 address in the next 32 bits.
    if s[0] == 0x2002 {
        return public_v4(embedded(s[1], s[2]));
    }
    let teredo = s[0] == 0x2001 && s[1] == 0;
    let documentation = s[0] == 0x2001 && s[1] == 0x0db8;
    let discard = s[0] == 0x0100 && s[1..4] == [0; 3];
    let unique_local = (s[0] & 0xfe00) == 0xfc00;
    let link_local = (s[0] & 0xffc0) == 0xfe80;
    let site_local = (s[0] & 0xffc0) == 0xfec0;
    !teredo && !documentation && !discard && !unique_local && !link_local && !site_local
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
            "0.1.2.3",
            "192.0.0.8",
            "192.88.99.1",
            "198.18.0.1",
            "198.19.255.255",
            "240.0.0.1",
            "::127.0.0.1",
            "::10.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::7f00:1",
            "64:ff9b:1::808:808",
            "2002:7f00:1::1",
            "2002:a9fe:a9fe::",
            "2001::1",
            "2001:db8::1",
            "100::1",
            "fec0::1",
            "ff02::1",
        ] {
            let ip = ip.parse().unwrap();
            assert!(!is_public(ip), "{ip}");
        }
        assert!(is_loopback("127.0.0.1".parse().unwrap()));
        assert!(is_loopback("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_public("1.1.1.1".parse().unwrap()));
        assert!(is_public("8.8.8.8".parse().unwrap()));
        assert!(is_public("2606:4700:4700::1111".parse().unwrap()));
        assert!(is_public("64:ff9b::808:808".parse().unwrap()));
        assert!(is_public("2002:808:808::1".parse().unwrap()));
        assert!(is_public("198.20.0.1".parse().unwrap()));
        assert!(is_public("192.0.1.1".parse().unwrap()));
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
