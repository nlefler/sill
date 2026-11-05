//! The strict address parser (AddressParser.swift): every address taken from a person or a link goes
//! through it, because the system's own parsers are lenient in ways that dial the wrong machine
//! ("1.2.3" is 1.2.0.3, "0x64.0x65.1.2" is 100.101.1.2).

use std::net::Ipv6Addr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AddrKind {
    Ipv4,
    Ipv6,
    Name,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParsedAddress {
    /// No brackets for IPv6; compressed, lowercase.
    pub host: String,
    pub port: Option<u16>,
    pub kind: AddrKind,
}

impl ParsedAddress {
    /// "host" or "host:port", IPv6 in brackets: the form a link's `a` carries.
    pub fn text(&self) -> String {
        let h = if self.kind == AddrKind::Ipv6 { format!("[{}]", self.host) } else { self.host.clone() };
        match self.port {
            Some(p) => format!("{h}:{p}"),
            None => h,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddrFailure {
    Empty,
    Malformed,
    Zone,
    Ipv4,
    Ipv6,
    Name,
    Port,
    PairingLink,
}

fn is_ws(c: char) -> bool {
    c.is_whitespace()
}

pub fn parse(raw: &str) -> Result<ParsedAddress, AddrFailure> {
    let s = raw.trim_matches(is_ws);
    if s.is_empty() {
        return Err(AddrFailure::Empty);
    }
    if s.to_ascii_lowercase().starts_with("sill:") {
        return Err(AddrFailure::PairingLink);
    }
    if s.chars().any(|c| is_ws(c) || (c as u32) < 0x20 || c as u32 == 0x7f) {
        return Err(AddrFailure::Malformed);
    }
    if s.contains('%') {
        return Err(AddrFailure::Zone);
    }
    if let Some(rest) = s.strip_prefix('[') {
        let close = rest.find(']').ok_or(AddrFailure::Ipv6)?;
        let (host, tail) = (&rest[..close], &rest[close + 1..]);
        let port_text = if tail.is_empty() { None } else { Some(tail.strip_prefix(':').ok_or(AddrFailure::Malformed)?) };
        // Swift parses the address before the port: a bad address wins.
        let mut a = ipv6(host).ok_or(AddrFailure::Ipv6)?;
        if let Some(p) = port_text {
            a.port = Some(parse_port(p).ok_or(AddrFailure::Port)?);
        }
        return Ok(a);
    }
    let colons = s.matches(':').count();
    if colons >= 2 {
        return ipv6(s).ok_or(AddrFailure::Ipv6);
    }
    let (host, port) = match s.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (s, None),
    };
    let mut a = host_only(host)?;
    if let Some(p) = port {
        a.port = Some(parse_port(p).ok_or(AddrFailure::Port)?);
    }
    Ok(a)
}

fn parse_port(p: &str) -> Option<u16> {
    if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    p.parse::<u32>().ok().filter(|v| (1..=65535).contains(v)).map(|v| v as u16)
}

fn host_only(h: &str) -> Result<ParsedAddress, AddrFailure> {
    if h.is_empty() {
        return Err(AddrFailure::Malformed);
    }
    let body = h.strip_suffix('.').unwrap_or(h);
    let labels: Vec<&str> = body.split('.').collect();
    let all_numeric = labels.iter().all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()));
    if all_numeric {
        let ok = !h.ends_with('.')
            && labels.len() == 4
            && labels.iter().all(|l| (*l == "0" || !l.starts_with('0')) && l.len() <= 3 && l.parse::<u32>().is_ok_and(|v| v <= 255));
        return if ok { Ok(ParsedAddress { host: h.to_string(), port: None, kind: AddrKind::Ipv4 }) } else { Err(AddrFailure::Ipv4) };
    }
    if accepted_by_inet_aton(h) {
        return Err(AddrFailure::Ipv4);
    }
    if !is_name(h) {
        return Err(AddrFailure::Name);
    }
    Ok(ParsedAddress { host: h.to_string(), port: None, kind: AddrKind::Name })
}

/// What `inet_aton` would read as an address and the canonical form above is not: one to four
/// labels, each a decimal, octal or 0x-hex number.
fn accepted_by_inet_aton(h: &str) -> bool {
    let labels: Vec<&str> = h.split('.').collect();
    if labels.is_empty() || labels.len() > 4 {
        return false;
    }
    labels.iter().all(|l| {
        let hex = l.strip_prefix("0x").or_else(|| l.strip_prefix("0X"));
        match hex {
            Some(d) => d.bytes().all(|b| b.is_ascii_hexdigit()),
            None => !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()),
        }
    })
}

fn is_name(h: &str) -> bool {
    let body = h.strip_suffix('.').unwrap_or(h);
    if body.is_empty() || body.chars().count() > 253 {
        return false;
    }
    body.split('.').all(|l| {
        (1..=63).contains(&l.len())
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

fn ipv6(h: &str) -> Option<ParsedAddress> {
    if h.is_empty() || h.contains('%') {
        return None;
    }
    let a: Ipv6Addr = h.parse().ok()?;
    let o = a.octets();
    if o[..10].iter().all(|b| *b == 0) && o[10] == 0xff && o[11] == 0xff {
        let host = format!("{}.{}.{}.{}", o[12], o[13], o[14], o[15]);
        return Some(ParsedAddress { host, port: None, kind: AddrKind::Ipv4 });
    }
    Some(ParsedAddress { host: a.to_string(), port: None, kind: AddrKind::Ipv6 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> (String, Option<u16>, AddrKind) {
        let a = parse(s).unwrap_or_else(|e| panic!("{s}: {e:?}"));
        (a.host, a.port, a.kind)
    }

    #[test]
    fn good_addresses() {
        assert_eq!(ok("100.101.102.103"), ("100.101.102.103".into(), None, AddrKind::Ipv4));
        assert_eq!(ok(" 10.0.0.1:7455 "), ("10.0.0.1".into(), Some(7455), AddrKind::Ipv4));
        assert_eq!(ok("mac-mini.tail1234.ts.net:9"), ("mac-mini.tail1234.ts.net".into(), Some(9), AddrKind::Name));
        assert_eq!(ok("localhost"), ("localhost".into(), None, AddrKind::Name));
        assert_eq!(ok("[FD7A:115C:A1E0:0:0:0:0:1234]:80"), ("fd7a:115c:a1e0::1234".into(), Some(80), AddrKind::Ipv6));
        assert_eq!(ok("fd7a::1:7455"), ("fd7a::1:7455".into(), None, AddrKind::Ipv6)); // never a port
        assert_eq!(ok("::ffff:1.2.3.4"), ("1.2.3.4".into(), None, AddrKind::Ipv4));
        assert_eq!(parse("[fd7a::1]:7455").unwrap().text(), "[fd7a::1]:7455");
    }

    #[test]
    fn refusals() {
        use AddrFailure::*;
        for (s, f) in [
            ("", Empty), ("   ", Empty), ("1.2.3", Ipv4), ("0x64.0x65.1.2", Ipv4), ("100.1", Ipv4), ("01.2.3.4", Ipv4),
            ("256.1.1.1", Ipv4), ("1.2.3.4.", Ipv4), ("fe80::1%en0", Zone), ("1.2.3.4 5", Malformed),
            ("sill://pair?v=1", PairingLink), ("[::1", Ipv6), ("[nope]", Ipv6), ("a:b:c", Ipv6), ("host:0", Port),
            ("host:65536", Port), ("host:", Port), ("-a.example", Name), ("a_b.example", Name), ("[::1]x", Malformed),
            (":80", Malformed), ("0x1", Ipv4),
        ] {
            assert_eq!(parse(s).unwrap_err(), f, "{s:?}");
        }
    }
}
