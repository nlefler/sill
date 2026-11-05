//! The client's TLS config: 1.3 only, no tickets, SNI "sill", one ALPN, our certificate presented,
//! and a verifier that trusts a pinned SPKI fingerprint and nothing else.

use crate::identity::Identity;
use crate::pin::{self, Fingerprint};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::Resumption;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use std::sync::{Arc, Mutex};

pub const SNI: &str = "sill";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alpn {
    /// `sill/1`: a paired key's session.
    Session,
    /// `sill-pair/1`: one pairing request and its answer.
    Pairing,
}

impl Alpn {
    pub fn wire(self) -> &'static [u8] {
        match self {
            Alpn::Session => b"sill/1",
            Alpn::Pairing => b"sill-pair/1",
        }
    }
}

#[derive(Clone, Debug)]
pub enum PinMode {
    /// The Mac's key must hash to this; otherwise the handshake fails before any application data.
    Pinned(Fingerprint),
    /// Accept any key and report it ([`Observed`]): the typed-code pairing path, which proves the
    /// Mac afterwards with the code's HMAC (M3).
    TrustOnFirstUse,
}

/// The fingerprint the Mac presented, readable after the handshake.
#[derive(Clone, Debug, Default)]
pub struct Observed(Arc<Mutex<Option<Fingerprint>>>);

impl Observed {
    pub fn get(&self) -> Option<Fingerprint> {
        *self.0.lock().unwrap()
    }
}

#[derive(Debug)]
struct PinVerifier {
    mode: PinMode,
    observed: Observed,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let fp = pin::fingerprint_of_cert(end_entity).ok_or(Error::InvalidCertificate(CertificateError::BadEncoding))?;
        *self.observed.0.lock().unwrap() = Some(fp);
        match &self.mode {
            PinMode::Pinned(want) if *want != fp => {
                Err(Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
            }
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::General("TLS 1.2 is not spoken".into()))
    }

    fn verify_tls13_signature(&self, m: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        pin::verify_signature(&self.provider, m, cert, dss)?;
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Build the client config. `Observed` is filled by the verifier during the handshake.
pub fn client_config(identity: &Identity, alpn: Alpn, mode: PinMode) -> Result<(Arc<ClientConfig>, Observed), String> {
    let provider = pin::provider();
    let observed = Observed::default();
    let verifier = Arc::new(PinVerifier { mode, observed: observed.clone(), provider: provider.clone() });
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der.clone()));
    let mut cfg = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![CertificateDer::from(identity.cert_der.clone())], key)
        .map_err(|e| e.to_string())?;
    cfg.alpn_protocols = vec![alpn.wire().to_vec()];
    cfg.resumption = Resumption::disabled();
    cfg.enable_early_data = false;
    Ok((Arc::new(cfg), observed))
}
