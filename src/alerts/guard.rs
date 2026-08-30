//! Outbound URL safety.
//!
//! A webhook URL is an instruction to make a request from inside your network.
//! Without a guard, `http://169.254.169.254/latest/meta-data/` turns the alert
//! system into a cloud-credential reader, and `http://10.0.0.5:6379/` turns it
//! into a port scanner. Rules are admin-created, and on this deployment an AI
//! agent holds admin, so the URL is not necessarily written by a human.
//!
//! Checked twice: when a rule is saved, and again immediately before each
//! delivery, because DNS can change its answer in between.

use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};

#[derive(Debug)]
pub enum UrlRejected {
    NotAUrl(String),
    BadScheme(String),
    NoHost,
    Unresolvable(String),
    PrivateAddress(IpAddr),
}

impl std::fmt::Display for UrlRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAUrl(u) => write!(f, "not a valid URL: {u}"),
            Self::BadScheme(s) => write!(f, "scheme must be http or https, got {s:?}"),
            Self::NoHost => write!(f, "URL has no host"),
            Self::Unresolvable(h) => write!(f, "cannot resolve host {h:?}"),
            Self::PrivateAddress(ip) => write!(
                f,
                "{ip} is a private, loopback or link-local address. Webhooks may not reach \
                 inside your network. Set LOGGER_WEBHOOK_ALLOW_PRIVATE=true if this is a \
                 deliberate internal relay."
            ),
        }
    }
}

/// Addresses a webhook must not reach: anything that is not a normal, routable
/// public host.
fn is_forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()          // includes 169.254.169.254
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
                // 100.64.0.0/10, carrier-grade NAT and some cloud fabrics.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
                // 198.18.0.0/15, benchmarking.
                || (v4.octets()[0] == 198 && (18..20).contains(&v4.octets()[1]))
                || v4 == Ipv4Addr::new(255, 255, 255, 255)
        }
        IpAddr::V6(v6) => {
            // An IPv4-mapped address such as ::ffff:127.0.0.1 reaches an IPv4
            // host, so it must be judged as that address rather than as IPv6.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_forbidden(IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 unique-local
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// Validates a webhook URL, resolving the host and checking every address it
/// answers with — one public answer is not enough if another is internal.
pub fn check(raw: &str, allow_private: bool) -> Result<(), UrlRejected> {
    let parsed = url::Url::parse(raw).map_err(|_| UrlRejected::NotAUrl(raw.to_string()))?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(UrlRejected::BadScheme(other.to_string())),
    }

    let host = parsed.host_str().ok_or(UrlRejected::NoHost)?.to_string();
    if allow_private {
        return Ok(());
    }

    // An IP literal needs no lookup.
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if is_forbidden(ip) {
            Err(UrlRejected::PrivateAddress(ip))
        } else {
            Ok(())
        };
    }

    let port = parsed.port_or_known_default().unwrap_or(443);
    let resolved: Vec<IpAddr> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|_| UrlRejected::Unresolvable(host.clone()))?
        .map(|s| s.ip())
        .collect();

    if resolved.is_empty() {
        return Err(UrlRejected::Unresolvable(host));
    }
    for ip in resolved {
        if is_forbidden(ip) {
            return Err(UrlRejected::PrivateAddress(ip));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_the_addresses_that_matter() {
        for raw in [
            "http://127.0.0.1:6379/",
            "http://localhost/hook",
            "http://169.254.169.254/latest/meta-data/", // cloud credentials
            "http://10.0.0.5/internal",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://[fc00::1]/",
            "http://[::ffff:127.0.0.1]/", // IPv4-mapped loopback
            "http://0.0.0.0/",
            "http://100.64.0.1/", // carrier-grade NAT
        ] {
            assert!(
                check(raw, false).is_err(),
                "{raw} must be rejected without the opt-out"
            );
            assert!(
                check(raw, true).is_ok(),
                "{raw} must be allowed once the operator opts in"
            );
        }
    }

    #[test]
    fn allows_ordinary_public_webhooks() {
        for raw in [
            "https://hooks.slack.com/services/T000/B000/XXXX",
            "https://discord.com/api/webhooks/1/abc",
            "https://events.pagerduty.com/v2/enqueue",
            "https://8.8.8.8/hook",
        ] {
            assert!(
                check(raw, false).is_ok(),
                "{raw} should be allowed: {:?}",
                check(raw, false)
            );
        }
    }

    #[test]
    fn rejects_non_http_schemes_and_junk() {
        for raw in [
            "file:///etc/passwd",
            "gopher://x/",
            "ftp://x/",
            "not a url",
            "",
        ] {
            assert!(check(raw, false).is_err(), "{raw:?} must be rejected");
            // The opt-out relaxes address rules, never the scheme or syntax.
            assert!(
                check(raw, true).is_err(),
                "{raw:?} must stay rejected even with the opt-out"
            );
        }
    }
}
