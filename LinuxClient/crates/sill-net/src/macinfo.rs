//! Kind 18, the signed MacInfo (`SignedMacInfo.verified()` in Remote.swift): who this Mac is and how to
//! reach it. Checked here, and accepted for a session only when the key that signed it is the key the
//! session's own TLS handshake saw.

use crate::b64;
use crate::identity::Identity;
use crate::pairing::mac_id;
use crate::pin::{fingerprint_of_spki, Fingerprint};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, UnparsedPublicKey, ECDSA_P256_SHA256_ASN1, ECDSA_P256_SHA256_ASN1_SIGNING};
use sill_proto::{decode_json, encode_json, MacInfo, SignedMacInfo};

/// DER SubjectPublicKeyInfo of an uncompressed P-256 point (the 26-byte prefix, then the 65 bytes).
pub fn spki_der(point: &[u8]) -> Vec<u8> {
    let mut d = vec![
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01, 0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D,
        0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ];
    d.extend_from_slice(point);
    d
}

/// The MacInfo and the fingerprint of the key that signed it, when the signature verifies over the
/// exact `info` bytes, the key is a 65-byte P-256 point, the JSON decodes and its `macID` is that key's.
pub fn verified(s: &SignedMacInfo) -> Option<(MacInfo, Fingerprint)> {
    let bytes = b64::std_decode(&s.info)?;
    let point = b64::std_decode(&s.key).filter(|p| p.len() == 65 && p[0] == 4)?;
    let sig = b64::std_decode(&s.sig)?;
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, &point).verify(&bytes, &sig).ok()?;
    let info: MacInfo = decode_json(&bytes)?;
    let fp = fingerprint_of_spki(&spki_der(&point));
    (info.mac_id == mac_id(&fp)).then_some((info, fp))
}

/// What a session makes of a kind 18.
#[derive(Clone, Debug, PartialEq)]
pub enum MacInfoCheck {
    /// Valid, and signed by the key this session's handshake saw: it names this session's Mac.
    Verified(MacInfo),
    /// Valid, but signed by another key (a relay replaying a Mac's info, or another Mac). For display
    /// at most; never saved, never names the session's Mac.
    Foreign { info: MacInfo, signer: Fingerprint },
    /// Does not verify.
    Invalid,
}

pub fn check(s: &SignedMacInfo, session_fp: &Fingerprint) -> MacInfoCheck {
    match verified(s) {
        Some((info, fp)) if fp == *session_fp => MacInfoCheck::Verified(info),
        Some((info, signer)) => MacInfoCheck::Foreign { info, signer },
        None => MacInfoCheck::Invalid,
    }
}

/// Sign `info` with the identity's key, as a Mac does (the mock host and the tests).
pub fn sign(identity: &Identity, info: &MacInfo) -> Result<SignedMacInfo, String> {
    let rng = SystemRandom::new();
    let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &identity.key_der, &rng).map_err(|e| e.to_string())?;
    let bytes = encode_json(info);
    let sig = kp.sign(&rng, &bytes).map_err(|e| e.to_string())?;
    Ok(SignedMacInfo {
        info: b64::std_encode(&bytes),
        key: b64::std_encode(kp.public_key().as_ref()),
        sig: b64::std_encode(sig.as_ref()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str) -> MacInfo {
        MacInfo { v: 1, mac_id: id.into(), name: "Studio".into(), issued_at: 100.0, remote_access: true, remote_port: 7455, internet: false, addresses: vec![] }
    }

    #[test]
    fn spki_matches_certificate_fingerprint() {
        let id = Identity::generate_in_memory().unwrap();
        let s = sign(&id, &info("X")).unwrap();
        let point = b64::std_decode(&s.key).unwrap();
        assert_eq!(fingerprint_of_spki(&spki_der(&point)), id.fingerprint);
    }

    #[test]
    fn good_bad_and_foreign() {
        let mac = Identity::generate_in_memory().unwrap();
        let other = Identity::generate_in_memory().unwrap();
        let good = sign(&mac, &info(&mac_id(&mac.fingerprint))).unwrap();
        assert!(matches!(check(&good, &mac.fingerprint), MacInfoCheck::Verified(i) if i.name == "Studio"));
        // Verifies, but the session's handshake saw another key.
        assert!(matches!(check(&good, &other.fingerprint), MacInfoCheck::Foreign { signer, .. } if signer == mac.fingerprint));
        // Tampered info bytes.
        let mut t = good.clone();
        let mut b = b64::std_decode(&t.info).unwrap();
        b[10] ^= 1;
        t.info = b64::std_encode(&b);
        assert_eq!(check(&t, &mac.fingerprint), MacInfoCheck::Invalid);
        // A macID that is not the signing key's.
        let wrong = sign(&mac, &info("0123456789ABCDEF")).unwrap();
        assert_eq!(check(&wrong, &mac.fingerprint), MacInfoCheck::Invalid);
        // Signed by one key, claimed by another.
        let mut swapped = good.clone();
        swapped.key = sign(&other, &info("x")).unwrap().key;
        assert_eq!(check(&swapped, &mac.fingerprint), MacInfoCheck::Invalid);
        // Garbage fields, a short point, the unsigned placeholder.
        for (i, k, s) in [("e30=", "", ""), ("!!", "AA==", "AA=="), (good.info.as_str(), "AAAA", good.sig.as_str())] {
            let bad = SignedMacInfo { info: i.into(), key: k.into(), sig: s.into() };
            assert_eq!(check(&bad, &mac.fingerprint), MacInfoCheck::Invalid);
        }
    }
}
