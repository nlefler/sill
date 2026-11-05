//! The device identity: a self-signed P-256 certificate whose key never leaves this machine.
//! Only the key's SPKI fingerprint matters to the Mac; the certificate is the vessel TLS needs.

use crate::pin::{fingerprint_of_cert, hex};
pub use crate::pin::Fingerprint;
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Identity {
    /// PKCS#8 DER of the private key.
    pub key_der: Vec<u8>,
    pub cert_der: Vec<u8>,
    pub fingerprint: Fingerprint,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Identity({})", hex(&self.fingerprint))
    }
}

impl Identity {
    /// A fresh key and certificate (valid long enough that expiry never matters: nobody checks).
    pub fn generate() -> Result<(Identity, String), String> {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
        let params = CertificateParams::new(vec!["sill".to_string()]).map_err(|e| e.to_string())?;
        let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
        let id = Identity::from_parts(key.serialize_der(), cert.der().to_vec())?;
        Ok((id, key.serialize_pem()))
    }

    pub fn generate_in_memory() -> Result<Identity, String> {
        Self::generate().map(|(i, _)| i)
    }

    fn from_parts(key_der: Vec<u8>, cert_der: Vec<u8>) -> Result<Identity, String> {
        let fingerprint = fingerprint_of_cert(&cert_der).ok_or("certificate does not parse")?;
        Ok(Identity { key_der, cert_der, fingerprint })
    }

    /// Load `key.pem` and `cert.der` from `dir`, creating both (key 0600) when missing.
    pub fn load_or_create(dir: &Path) -> Result<Identity, String> {
        let (kp, cp) = (dir.join("device-key.pem"), dir.join("device-cert.der"));
        if let (Ok(pem), Ok(cert)) = (std::fs::read_to_string(&kp), std::fs::read(&cp)) {
            let key = KeyPair::from_pem(&pem).map_err(|e| format!("{}: {e}", kp.display()))?;
            return Identity::from_parts(key.serialize_der(), cert);
        }
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let (id, pem) = Identity::generate()?;
        write_private(&kp, pem.as_bytes())?;
        std::fs::write(&cp, &id.cert_der).map_err(|e| e.to_string())?;
        Ok(id)
    }
}

/// Created 0600 from the start, so the key is never readable by others even briefly.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(bytes).map_err(|e| e.to_string())
}

/// `$XDG_DATA_HOME/sill`, else `~/.local/share/sill`.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(x).join("sill"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/sill"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn created_then_loaded_with_same_fingerprint_and_private_key() {
        let dir = std::env::temp_dir().join(format!("sill-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Identity::load_or_create(&dir).unwrap();
        let b = Identity::load_or_create(&dir).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(a.key_der, b.key_der);
        let mode = std::fs::metadata(dir.join("device-key.pem")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fingerprint_is_sha256_of_p256_spki() {
        let id = Identity::generate_in_memory().unwrap();
        let spki = crate::pin::spki_of_cert(&id.cert_der).unwrap();
        // 26-byte P-256 SPKI prefix (as Scripts/sillclient.py SPKI_PREFIX) + 65-byte point.
        assert_eq!(spki.len(), 91);
        assert_eq!(&spki[..2], &[0x30, 0x59]);
        assert_eq!(id.fingerprint, crate::pin::fingerprint_of_spki(&spki));
    }
}
