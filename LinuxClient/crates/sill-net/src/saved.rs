//! Saved Macs: what a device keeps after pairing. A JSON file under the XDG data directory (0600),
//! written atomically. The pin is the Mac's fingerprint; addresses are only where to dial.

use crate::addr;
use crate::b64;
use crate::macinfo::MacInfoCheck;
use crate::pairing::NAME_LIMIT;
use crate::pin::Fingerprint;
use crate::policy::GoodbyeOutcome;
use crate::safetext;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod b64_32 {
    use super::*;
    use serde::{de::Error, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&b64::url_encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let t = String::deserialize(d)?;
        b64::url_decode(&t).and_then(|b| b.try_into().ok()).ok_or_else(|| D::Error::custom("not 32 bytes of base64url"))
    }
}

/// Whether the Mac still takes this device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Standing {
    #[default]
    Paired,
    /// The Mac said goodbye `removed`: pair again.
    Revoked,
    /// The Mac said `pairingRequired`: a tap asks to pair.
    NeedsPairing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAddress {
    pub host: String,
    pub port: u16,
    /// From a verified kind 18 (replaced by the next one) rather than typed or scanned.
    #[serde(default)]
    pub learned: bool,
}

impl SavedAddress {
    pub fn typed(host: &str, port: u16) -> SavedAddress {
        SavedAddress { host: host.into(), port, learned: false }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedMac {
    pub mac_id: String,
    pub name: String,
    /// The pin.
    #[serde(with = "b64_32")]
    pub fingerprint: Fingerprint,
    /// From kind 20; recognises the Mac's Bonjour tag (M4).
    #[serde(with = "b64_32")]
    pub recognition_key: [u8; 32],
    pub remote_port: u16,
    pub addresses: Vec<SavedAddress>,
    pub paired_at: f64,
    #[serde(default)]
    pub last_connected_at: Option<f64>,
    #[serde(default)]
    pub info_issued_at: f64,
    /// "qr" or "code".
    pub method: String,
    #[serde(default)]
    pub standing: Standing,
}

pub const MAX_ADDRESSES: usize = 8;

impl SavedMac {
    /// Dial order: the one that last worked, then typed addresses, then learned ones, each once.
    pub fn dial_order(&self, last_worked: Option<&SavedAddress>) -> Vec<SavedAddress> {
        let mut out: Vec<SavedAddress> = vec![];
        let mut push = |a: &SavedAddress| {
            if !out.iter().any(|o| o.host == a.host && o.port == a.port) {
                out.push(a.clone());
            }
        };
        if let Some(l) = last_worked.and_then(|l| self.addresses.iter().find(|a| a.host == l.host && a.port == l.port)) {
            push(l);
        }
        self.addresses.iter().filter(|a| !a.learned).for_each(&mut push);
        self.addresses.iter().filter(|a| a.learned).for_each(&mut push);
        out
    }

    /// Take a kind 18 into the record: only a [`MacInfoCheck::Verified`] one (signed by the key the
    /// session saw) whose signer is this record's pin, naming this Mac, and newer than the last.
    /// Returns whether anything changed.
    pub fn apply_mac_info(&mut self, check: &MacInfoCheck, session_fp: &Fingerprint) -> bool {
        let MacInfoCheck::Verified(info) = check else { return false };
        if *session_fp != self.fingerprint || info.mac_id != self.mac_id || info.issued_at <= self.info_issued_at {
            return false;
        }
        self.info_issued_at = info.issued_at;
        if (1..=65535).contains(&info.remote_port) {
            self.remote_port = info.remote_port as u16;
        }
        let name = safetext::label(&info.name, NAME_LIMIT);
        if !name.is_empty() {
            self.name = name;
        }
        let mut learned: Vec<SavedAddress> = vec![];
        for a in &info.addresses {
            let Ok(p) = addr::parse(&a.host) else { continue };
            let port = a.port.filter(|p| (1..=65535).contains(p)).map(|p| p as u16).unwrap_or(self.remote_port);
            let s = SavedAddress { host: p.host, port, learned: true };
            if !learned.contains(&s) && !self.addresses.iter().any(|t| !t.learned && t.host == s.host && t.port == s.port) {
                learned.push(s);
            }
        }
        self.addresses.retain(|a| !a.learned);
        self.addresses.extend(learned);
        self.addresses.truncate(MAX_ADDRESSES);
        true
    }

    /// A goodbye `removed` or `pairingRequired` ends the pairing's use: no reconnect until paired
    /// again. The caller applies it only to a session pinned to this Mac (the iOS client's rule).
    pub fn apply_goodbye(&mut self, g: &GoodbyeOutcome) -> Option<Standing> {
        let s = match g.reason.as_str() {
            "removed" => Standing::Revoked,
            "pairingRequired" => Standing::NeedsPairing,
            _ => return None,
        };
        self.standing = s;
        Some(s)
    }

    pub fn can_connect(&self) -> bool {
        self.standing == Standing::Paired
    }
}

#[derive(Debug)]
pub struct SavedMacs {
    path: PathBuf,
    pub macs: Vec<SavedMac>,
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    macs: Vec<SavedMac>,
}

impl SavedMacs {
    pub fn default_path() -> Option<PathBuf> {
        crate::identity::default_dir().map(|d| d.join("saved-macs.json"))
    }

    /// A missing file is an empty list; one that does not parse is an error (never silently replaced).
    pub fn load(path: &Path) -> Result<SavedMacs, String> {
        let macs = match std::fs::read(path) {
            Ok(b) => serde_json::from_slice::<File>(&b).map_err(|e| format!("{}: {e}", path.display()))?.macs,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(SavedMacs { path: path.to_path_buf(), macs })
    }

    /// Write a temporary file (0600, synced) and rename it over the real one.
    pub fn save(&self) -> Result<(), String> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let dir = self.path.parent().ok_or("no directory")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension(format!("json.tmp{}", std::process::id()));
        let bytes = serde_json::to_vec_pretty(&File { version: 1, macs: self.macs.clone() }).map_err(|e| e.to_string())?;
        let write = || -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", self.path.display())
        })
    }

    pub fn get(&self, mac_id: &str) -> Option<&SavedMac> {
        self.macs.iter().find(|m| m.mac_id == mac_id)
    }

    pub fn get_mut(&mut self, mac_id: &str) -> Option<&mut SavedMac> {
        self.macs.iter_mut().find(|m| m.mac_id == mac_id)
    }

    /// Add a Mac, or replace the record of the same Mac ID (pairing again).
    pub fn upsert(&mut self, mac: SavedMac) {
        match self.macs.iter_mut().find(|m| m.mac_id == mac.mac_id) {
            Some(m) => *m = mac,
            None => self.macs.push(mac),
        }
    }

    pub fn remove(&mut self, mac_id: &str) -> bool {
        let n = self.macs.len();
        self.macs.retain(|m| m.mac_id != mac_id);
        self.macs.len() != n
    }

    /// Rename; the name is cleaned and may not end up empty.
    pub fn rename(&mut self, mac_id: &str, name: &str) -> bool {
        let clean = safetext::label(name, NAME_LIMIT);
        match self.get_mut(mac_id) {
            Some(m) if !clean.is_empty() => {
                m.name = clean;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macinfo::MacInfoCheck;
    use sill_proto::{MacAddress, MacInfo};
    use std::os::unix::fs::PermissionsExt;

    fn mac() -> SavedMac {
        SavedMac {
            mac_id: "ABCDEFGHJKMNPQRS".into(),
            name: "Studio".into(),
            fingerprint: [7; 32],
            recognition_key: [9; 32],
            remote_port: 7455,
            addresses: vec![SavedAddress::typed("10.0.0.2", 7455), SavedAddress { host: "old.ts.net".into(), port: 7455, learned: true }],
            paired_at: 1.0,
            last_connected_at: None,
            info_issued_at: 0.0,
            method: "qr".into(),
            standing: Standing::Paired,
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sill-saved-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("saved-macs.json")
    }

    #[test]
    fn round_trip_private_atomic() {
        let p = tmp("rt");
        let mut s = SavedMacs::load(&p).unwrap();
        assert!(s.macs.is_empty());
        s.upsert(mac());
        s.save().unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1, "no temp file left");
        let mut t = SavedMacs::load(&p).unwrap();
        assert_eq!(t.macs, vec![mac()]);
        assert!(t.rename("ABCDEFGHJKMNPQRS", "  Office \n Mac "));
        assert!(!t.rename("ABCDEFGHJKMNPQRS", "\u{200B}"));
        assert!(!t.rename("nope", "x"));
        assert_eq!(t.get("ABCDEFGHJKMNPQRS").unwrap().name, "Office Mac");
        t.upsert(mac()); // pairing again replaces
        assert_eq!(t.macs.len(), 1);
        t.save().unwrap();
        assert!(t.remove("ABCDEFGHJKMNPQRS") && !t.remove("ABCDEFGHJKMNPQRS"));
        std::fs::write(&p, b"{not json").unwrap();
        assert!(SavedMacs::load(&p).is_err());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn dial_order_prefers_last_worked_then_typed() {
        let m = mac();
        let order = m.dial_order(Some(&SavedAddress { host: "old.ts.net".into(), port: 7455, learned: true }));
        assert_eq!(order.iter().map(|a| a.host.as_str()).collect::<Vec<_>>(), ["old.ts.net", "10.0.0.2"]);
        let order = m.dial_order(None);
        assert_eq!(order.iter().map(|a| a.host.as_str()).collect::<Vec<_>>(), ["10.0.0.2", "old.ts.net"]);
        let unknown = SavedAddress::typed("9.9.9.9", 1);
        assert_eq!(m.dial_order(Some(&unknown)).len(), 2);
    }

    fn info(issued: f64, id: &str) -> MacInfo {
        MacInfo {
            v: 1, mac_id: id.into(), name: "Renamed".into(), issued_at: issued, remote_access: true, remote_port: 8000, internet: false,
            addresses: vec![
                MacAddress { host: "100.1.2.3".into(), port: None, kind: "vpn".into(), via: "tailscale".into() },
                MacAddress { host: "10.0.0.2".into(), port: Some(7455), kind: "lan".into(), via: "en0".into() },
                MacAddress { host: "bad host".into(), port: None, kind: "lan".into(), via: "x".into() },
            ],
        }
    }

    #[test]
    fn mac_info_updates_only_when_verified_pinned_and_newer() {
        let mut m = mac();
        let fp = m.fingerprint;
        let v = MacInfoCheck::Verified(info(5.0, &m.mac_id));
        assert!(!m.clone().apply_mac_info(&MacInfoCheck::Invalid, &fp));
        assert!(!m.clone().apply_mac_info(&MacInfoCheck::Foreign { info: info(5.0, &m.mac_id), signer: [1; 32] }, &fp));
        assert!(!m.clone().apply_mac_info(&v, &[1; 32]), "session key is not the pin");
        assert!(!m.clone().apply_mac_info(&MacInfoCheck::Verified(info(5.0, "OTHER")), &fp));
        assert!(m.apply_mac_info(&v, &fp));
        assert_eq!((m.name.as_str(), m.remote_port, m.info_issued_at), ("Renamed", 8000, 5.0));
        let hosts: Vec<_> = m.addresses.iter().map(|a| (a.host.as_str(), a.port, a.learned)).collect();
        assert_eq!(hosts, [("10.0.0.2", 7455, false), ("100.1.2.3", 8000, true)]);
        assert!(!m.apply_mac_info(&v, &fp), "not newer");
    }

    #[test]
    fn goodbye_marks_standing() {
        let g = |r: &str| GoodbyeOutcome { reason: r.into(), message: None, reconnect: false, is_notice: false };
        let mut m = mac();
        assert_eq!(m.apply_goodbye(&g("quit")), None);
        assert!(m.can_connect());
        assert_eq!(m.apply_goodbye(&g("removed")), Some(Standing::Revoked));
        assert!(!m.can_connect());
        assert_eq!(m.apply_goodbye(&g("pairingRequired")), Some(Standing::NeedsPairing));
    }
}
