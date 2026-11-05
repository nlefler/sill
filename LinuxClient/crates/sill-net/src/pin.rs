//! SPKI fingerprints and the signature check both ends share. Trust in Sill is "the SHA-256 of the
//! peer's SubjectPublicKeyInfo", never names, dates or chains (RemoteIdentity.swift, `SPKI`).

use rustls::crypto::{verify_tls13_signature_with_raw_key, CryptoProvider};
use rustls::pki_types::{CertificateDer, SubjectPublicKeyInfoDer};
use rustls::{DigitallySignedStruct, Error};
use sha2::{Digest, Sha256};

pub type Fingerprint = [u8; 32];

/// The DER SubjectPublicKeyInfo of a certificate, byte for byte as it sits in the certificate.
pub fn spki_of_cert(cert: &[u8]) -> Option<Vec<u8>> {
    let (_, c) = x509_parser::parse_x509_certificate(cert).ok()?;
    Some(c.tbs_certificate.subject_pki.raw.to_vec())
}

pub fn fingerprint_of_spki(spki: &[u8]) -> Fingerprint {
    Sha256::digest(spki).into()
}

pub fn fingerprint_of_cert(cert: &[u8]) -> Option<Fingerprint> {
    spki_of_cert(cert).map(|s| fingerprint_of_spki(&s))
}

pub fn hex(fp: &Fingerprint) -> String {
    fp.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn provider() -> std::sync::Arc<CryptoProvider> {
    std::sync::Arc::new(rustls::crypto::ring::default_provider())
}

/// Verify a TLS 1.3 CertificateVerify against the key inside the certificate. Done against the raw
/// SPKI rather than through webpki's certificate parser so that a certificate that is merely
/// unusual (Swift builds its own by hand) is not refused for reasons that do not matter here.
pub fn verify_signature(
    provider: &CryptoProvider,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<(), Error> {
    let spki = spki_of_cert(cert).ok_or_else(|| Error::General("certificate does not parse".into()))?;
    let spki = SubjectPublicKeyInfoDer::from(spki);
    verify_tls13_signature_with_raw_key(message, &spki, dss, &provider.signature_verification_algorithms)
        .map(|_| ())
}
