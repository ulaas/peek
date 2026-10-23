//! Connection endpoints and the quick-connect address syntax.
//!
//! Accepted forms:
//! `host`, `host:port`, `user@host`, `rdp://user@host:3389`, `vnc://host:1`
//! (VNC display number), `[fe80::1]:3390`. Without a scheme the protocol is
//! guessed from the port: 5900–5999 is VNC, 22 is SSH, anything else RDP.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Rdp,
    Vnc,
    Ssh,
}

impl Protocol {
    pub const ALL: [Protocol; 3] = [Protocol::Rdp, Protocol::Vnc, Protocol::Ssh];

    pub const fn default_port(self) -> u16 {
        match self {
            Protocol::Rdp => 3389,
            Protocol::Vnc => 5900,
            Protocol::Ssh => 22,
        }
    }

    pub const fn scheme(self) -> &'static str {
        match self {
            Protocol::Rdp => "rdp",
            Protocol::Vnc => "vnc",
            Protocol::Ssh => "ssh",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Protocol::Rdp => "RDP",
            Protocol::Vnc => "VNC",
            Protocol::Ssh => "SSH",
        }
    }

    fn from_scheme(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "rdp" | "ms-rd" => Some(Protocol::Rdp),
            "vnc" => Some(Protocol::Vnc),
            "ssh" => Some(Protocol::Ssh),
            _ => None,
        }
    }

    fn guess_from_port(port: u16) -> Self {
        match port {
            5900..=5999 => Protocol::Vnc,
            22 => Protocol::Ssh,
            _ => Protocol::Rdp,
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Endpoint {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("enter a host name or address")]
    Empty,
    #[error("unknown protocol \"{0}\" (use rdp, vnc or ssh)")]
    UnknownScheme(String),
    #[error("\"{0}\" is not a valid port")]
    BadPort(String),
    #[error("missing closing ] in IPv6 address")]
    BadIpv6,
}

impl Endpoint {
    pub fn new(protocol: Protocol, host: impl Into<String>) -> Self {
        Self { protocol, host: host.into(), port: protocol.default_port(), username: None }
    }

    pub fn parse(input: &str) -> Result<Self, ParseError> {
        let mut s = input.trim();
        if s.is_empty() {
            return Err(ParseError::Empty);
        }

        let mut protocol = None;
        if let Some((scheme, rest)) = s.split_once("://") {
            protocol = Some(Protocol::from_scheme(scheme).ok_or_else(|| ParseError::UnknownScheme(scheme.into()))?);
            s = rest;
        }
        s = s.trim_end_matches('/');

        let mut username = None;
        if let Some((user, rest)) = s.rsplit_once('@') {
            if !user.is_empty() {
                username = Some(user.to_string());
            }
            s = rest;
        }

        let (host, port_str) = if let Some(rest) = s.strip_prefix('[') {
            let (h, after) = rest.split_once(']').ok_or(ParseError::BadIpv6)?;
            (h, after.strip_prefix(':'))
        } else if s.matches(':').count() > 1 {
            // Bare IPv6 address without a port.
            (s, None)
        } else {
            match s.split_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (s, None),
            }
        };
        if host.is_empty() {
            return Err(ParseError::Empty);
        }

        let port = match port_str {
            Some(p) if !p.is_empty() => Some(p.parse::<u16>().map_err(|_| ParseError::BadPort(p.into()))?),
            _ => None,
        };

        let protocol = protocol.unwrap_or_else(|| port.map(Protocol::guess_from_port).unwrap_or_default());
        let port = match (protocol, port) {
            // VNC display numbers: host:1 means port 5901.
            (Protocol::Vnc, Some(p)) if p < 100 => 5900 + p,
            (_, Some(p)) => p,
            (proto, None) => proto.default_port(),
        };

        Ok(Self { protocol, host: host.to_string(), port, username })
    }

    /// `host:port` suitable for a socket connect.
    pub fn socket_addr(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://", self.protocol.scheme())?;
        if let Some(u) = &self.username {
            write!(f, "{u}@")?;
        }
        if self.port == self.protocol.default_port() {
            if self.host.contains(':') { write!(f, "[{}]", self.host) } else { f.write_str(&self.host) }
        } else {
            f.write_str(&self.socket_addr())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Endpoint {
        Endpoint::parse(s).unwrap()
    }

    #[test]
    fn bare_host_is_rdp() {
        let e = p("server01");
        assert_eq!((e.protocol, e.port), (Protocol::Rdp, 3389));
    }

    #[test]
    fn port_guesses_protocol() {
        assert_eq!(p("box:5901").protocol, Protocol::Vnc);
        assert_eq!(p("box:22").protocol, Protocol::Ssh);
        assert_eq!(p("box:3390").protocol, Protocol::Rdp);
    }

    #[test]
    fn vnc_display_number() {
        assert_eq!(p("vnc://box:2").port, 5902);
    }

    #[test]
    fn full_url() {
        let e = p("rdp://DOMAIN\\alice@10.0.0.5:3390/");
        assert_eq!(e.username.as_deref(), Some("DOMAIN\\alice"));
        assert_eq!(e.host, "10.0.0.5");
        assert_eq!(e.port, 3390);
        assert_eq!(e.to_string(), "rdp://DOMAIN\\alice@10.0.0.5:3390");
    }

    #[test]
    fn microsoft_account_user() {
        let e = p("MicrosoftAccount\\you@hotmail.com@pc.local");
        assert_eq!(e.username.as_deref(), Some("MicrosoftAccount\\you@hotmail.com"));
        assert_eq!((e.host.as_str(), e.port), ("pc.local", 3389));
        assert_eq!(p(&e.to_string()), e);
    }

    #[test]
    fn ipv6() {
        assert_eq!(p("[fe80::1]:5900").host, "fe80::1");
        assert_eq!(p("fe80::1").port, 3389);
        assert_eq!(p("[::1]").socket_addr(), "[::1]:3389");
    }

    #[test]
    fn errors() {
        assert_eq!(Endpoint::parse("  "), Err(ParseError::Empty));
        assert!(matches!(Endpoint::parse("ftp://x"), Err(ParseError::UnknownScheme(_))));
        assert!(matches!(Endpoint::parse("x:99999"), Err(ParseError::BadPort(_))));
    }
}
