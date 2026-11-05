//! Row and reconnect decisions as pure functions, after `iOSClient/DiscoveryPolicy.swift`
//! (`rowMac`, `rowDoor`, `reconnectMatches`, `redialRow`, `reconnectLine`, `reconnectRow`'s quit wait)
//! and `Sources/StreamProtocol/Pairing.swift` (`RecognitionTag`, `HomeDoorTXT`). No clock, no I/O.

use ring::hmac;
use sill_net::b64;
use sill_net::saved::{SavedMac, Standing};
use std::net::IpAddr;

/// The Bonjour TXT keys.
pub const TXT_TAG: &str = "r";
pub const TXT_DOOR: &str = "p";

/// `HomeDoorTXT.Door`: how a Mac's home door speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeDoor {
    /// No `p`: plain TCP. This client never dials it.
    Plain,
    /// `p=1`, any value but "0", or `p` with no value: TLS, paired devices only.
    PairingRequired,
    /// `p=0`: TLS, any device.
    Open,
}

impl HomeDoor {
    /// `value`: `None` when the record has no `p`; `Some(None)` for a `p` without a value.
    pub fn from_txt(value: Option<Option<&str>>) -> HomeDoor {
        match value {
            None => HomeDoor::Plain,
            Some(Some("0")) => HomeDoor::Open,
            Some(_) => HomeDoor::PairingRequired,
        }
    }
}

/// The tag a Mac advertises for `key`: base64url of nonce(6) || HMAC-SHA256("sill-tag-v1" || nonce)[..6].
pub fn make_tag(recognition_key: &[u8; 32], nonce: [u8; 6]) -> String {
    let mut t = nonce.to_vec();
    t.extend_from_slice(&tag_mac(recognition_key, &nonce));
    b64::url_encode(&t)
}

fn tag_mac(key: &[u8; 32], nonce: &[u8]) -> [u8; 6] {
    let mut msg = b"sill-tag-v1".to_vec();
    msg.extend_from_slice(nonce);
    let full = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), &msg);
    full.as_ref()[..6].try_into().unwrap()
}

/// `RecognitionTag.matches`, constant time in the 6 compared bytes.
pub fn tag_matches(tag: &str, recognition_key: &[u8; 32]) -> bool {
    let Some(bytes) = b64::url_decode(tag) else { return false };
    if bytes.len() != 12 {
        return false;
    }
    let expected = tag_mac(recognition_key, &bytes[..6]);
    expected.iter().zip(&bytes[6..]).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

/// What a browsed service is, as the policy needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The Bonjour instance name ("Noah's MacBook Pro").
    pub name: String,
    pub addrs: Vec<IpAddr>,
    pub port: u16,
    /// TXT `r`, if the record carries one.
    pub tag: Option<String>,
    pub door: HomeDoor,
}

/// `rowMac`: the saved Mac a row stands for. `tagged` is the saved Mac whose recognition key makes
/// the tag. A row that carries a tag no saved Mac resolves is not one, whatever its name says.
pub fn row_mac<'a>(row: &Row, saved: &'a [SavedMac]) -> Option<(&'a SavedMac, bool)> {
    if let Some(tag) = row.tag.as_deref() {
        return saved.iter().find(|m| tag_matches(tag, &m.recognition_key)).map(|m| (m, true));
    }
    saved.iter().find(|m| m.name == row.name).map(|m| (m, false))
}

/// `otherTagUnderSavedName`: a tag that names no saved Mac under a saved Mac's name (a Mac set up
/// again, or a look-alike shedding the name rule).
pub fn other_tag_under_saved_name(row: &Row, saved: &[SavedMac]) -> bool {
    match row.tag.as_deref() {
        Some(tag) => {
            !saved.iter().any(|m| tag_matches(tag, &m.recognition_key)) && saved.iter().any(|m| m.name == row.name)
        }
        None => false,
    }
}

/// `rowDoor`: an open door under a saved Mac's name with a foreign tag is dialed as one that requires
/// pairing, so nothing hands such a look-alike a session.
pub fn row_door(door: HomeDoor, other_tag_under_saved_name: bool) -> HomeDoor {
    if other_tag_under_saved_name && door == HomeDoor::Open { HomeDoor::PairingRequired } else { door }
}

/// What a row is to this client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowClass {
    /// A saved Mac we may connect to: dial pinned to its key.
    Paired { mac_id: String, by_tag: bool },
    /// Not saved (or removed, or asks to pair): pairing is needed first.
    NotPaired { door: HomeDoor, was_saved: Option<String> },
    /// An open door (`p=0`) not saved: a tap could connect with any key; shown "Not paired".
    OpenDoor,
    /// No `p`: the Mac's Sill is from before pairing at home. Never dialed ("Update Sill on the Mac").
    UpdateSill,
}

impl RowClass {
    pub fn word(&self) -> &'static str {
        match self {
            RowClass::Paired { .. } => "Paired",
            RowClass::NotPaired { .. } | RowClass::OpenDoor => "Not paired",
            RowClass::UpdateSill => "Update Sill on the Mac",
        }
    }
    pub fn dialable(&self) -> bool {
        !matches!(self, RowClass::UpdateSill)
    }
}

pub fn classify(row: &Row, saved: &[SavedMac]) -> RowClass {
    if row.door == HomeDoor::Plain {
        return RowClass::UpdateSill;
    }
    let door = row_door(row.door, other_tag_under_saved_name(row, saved));
    match row_mac(row, saved) {
        Some((m, by_tag)) if m.standing == Standing::Paired => RowClass::Paired { mac_id: m.mac_id.clone(), by_tag },
        Some((m, _)) => RowClass::NotPaired { door, was_saved: Some(m.mac_id.clone()) },
        None if door == HomeDoor::Open => RowClass::OpenDoor,
        None => RowClass::NotPaired { door, was_saved: None },
    }
}

/// `reconnectMatches`: whether `row` is the saved Mac the reconnect looks for.
pub fn reconnect_matches(saved: &SavedMac, row: &Row, others: &[SavedMac]) -> bool {
    match row_mac(row, others) {
        Some((m, _)) => m.mac_id == saved.mac_id,
        None => row.tag.is_none() && row.name == saved.name,
    }
}

fn is_link_local_v6(a: &IpAddr) -> bool {
    matches!(a, IpAddr::V6(v) if (v.segments()[0] & 0xffc0) == 0xfe80)
}

/// The row's addresses in dial order: IPv4, then other IPv6. An unscoped link-local IPv6 address
/// is left out: mdns-sd does not report its interface, and it cannot be dialed without one.
pub fn dial_hosts(row: &Row) -> Vec<(String, u16)> {
    let mut v4: Vec<&IpAddr> = row.addrs.iter().filter(|a| a.is_ipv4()).collect();
    let mut v6: Vec<&IpAddr> = row.addrs.iter().filter(|a| a.is_ipv6() && !is_link_local_v6(a)).collect();
    v4.sort();
    v6.sort();
    v4.into_iter().chain(v6).map(|a| (a.to_string(), row.port)).collect()
}

// ---- the browser's restart backoff

pub const BROWSER_BACKOFF: [u64; 6] = [1, 2, 4, 8, 16, 30];

/// Seconds to wait before making the browser again after its `failures`-th failure in a row (1-based).
pub fn browser_backoff(failures: u32) -> u64 {
    BROWSER_BACKOFF[(failures.max(1) as usize - 1).min(BROWSER_BACKOFF.len() - 1)]
}

// ---- the reconnect schedule

pub const QUIT_WAIT: f64 = 3.0;
pub const RECONNECT_RETRY: [f64; 4] = [5.0, 10.0, 20.0, 30.0];
pub const RECONNECT_QUIET_AFTER: u32 = 3;
pub const RECONNECT_DIALS: u32 = 12;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Redial {
    /// Dial now.
    Now,
    /// Look again at this time.
    At(f64),
    /// Not while the app is away, or after 12 tries: a tap, a fresh listing or the return looks again.
    No,
}

/// `redialRow`: after `failed` dials of this listing that did not connect, the last started at `last_dial_at`.
pub fn redial_row(failed: u32, last_dial_at: Option<f64>, foreground: bool, now: f64) -> Redial {
    if !foreground || failed >= RECONNECT_DIALS {
        return Redial::No;
    }
    let Some(last) = last_dial_at.filter(|_| failed > 0) else { return Redial::Now };
    let due = last + RECONNECT_RETRY[(failed as usize).min(RECONNECT_RETRY.len()) - 1];
    if now >= due { Redial::Now } else { Redial::At(due) }
}

/// A row listed since before a goodbye `quit` is the Mac's old registration: not taken until
/// `QUIT_WAIT` has passed. `Some(t)` is when to look again.
pub fn quit_hold(quit_at: Option<f64>, row_since: f64, now: f64) -> Option<f64> {
    let q = quit_at?;
    (now < q + QUIT_WAIT && row_since <= q).then_some(q + QUIT_WAIT)
}

/// `reconnectLine`.
pub fn reconnect_line(mac: &str, failed: u32) -> String {
    if failed >= RECONNECT_DIALS {
        format!("Stopped trying to reach {mac}. Tap it to try again.")
    } else if failed >= RECONNECT_QUIET_AFTER {
        format!("{mac} isn\u{2019}t answering. It may be asleep. Sill keeps trying, or tap it to try now.")
    } else {
        format!("Reconnecting to {mac}\u{2026}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn mac(id: &str, name: &str, key: u8) -> SavedMac {
        SavedMac {
            mac_id: id.into(),
            name: name.into(),
            fingerprint: [1; 32],
            recognition_key: [key; 32],
            remote_port: 7455,
            addresses: vec![],
            paired_at: 0.0,
            last_connected_at: None,
            info_issued_at: 0.0,
            method: "qr".into(),
            standing: Standing::Paired,
        }
    }
    fn row(name: &str, tag: Option<String>, door: HomeDoor) -> Row {
        Row { name: name.into(), addrs: vec![], port: 1, tag, door }
    }

    #[test]
    fn door_txt() {
        assert_eq!(HomeDoor::from_txt(None), HomeDoor::Plain);
        assert_eq!(HomeDoor::from_txt(Some(Some("0"))), HomeDoor::Open);
        assert_eq!(HomeDoor::from_txt(Some(Some("1"))), HomeDoor::PairingRequired);
        assert_eq!(HomeDoor::from_txt(Some(Some("x"))), HomeDoor::PairingRequired);
        assert_eq!(HomeDoor::from_txt(Some(Some(""))), HomeDoor::PairingRequired);
        assert_eq!(HomeDoor::from_txt(Some(None)), HomeDoor::PairingRequired);
    }

    #[test]
    fn tags() {
        let k = [7u8; 32];
        let t = make_tag(&k, [1, 2, 3, 4, 5, 6]);
        assert_eq!(t.len(), 16);
        assert!(tag_matches(&t, &k));
        assert!(!tag_matches(&t, &[8u8; 32]));
        assert!(!tag_matches("short", &k));
        assert!(!tag_matches("", &k));
        let mut bad = b64::url_decode(&t).unwrap();
        bad[11] ^= 1;
        assert!(!tag_matches(&b64::url_encode(&bad), &k));
        // A fresh nonce makes another valid tag.
        assert!(tag_matches(&make_tag(&k, [9; 6]), &k));
    }

    #[test]
    fn row_mac_rules() {
        let saved = vec![mac("A", "Studio", 1), mac("B", "Laptop", 2)];
        let tag_b = make_tag(&[2; 32], [0; 6]);
        // A tag wins over the name.
        let r = row("Studio", Some(tag_b.clone()), HomeDoor::PairingRequired);
        assert_eq!(row_mac(&r, &saved).map(|(m, t)| (m.mac_id.as_str(), t)), Some(("B", true)));
        // No tag at all: the name.
        let r = row("Studio", None, HomeDoor::PairingRequired);
        assert_eq!(row_mac(&r, &saved).map(|(m, t)| (m.mac_id.as_str(), t)), Some(("A", false)));
        // A tag no saved Mac resolves is no saved Mac, under a saved name too.
        let r = row("Studio", Some(make_tag(&[9; 32], [0; 6])), HomeDoor::Open);
        assert!(row_mac(&r, &saved).is_none());
        assert!(other_tag_under_saved_name(&r, &saved));
        assert_eq!(row_door(HomeDoor::Open, true), HomeDoor::PairingRequired);
        assert_eq!(classify(&r, &saved), RowClass::NotPaired { door: HomeDoor::PairingRequired, was_saved: None });
        assert!(!reconnect_matches(&saved[0], &r, &saved));
        assert!(reconnect_matches(&saved[0], &row("Studio", None, HomeDoor::Open), &saved));
        assert!(!reconnect_matches(&saved[0], &row("Other", None, HomeDoor::Open), &saved));
    }

    #[test]
    fn classification() {
        let mut saved = vec![mac("A", "Studio", 1)];
        let t = make_tag(&[1; 32], [3; 6]);
        assert_eq!(classify(&row("x", Some(t.clone()), HomeDoor::PairingRequired), &saved), RowClass::Paired { mac_id: "A".into(), by_tag: true });
        // Plain door: never dialed, even for a saved Mac.
        let c = classify(&row("Studio", Some(t.clone()), HomeDoor::Plain), &saved);
        assert_eq!(c, RowClass::UpdateSill);
        assert!(!c.dialable());
        assert_eq!(classify(&row("n", None, HomeDoor::Open), &saved), RowClass::OpenDoor);
        assert_eq!(classify(&row("n", None, HomeDoor::PairingRequired), &saved).word(), "Not paired");
        saved[0].standing = Standing::Revoked;
        assert!(matches!(classify(&row("x", Some(t), HomeDoor::PairingRequired), &saved), RowClass::NotPaired { was_saved: Some(_), .. }));
    }

    #[test]
    fn dial_hosts_order() {
        let r = Row {
            name: "m".into(),
            addrs: vec![
                IpAddr::V6("fe80::1".parse::<Ipv6Addr>().unwrap()),
                IpAddr::V6("fd00::2".parse().unwrap()),
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)),
            ],
            port: 9,
            tag: None,
            door: HomeDoor::Open,
        };
        assert_eq!(dial_hosts(&r), vec![("10.0.0.5".to_string(), 9), ("fd00::2".to_string(), 9)]);
    }

    #[test]
    fn backoff() {
        let v: Vec<u64> = (1..=8).map(browser_backoff).collect();
        assert_eq!(v, [1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[test]
    fn ladder() {
        assert_eq!(redial_row(0, None, true, 100.0), Redial::Now);
        assert_eq!(redial_row(1, Some(100.0), true, 103.0), Redial::At(105.0));
        assert_eq!(redial_row(1, Some(100.0), true, 105.0), Redial::Now);
        assert_eq!(redial_row(2, Some(100.0), true, 101.0), Redial::At(110.0));
        assert_eq!(redial_row(3, Some(100.0), true, 101.0), Redial::At(120.0));
        assert_eq!(redial_row(4, Some(100.0), true, 101.0), Redial::At(130.0));
        assert_eq!(redial_row(11, Some(100.0), true, 101.0), Redial::At(130.0));
        assert_eq!(redial_row(12, Some(100.0), true, 1e9), Redial::No);
        assert_eq!(redial_row(0, None, false, 0.0), Redial::No);
    }

    #[test]
    fn quit() {
        assert_eq!(quit_hold(Some(10.0), 5.0, 11.0), Some(13.0));
        assert_eq!(quit_hold(Some(10.0), 5.0, 13.0), None);
        assert_eq!(quit_hold(Some(10.0), 10.5, 11.0), None); // listed afresh since the quit
        assert_eq!(quit_hold(None, 5.0, 11.0), None);
    }

    #[test]
    fn lines() {
        assert_eq!(reconnect_line("M", 0), "Reconnecting to M\u{2026}");
        assert_eq!(reconnect_line("M", 2), "Reconnecting to M\u{2026}");
        assert!(reconnect_line("M", 3).contains("isn\u{2019}t answering"));
        assert!(reconnect_line("M", 11).contains("Sill keeps trying"));
        assert_eq!(reconnect_line("M", 12), "Stopped trying to reach M. Tap it to try again.");
    }
}
