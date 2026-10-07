//! Public IP lookup via several plain-text "what is my IP" services.

use std::net::IpAddr;
#[cfg(windows)]
use std::sync::Arc;
#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
/// Tried in order; the first valid answer wins.
const PROVIDERS: &[&str] = &[
    "https://api.ipify.org",
    "https://checkip.amazonaws.com",
    "https://ipv4.icanhazip.com",
    "https://ipinfo.io/ip",
];

#[cfg(windows)]
pub struct IpFetcher {
    agent: ureq::Agent,
    providers: Vec<String>,
}

#[cfg(windows)]
impl IpFetcher {
    pub fn new(custom_providers: &[String]) -> Result<Self, String> {
        // native-tls uses Windows SChannel, so the system certificate store
        // (including corporate root CAs) is respected.
        let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
        let agent = ureq::AgentBuilder::new()
            .tls_connector(Arc::new(tls))
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(8))
            .user_agent(concat!("ip-tray-monitor/", env!("CARGO_PKG_VERSION")))
            .build();
        let providers = if custom_providers.is_empty() {
            PROVIDERS.iter().map(|s| s.to_string()).collect()
        } else {
            custom_providers.to_vec()
        };
        Ok(IpFetcher { agent, providers })
    }

    pub fn fetch(&self) -> Result<IpAddr, String> {
        let mut last_err = String::from("no providers configured");
        for url in &self.providers {
            match self.fetch_from(url) {
                Ok(ip) => return Ok(ip),
                Err(e) => last_err = format!("{url}: {e}"),
            }
        }
        Err(last_err)
    }

    fn fetch_from(&self, url: &str) -> Result<IpAddr, String> {
        let body = self
            .agent
            .get(url)
            .call()
            .map_err(|e| e.to_string())?
            .into_string()
            .map_err(|e| e.to_string())?;
        parse_ip_body(&body)
    }
}

pub fn parse_ip_body(body: &str) -> Result<IpAddr, String> {
    let text = body.trim();
    text.parse::<IpAddr>()
        .map_err(|_| format!("unexpected response: {:.40}", text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_bodies() {
        assert_eq!(parse_ip_body("203.0.113.5\n").unwrap().to_string(), "203.0.113.5");
        assert!(parse_ip_body("<html>blocked</html>").is_err());
        assert!(parse_ip_body("").is_err());
    }
}
