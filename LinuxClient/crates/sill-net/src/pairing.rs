//! Pairing, device side (Pairing.swift, Remote.swift; docs/remote-access-plan.md §3, home-pairing-plan.md §3):
//! the typed code and its Damm check digit, the Mac ID, the `sill://pair` link, the proofs, and the
//! one-request exchange on a `sill-pair/1` connection.
//!
//! The Mac's fingerprint comes from the TLS session, never from a message. Nothing is saved before
//! proof_M has been verified.

use crate::addr::{self, ParsedAddress};
use crate::b64;
use crate::identity::Identity;
use crate::pin::Fingerprint;
use crate::safetext;
use crate::saved::{SavedAddress, SavedMac, Standing};
use crate::session::{connect_tls, ConnectError};
use crate::tls::{Alpn, PinMode};
use ring::{hmac, pbkdf2};
use sill_proto::*;
use std::num::NonZeroU32;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ---------------------------------------------------------------- the typed code

const DAMM: [[u8; 10]; 10] = [
    [0, 3, 1, 7, 5, 9, 8, 6, 4, 2], [7, 0, 9, 2, 1, 5, 4, 8, 6, 3], [4, 2, 0, 6, 8, 7, 1, 3, 5, 9],
    [1, 7, 5, 0, 9, 8, 3, 4, 2, 6], [6, 1, 2, 3, 0, 4, 5, 9, 7, 8], [3, 6, 7, 4, 2, 0, 9, 5, 8, 1],
    [5, 8, 6, 9, 7, 2, 0, 1, 3, 4], [8, 9, 4, 5, 3, 6, 2, 0, 1, 7], [9, 4, 3, 8, 6, 1, 7, 2, 0, 5],
    [2, 5, 8, 1, 4, 3, 6, 7, 9, 0],
];

pub const CODE_LENGTH: usize = 12;

/// Why typed input is not a code yet: said locally, nothing is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeProblem {
    /// "A code has 12 digits."
    Length,
    /// "That code has a typo. Check it against your Mac."
    Typo,
}

impl CodeProblem {
    pub fn text(self) -> &'static str {
        match self {
            CodeProblem::Length => "A code has 12 digits.",
            CodeProblem::Typo => "That code has a typo. Check it against your Mac.",
        }
    }
}

/// The Damm interim digit over `digits` (each 0-9): 0 for a valid code, else the check digit to append.
pub fn check_digit(digits: &[u8]) -> u8 {
    digits.iter().fold(0u8, |i, d| DAMM[i as usize][*d as usize])
}

/// `input` as a code: spaces, hyphens and no-break spaces removed, exactly 12 ASCII digits, check digit valid.
pub fn check_code(input: &str) -> Result<String, CodeProblem> {
    let kept: String = input.chars().filter(|c| !matches!(c, ' ' | '-' | '\u{00A0}')).collect();
    if kept.chars().count() != CODE_LENGTH || !kept.bytes().all(|b| b.is_ascii_digit()) {
        return Err(CodeProblem::Length);
    }
    let digits: Vec<u8> = kept.bytes().map(|b| b - b'0').collect();
    if check_digit(&digits) != 0 {
        return Err(CodeProblem::Typo);
    }
    Ok(kept)
}

/// "4829 1355 7208".
pub fn group_code(code: &str) -> String {
    code.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

/// A code with the check digit appended to 11 given digits (the Mac makes codes; tests and the mock too).
pub fn code_from_digits(first_eleven: &[u8; 11]) -> String {
    let mut d = first_eleven.to_vec();
    d.push(check_digit(&d));
    d.iter().map(|x| (b'0' + x) as char).collect()
}

// ---------------------------------------------------------------- the Mac ID

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Crockford base32 of the fingerprint's first 10 bytes: 16 characters.
pub fn mac_id(fingerprint: &[u8]) -> String {
    if fingerprint.len() < 10 {
        return String::new();
    }
    let mut out = String::with_capacity(16);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for b in &fingerprint[..10] {
        buffer = buffer << 8 | *b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(CROCKFORD[((buffer >> bits) & 31) as usize] as char);
        }
        buffer &= (1 << bits) - 1;
    }
    out
}

pub fn mac_id_is_well_formed(t: &str) -> bool {
    t.len() == 16 && t.bytes().all(|b| CROCKFORD.contains(&b))
}

// ---------------------------------------------------------------- proofs

pub const ROUNDS: u32 = 600_000;
const LABEL: &[u8] = b"sill-pair-v1";

/// The typed path's key: PBKDF2-HMAC-SHA256 of the 12 digits, salt "sill-pair-v1" || fp_mac.
/// `None` for anything that is not 12 ASCII digits.
pub fn code_key(code: &str, mac_fp: &[u8], rounds: u32) -> Option<[u8; 32]> {
    if code.len() != CODE_LENGTH || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut salt = LABEL.to_vec();
    salt.extend_from_slice(mac_fp);
    let mut out = [0u8; 32];
    pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256, NonZeroU32::new(rounds)?, &salt, code.as_bytes(), &mut out);
    Some(out)
}

fn message(role: &[u8], first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut m = LABEL.to_vec();
    m.push(b' ');
    m.extend_from_slice(role);
    m.push(0);
    m.extend_from_slice(first);
    m.extend_from_slice(second);
    m
}

fn tag(key: &[u8], msg: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), msg).as_ref().to_vec()
}

fn verify(key: &[u8], msg: &[u8], proof: &[u8]) -> bool {
    hmac::verify(&hmac::Key::new(hmac::HMAC_SHA256, key), msg, proof).is_ok() // constant time
}

/// proof_D = HMAC(key, "sill-pair-v1 device" 0x00 fp_device fp_mac)
pub fn device_proof(key: &[u8], fp_device: &[u8], fp_mac: &[u8]) -> Vec<u8> {
    tag(key, &message(b"device", fp_device, fp_mac))
}

/// proof_M = HMAC(key, "sill-pair-v1 mac" 0x00 fp_mac fp_device)
pub fn mac_proof(key: &[u8], fp_mac: &[u8], fp_device: &[u8]) -> Vec<u8> {
    tag(key, &message(b"mac", fp_mac, fp_device))
}

/// The Mac's side of the check (the mock host uses it), constant time.
pub fn is_valid_device_proof(proof: &[u8], key: &[u8], fp_device: &[u8], fp_mac: &[u8]) -> bool {
    verify(key, &message(b"device", fp_device, fp_mac), proof)
}

/// The device's check of proof_M, constant time.
pub fn is_valid_mac_proof(proof: &[u8], key: &[u8], fp_mac: &[u8], fp_device: &[u8]) -> bool {
    verify(key, &message(b"mac", fp_mac, fp_device), proof)
}

// ---------------------------------------------------------------- the link

pub const MAX_LINK_ADDRESSES: usize = 5;
pub const NAME_LIMIT: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairLink {
    pub mac_id: String,
    pub fingerprint: Fingerprint,
    /// The QR path's one-time key.
    pub secret: [u8; 16],
    pub name: String,
    /// The Mac's remote port.
    pub port: u16,
    pub addresses: Vec<ParsedAddress>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkFailure {
    NotALink,
    Version,
    /// `m`, `k`, `s` or `p` missing, repeated or malformed.
    Field(&'static str),
    MacIdMismatch,
    /// An `a` the address parser refuses.
    Address(String),
}

impl LinkFailure {
    /// "That's not a Sill code." is the device's word for every one of these.
    pub fn text(&self) -> &'static str {
        "That\u{2019}s not a Sill code."
    }
}

impl PairLink {
    pub fn new(fingerprint: Fingerprint, secret: [u8; 16], name: &str, port: u16, addresses: Vec<ParsedAddress>) -> PairLink {
        let clean = safetext::label(name, NAME_LIMIT);
        PairLink {
            mac_id: mac_id(&fingerprint),
            fingerprint,
            secret,
            name: if clean.is_empty() { "Mac".into() } else { clean },
            port,
            addresses: addresses.into_iter().take(MAX_LINK_ADDRESSES).collect(),
        }
    }

    /// The link as the QR code carries it: values percent-encoded down to unreserved characters and ":".
    pub fn url(&self) -> String {
        let mut items = vec![
            ("v", "1".to_string()),
            ("m", self.mac_id.clone()),
            ("k", b64::url_encode(&self.fingerprint)),
            ("s", b64::url_encode(&self.secret)),
            ("n", self.name.clone()),
            ("p", self.port.to_string()),
        ];
        items.extend(self.addresses.iter().take(MAX_LINK_ADDRESSES).map(|a| ("a", a.text())));
        let q: Vec<String> = items.iter().map(|(k, v)| format!("{k}={}", escape(v))).collect();
        format!("sill://pair?{}", q.join("&"))
    }

    /// Each address with its own port, else the link's.
    pub fn dial_addresses(&self) -> Vec<(String, u16)> {
        self.addresses.iter().map(|a| (a.host.clone(), a.port.unwrap_or(self.port))).collect()
    }

    pub fn parse(text: &str) -> Result<PairLink, LinkFailure> {
        let t = text.trim();
        let (scheme, rest) = t.split_once("://").ok_or(LinkFailure::NotALink)?;
        if !scheme.eq_ignore_ascii_case("sill") {
            return Err(LinkFailure::NotALink);
        }
        let rest = rest.split('#').next().unwrap_or("");
        let (authority_path, query) = rest.split_once('?').ok_or(LinkFailure::NotALink)?;
        let (host, path) = match authority_path.split_once('/') {
            Some((h, p)) => (h, p),
            None => (authority_path, ""),
        };
        if !host.eq_ignore_ascii_case("pair") || !path.is_empty() {
            return Err(LinkFailure::NotALink);
        }
        // name -> value; a bare name (no "=") has no value, which `single` refuses.
        let items: Vec<(String, Option<String>)> = query
            .split('&')
            .filter(|s| !s.is_empty())
            .map(|s| match s.split_once('=') {
                Some((k, v)) => (k.to_string(), unescape(v)),
                None => (s.to_string(), None),
            })
            .collect();
        let single = |key: &str| -> Option<&str> {
            let v: Vec<_> = items.iter().filter(|(k, _)| k == key).collect();
            match v.as_slice() {
                [(_, Some(s))] if !s.is_empty() => Some(s.as_str()),
                _ => None,
            }
        };
        if single("v") != Some("1") {
            return Err(LinkFailure::Version);
        }
        let m = single("m").filter(|m| mac_id_is_well_formed(m)).ok_or(LinkFailure::Field("m"))?;
        let fp: Fingerprint = single("k").and_then(b64::url_decode).and_then(|d| d.try_into().ok()).ok_or(LinkFailure::Field("k"))?;
        let secret: [u8; 16] = single("s").and_then(b64::url_decode).and_then(|d| d.try_into().ok()).ok_or(LinkFailure::Field("s"))?;
        let port = single("p")
            .filter(|p| p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|p| p.parse::<u32>().ok())
            .filter(|p| (1..=65535).contains(p))
            .ok_or(LinkFailure::Field("p"))? as u16;
        if m != mac_id(&fp) {
            return Err(LinkFailure::MacIdMismatch);
        }
        let mut addresses = vec![];
        for (k, v) in items.iter().filter(|(k, _)| k == "a") {
            let _ = k;
            let raw = v.clone().unwrap_or_default();
            addresses.push(addr::parse(&raw).map_err(|_| LinkFailure::Address(raw))?);
        }
        let name = items.iter().find(|(k, _)| k == "n").and_then(|(_, v)| v.clone()).unwrap_or_default();
        Ok(PairLink::new(fp, secret, &name, port, addresses))
    }
}

fn escape(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b':') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// Percent-decoding as a URL query item (no "+" to space); `None` for bad escapes or invalid UTF-8.
fn unescape(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut o = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            o.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            o.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(o).ok()
}

// ---------------------------------------------------------------- errors and their words

#[derive(Clone, Debug, PartialEq)]
pub enum PairError {
    /// Typed input that is not a code yet; nothing was sent.
    Code(CodeProblem),
    NoAddress,
    /// Nothing reached a Sill door at these addresses (connection, TLS or ALPN failed).
    NothingAnswered(String),
    /// The Mac at the address presented another key than the link's: nothing was sent.
    WrongMac(String),
    /// "code": the proof was wrong.
    WrongCode { tries_left: i64 },
    /// "stopped": five wrong codes.
    Stopped,
    Expired,
    /// "busy": too many tries too fast; `retry_after` seconds.
    Busy { retry_after: Option<f64> },
    /// "closed", or nothing is pairing right now (also "shown", "openOnMac", "locked" for a proof).
    NotPairing,
    /// A reason this build does not know, with the Mac's own words.
    MacSaid(String),
    /// kind 20 ok, but proof_M did not check: the Mac does not know the code. Nothing saved.
    ProofFailed,
    /// An answer that is not a well-formed kind 20 ok (no proof, wrong Mac ID, no recognition key…).
    BadReply(String),
    /// No kind 20 in time.
    Timeout,
}

impl PairError {
    /// The words, as the iOS client says them (StreamClient+Remote `PairingProblem`).
    pub fn text(&self, mac: &str) -> String {
        match self {
            PairError::Code(c) => c.text().into(),
            PairError::NoAddress => "That doesn\u{2019}t look like an address. Try 100.101.102.103 or mac.example.net.".into(),
            PairError::NothingAnswered(host) => format!("Nothing answered at {host}. Check the address, and that Sill is open on your Mac."),
            PairError::WrongMac(host) => format!(
                "Pairing didn\u{2019}t finish: the Mac at {host} couldn\u{2019}t show it knows the code. Pair from the same network as your Mac, or scan the code instead."
            ),
            PairError::ProofFailed => format!(
                "Pairing didn\u{2019}t finish: {mac} couldn\u{2019}t show it knows the code. Pair from the same network as your Mac, or scan the code instead."
            ),
            PairError::BadReply(_) => format!("Something answered, but it isn\u{2019}t Sill, or its Sill is too old to pair ({mac})."),
            PairError::WrongCode { tries_left } => {
                let n = (*tries_left).max(0);
                format!("That code didn\u{2019}t work. Check the code on your Mac. {n}\u{00A0}tr{} left.", if n == 1 { "y" } else { "ies" })
            }
            PairError::Stopped => format!("{mac} stopped pairing after too many wrong codes. Choose Pair iPhone or iPad\u{2026} on your Mac for a new code."),
            PairError::Expired => "This code expired. Choose Pair iPhone or iPad\u{2026} on your Mac for a new one.".into(),
            PairError::Busy { .. } | PairError::NotPairing => {
                format!("{mac} isn\u{2019}t pairing right now. On your Mac, choose Pair iPhone or iPad\u{2026} first.")
            }
            PairError::MacSaid(words) => words.clone(),
            PairError::Timeout => format!("{mac} didn\u{2019}t answer. Check that Sill is open on it, then try again."),
        }
    }
}

impl std::fmt::Display for PairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.text("The Mac"))
    }
}
impl std::error::Error for PairError {}

/// A refused kind 20 as a typed error (`StreamClient.problem(for:mac:)`).
pub fn refusal(r: &PairResult) -> PairError {
    if let Some(words) = r.unknown_reason_message() {
        return PairError::MacSaid(clean_words(words));
    }
    match r.reason.as_deref() {
        Some("code") => PairError::WrongCode { tries_left: r.tries_left.unwrap_or(0).max(0) },
        Some("expired") => PairError::Expired,
        Some("stopped") => PairError::Stopped,
        Some("busy") => PairError::Busy { retry_after: r.retry_after },
        _ => PairError::NotPairing,
    }
}

fn clean_words(s: &str) -> String {
    crate::policy::clean_message(s, 300).unwrap_or_default()
}

// ---------------------------------------------------------------- the exchange

#[derive(Clone, Debug)]
pub struct PairOptions {
    pub identity: Identity,
    /// "Linux", the host name…; cleaned like the Mac cleans it.
    pub device_name: String,
    pub model: Option<String>,
    /// Everything, connection included (10 s).
    pub timeout: Duration,
    /// PBKDF2 rounds; [`ROUNDS`] unless a test lowers it (the Mac uses 600,000).
    pub rounds: u32,
}

impl PairOptions {
    pub fn new(identity: Identity, device_name: &str) -> PairOptions {
        PairOptions { identity, device_name: device_name.into(), model: None, timeout: Duration::from_secs(10), rounds: ROUNDS }
    }
}

/// Write one kind 19, read one kind 20 (header first, so an oversized answer is refused before its
/// bytes are read). The host closes afterwards; whatever follows is not read.
pub async fn exchange<S: AsyncReadExt + AsyncWriteExt + Unpin>(s: &mut S, req: &PairRequest) -> Result<PairResult, PairError> {
    let payload = encode_json(req);
    if payload.len() > MAX_PAIRING_PAYLOAD {
        return Err(PairError::BadReply("request too large".into()));
    }
    s.write_all(&Message::client(Kind::PairRequest, payload).serialize()).await.map_err(|e| PairError::NothingAnswered(e.to_string()))?;
    s.flush().await.map_err(|e| PairError::NothingAnswered(e.to_string()))?;
    let mut head = [0u8; HEADER_LENGTH];
    s.read_exact(&mut head).await.map_err(|e| PairError::NothingAnswered(format!("no answer ({e})")))?;
    let h = Header::parse(&head).ok_or_else(|| PairError::BadReply("header".into()))?;
    if h.kind != Kind::PairResult {
        return Err(PairError::BadReply(format!("kind {} instead of 20", h.raw_kind)));
    }
    if h.payload_length > MAX_PAIRING_PAYLOAD {
        return Err(PairError::BadReply("answer too large".into()));
    }
    let mut body = vec![0u8; h.payload_length];
    s.read_exact(&mut body).await.map_err(|e| PairError::NothingAnswered(format!("answer cut short ({e})")))?;
    decode_json::<PairResult>(&body).ok_or_else(|| PairError::BadReply("kind 20 did not decode".into()))
}

pub enum Credential<'a> {
    /// The link's one-time secret (key = secret).
    Secret(&'a [u8; 16]),
    /// The typed code (key = PBKDF2).
    Code(&'a str),
}

/// What a finished exchange becomes: the kind 20 judged against the key and both fingerprints.
/// Verifies proof_M before anything exists to save.
#[allow(clippy::too_many_arguments)]
pub fn conclude(
    r: &PairResult,
    key: &[u8],
    mac_fp: &Fingerprint,
    device_fp: &Fingerprint,
    fallback_name: &str,
    addresses: Vec<SavedAddress>,
    remote_port: u16,
    method: &str,
    now: f64,
) -> Result<SavedMac, PairError> {
    if !r.ok {
        return Err(refusal(r));
    }
    let proof = r.proof.as_deref().and_then(b64::url_decode).ok_or_else(|| PairError::BadReply("ok without a proof".into()))?;
    if !is_valid_mac_proof(&proof, key, mac_fp, device_fp) {
        return Err(PairError::ProofFailed);
    }
    let id = mac_id(mac_fp);
    if r.mac_id.as_deref() != Some(id.as_str()) {
        return Err(PairError::BadReply("the Mac ID is not the key\u{2019}s".into()));
    }
    let recognition: [u8; 32] = r
        .recognition_key
        .as_deref()
        .and_then(b64::url_decode)
        .and_then(|d| d.try_into().ok())
        .ok_or_else(|| PairError::BadReply("no recognition key".into()))?;
    let name = safetext::label(r.name.as_deref().unwrap_or(fallback_name), NAME_LIMIT);
    let name = if name.is_empty() { safetext::label(fallback_name, NAME_LIMIT) } else { name };
    Ok(SavedMac {
        mac_id: id,
        name: if name.is_empty() { "Mac".into() } else { name },
        fingerprint: *mac_fp,
        recognition_key: recognition,
        remote_port,
        addresses,
        paired_at: now,
        last_connected_at: None,
        info_issued_at: 0.0,
        method: method.to_string(),
        standing: Standing::Paired,
    })
}

fn wall_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn request(opts: &PairOptions, method: &str, proof: &[u8]) -> PairRequest {
    let model = opts.model.as_deref().map(|m| safetext::label(m, NAME_LIMIT)).filter(|m| !m.is_empty());
    PairRequest {
        v: PairRequest::VERSION,
        method: method.into(),
        proof: b64::url_encode(proof),
        name: safetext::label(&opts.device_name, NAME_LIMIT),
        model,
        cable: None,
    }
}

fn connect_error(host: &str, e: ConnectError) -> PairError {
    match e {
        ConnectError::PinMismatch { .. } => PairError::WrongMac(host.to_string()),
        _ => PairError::NothingAnswered(host.to_string()),
    }
}

/// Pair by a scanned or pasted link. The Mac must present the link's key (the handshake fails
/// otherwise, before kind 19 is written); each of the link's addresses is tried in order.
pub async fn pair_with_link(link: &PairLink, opts: &PairOptions) -> Result<SavedMac, PairError> {
    let addresses = link.dial_addresses();
    if addresses.is_empty() {
        return Err(PairError::NoAddress);
    }
    tokio::time::timeout(opts.timeout, async {
        let mut last = PairError::NoAddress;
        for (host, port) in &addresses {
            let (mut tls, fp) = match connect_tls((host, *port), &opts.identity, Alpn::Pairing, PinMode::Pinned(link.fingerprint), opts.timeout).await {
                Ok(c) => c,
                Err(e) => {
                    let e = connect_error(host, e);
                    // A wrong key at one address outranks "nothing answered" at another.
                    if !matches!((&last, &e), (PairError::WrongMac(_), PairError::NothingAnswered(_))) {
                        last = e;
                    }
                    continue;
                }
            };
            let proof = device_proof(&link.secret, &opts.identity.fingerprint, &fp);
            let r = exchange(&mut tls, &request(opts, PairRequest::QR, &proof)).await?;
            let saved: Vec<SavedAddress> =
                addresses.iter().map(|(h, p)| SavedAddress::typed(h, *p)).collect();
            return conclude(&r, &link.secret, &fp, &opts.identity.fingerprint, &link.name, saved, link.port, PairRequest::QR, wall_now());
        }
        Err(last)
    })
    .await
    .map_err(|_| PairError::Timeout)?
}

/// Pair by the typed code at an address. The Mac's key is learned from the TLS session (or must equal
/// `pin` when the caller already saw it); the proofs bind it, so a relay makes proof_M fail.
pub async fn pair_with_code(
    code: &str,
    host: &str,
    port: u16,
    pin: Option<Fingerprint>,
    opts: &PairOptions,
) -> Result<SavedMac, PairError> {
    let code = check_code(code).map_err(PairError::Code)?;
    tokio::time::timeout(opts.timeout, async {
        let mode = pin.map(PinMode::Pinned).unwrap_or(PinMode::TrustOnFirstUse);
        let (mut tls, fp) = connect_tls((host, port), &opts.identity, Alpn::Pairing, mode, opts.timeout)
            .await
            .map_err(|e| connect_error(host, e))?;
        let rounds = opts.rounds;
        let (c, f) = (code.clone(), fp);
        let key = tokio::task::spawn_blocking(move || code_key(&c, &f, rounds))
            .await
            .map_err(|e| PairError::BadReply(e.to_string()))?
            .ok_or(PairError::Code(CodeProblem::Length))?;
        let proof = device_proof(&key, &opts.identity.fingerprint, &fp);
        let r = exchange(&mut tls, &request(opts, PairRequest::CODE, &proof)).await?;
        let saved = vec![SavedAddress::typed(host, port)];
        conclude(&r, &key, &fp, &opts.identity.fingerprint, "Mac", saved, port, PairRequest::CODE, wall_now())
    })
    .await
    .map_err(|_| PairError::Timeout)?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }
    fn fp_mac() -> Vec<u8> {
        (0u8..32).collect()
    }
    fn fp_dev() -> Vec<u8> {
        (32u8..64).collect()
    }

    // Expected values below were computed once with OpenSSL 3.6 (`openssl kdf PBKDF2`, `openssl mac HMAC`)
    // and coreutils (`base32` with the alphabet mapped to Crockford's), not with this code.

    #[test]
    fn damm_known_answers() {
        assert_eq!(check_digit(&[5, 7, 2]), 4); // the textbook example
        assert_eq!(check_code("482913557208").unwrap(), "482913557208");
        assert_eq!(code_from_digits(&[4, 8, 2, 9, 1, 3, 5, 5, 7, 2, 0]), "482913557208");
        assert_eq!(code_from_digits(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 1]), "123456789018");
        assert_eq!(code_from_digits(&[0; 11]), "000000000000");
        assert_eq!(group_code("482913557208"), "4829 1355 7208");
    }

    #[test]
    fn damm_catches_every_single_substitution_and_adjacent_swap() {
        let good = "482913557208";
        for i in 0..12 {
            for d in b'0'..=b'9' {
                if good.as_bytes()[i] == d {
                    continue;
                }
                let mut t = good.as_bytes().to_vec();
                t[i] = d;
                assert_eq!(check_code(std::str::from_utf8(&t).unwrap()), Err(CodeProblem::Typo));
            }
            if i < 11 && good.as_bytes()[i] != good.as_bytes()[i + 1] {
                let mut t = good.as_bytes().to_vec();
                t.swap(i, i + 1);
                assert_eq!(check_code(std::str::from_utf8(&t).unwrap()), Err(CodeProblem::Typo));
            }
        }
    }

    #[test]
    fn code_input_tolerance_and_errors() {
        for ok in ["4829 1355 7208", "4829-1355-7208", " 4829\u{00A0}1355 - 7208 ", "482913557208"] {
            assert_eq!(check_code(ok).as_deref(), Ok("482913557208"), "{ok:?}");
        }
        for (bad, e) in [
            ("", CodeProblem::Length), ("48291355720", CodeProblem::Length), ("4829135572080", CodeProblem::Length),
            ("48291355720a", CodeProblem::Length), ("4829\t1355 7208", CodeProblem::Length), ("\u{0664}82913557208", CodeProblem::Length),
            ("482913557209", CodeProblem::Typo),
        ] {
            assert_eq!(check_code(bad), Err(e), "{bad:?}");
        }
        assert_eq!(CodeProblem::Length.text(), "A code has 12 digits.");
        assert_eq!(CodeProblem::Typo.text(), "That code has a typo. Check it against your Mac.");
    }

    #[test]
    fn mac_id_known_answers() {
        assert_eq!(mac_id(&fp_mac()), "000G40R40M30E209");
        assert_eq!(mac_id(&[0xff; 32]), "ZZZZZZZZZZZZZZZZ");
        assert_eq!(mac_id(&[0; 32]), "0000000000000000");
        assert_eq!(mac_id(&[1; 9]), "");
        assert!(mac_id_is_well_formed("000G40R40M30E209"));
        assert!(!mac_id_is_well_formed("000g40r40m30e209") && !mac_id_is_well_formed("000I40R40M30E209") && !mac_id_is_well_formed("0"));
    }

    #[test]
    fn pbkdf2_key_and_proofs_match_openssl() {
        let k = code_key("123456789012", &fp_mac(), ROUNDS).unwrap();
        assert_eq!(k.to_vec(), hex("8a03e060e4eba37bb89cc8319384c2188e52b4edc432ac500ddd826fb304fcc1"));
        assert_eq!(device_proof(&k, &fp_dev(), &fp_mac()), hex("ef6016c980791a5d6fbce332c6579fe97b548b639a6d44afc6c7dd1f9816a1f9"));
        assert_eq!(mac_proof(&k, &fp_mac(), &fp_dev()), hex("061e5a332531c3e17a68eef417d1fef24035d7f4a502e184d33728bb6ec7443a"));
        let q: Vec<u8> = (0u8..16).collect(); // the QR path: the secret is the key
        assert_eq!(device_proof(&q, &fp_dev(), &fp_mac()), hex("0eec0363f46e2a92107e5cfc2748023e21f7c595d4df51bc67aa713bf84223bb"));
        assert_eq!(mac_proof(&q, &fp_mac(), &fp_dev()), hex("6bb12dd0a9d9475510c1266a3b3bd400521722b613b878508be8a63b057afb59"));
    }

    #[test]
    fn proof_verification_binds_roles_and_fingerprints() {
        let k = [3u8; 16];
        let (m, d) = (fp_mac(), fp_dev());
        let pm = mac_proof(&k, &m, &d);
        assert!(is_valid_mac_proof(&pm, &k, &m, &d));
        assert!(!is_valid_mac_proof(&pm, &k, &d, &m), "swapped fingerprints");
        assert!(!is_valid_mac_proof(&pm, &[4u8; 16], &m, &d));
        assert!(!is_valid_mac_proof(&pm[..31], &k, &m, &d));
        assert!(!is_valid_device_proof(&pm, &k, &d, &m), "a Mac proof is not a device proof");
        let pd = device_proof(&k, &d, &m);
        assert!(is_valid_device_proof(&pd, &k, &d, &m));
        let mut other_mac = m.clone();
        other_mac[0] ^= 1; // a relay presenting its own key
        assert!(!is_valid_device_proof(&pd, &k, &d, &other_mac));
        assert_eq!(code_key("12345678901", &m, 10), None);
        assert_eq!(code_key("12345678901a", &m, 10), None);
    }

    fn good_link() -> PairLink {
        let fp: Fingerprint = fp_mac().try_into().unwrap();
        PairLink::new(fp, [9; 16], "Tom & Jerry\u{202E}'s Mac", 7455, vec![addr::parse("100.64.0.1").unwrap(), addr::parse("[fd7a::1]:9000").unwrap()])
    }

    fn url_with(f: impl Fn(&str, &str) -> Option<String>) -> String {
        let u = good_link().url();
        let (head, q) = u.split_once('?').unwrap();
        let items: Vec<String> = q.split('&').filter_map(|i| { let (k, v) = i.split_once('=').unwrap(); f(k, v) }).collect();
        format!("{head}?{}", items.join("&"))
    }

    #[test]
    fn link_round_trip() {
        let l = good_link();
        assert_eq!(l.name, "Tom & Jerry's Mac");
        let u = l.url();
        assert!(u.starts_with("sill://pair?v=1&m=000G40R40M30E209&k=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8&s=CQkJCQkJCQkJCQkJCQkJCQ&n=Tom%20%26%20Jerry%27s%20Mac&p=7455&a=100.64.0.1&a=%5Bfd7a::1%5D:9000"), "{u}");
        assert_eq!(PairLink::parse(&u).unwrap(), l);
        assert_eq!(PairLink::parse(&format!("  {}\n", u.replace("sill://pair", "SILL://Pair"))).unwrap(), l);
        assert_eq!(l.dial_addresses(), vec![("100.64.0.1".to_string(), 7455), ("fd7a::1".to_string(), 9000)]);
    }

    #[test]
    fn link_failures_mirror_swift() {
        use LinkFailure::*;
        let drop = |key: &'static str| move |k: &str, v: &str| (k != key).then(|| format!("{k}={v}"));
        let set = |key: &'static str, val: &'static str| move |k: &str, v: &str| Some(if k == key { format!("{k}={val}") } else { format!("{k}={v}") });
        let dup = |key: &'static str| move |k: &str, v: &str| Some(if k == key { format!("{k}={v}&{k}={v}") } else { format!("{k}={v}") });
        assert_eq!(PairLink::parse("http://pair?v=1"), Err(NotALink));
        assert_eq!(PairLink::parse("sill://other?v=1"), Err(NotALink));
        assert_eq!(PairLink::parse("sill://pair/x?v=1"), Err(NotALink));
        assert_eq!(PairLink::parse("sill://pair"), Err(NotALink));
        assert_eq!(PairLink::parse("not a url"), Err(NotALink));
        assert_eq!(PairLink::parse(&url_with(drop("v"))), Err(Version));
        assert_eq!(PairLink::parse(&url_with(set("v", "2"))), Err(Version));
        assert_eq!(PairLink::parse(&url_with(dup("v"))), Err(Version));
        for key in ["m", "k", "s", "p"] {
            assert_eq!(PairLink::parse(&url_with(drop(key))), Err(Field(key)), "missing {key}");
            assert_eq!(PairLink::parse(&url_with(dup(key))), Err(Field(key)), "repeated {key}");
        }
        assert_eq!(PairLink::parse(&url_with(set("m", "000g40r40m30e209"))), Err(Field("m")));
        assert_eq!(PairLink::parse(&url_with(set("k", "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh"))), Err(Field("k"))); // 31 bytes
        assert_eq!(PairLink::parse(&url_with(set("s", "CQkJ"))), Err(Field("s")));
        for p in ["0", "65536", "123456", "80a", "-1"] {
            assert_eq!(PairLink::parse(&url_with(set("p", p))), Err(Field("p")), "{p}");
        }
        assert_eq!(PairLink::parse(&url_with(set("m", "0000000000000000"))), Err(MacIdMismatch));
        assert_eq!(PairLink::parse(&url_with(set("a", "1.2.3"))), Err(Address("1.2.3".into())));
        assert_eq!(PairLink::parse(&url_with(set("a", "fe80%3A%3A1%25en0"))), Err(Address("fe80::1%en0".into())));
    }

    #[test]
    fn link_tolerances() {
        // Unknown parameters ignored, name optional ("Mac"), more than five addresses cut to five.
        let u = url_with(|k, v| if k == "n" { None } else { Some(format!("{k}={v}")) });
        let l = PairLink::parse(&format!("{u}&future=1&a=1.1.1.1&a=2.2.2.2&a=3.3.3.3&a=4.4.4.4")).unwrap();
        assert_eq!(l.name, "Mac");
        assert_eq!(l.addresses.len(), 5);
        assert_eq!(PairLink::parse(&format!("{u}#frag")).unwrap().addresses.len(), 2);
        let long = url_with(|k, v| Some(if k == "n" { format!("n={}", "x".repeat(200)) } else { format!("{k}={v}") }));
        assert_eq!(PairLink::parse(&long).unwrap().name.len(), 64);
    }

    #[test]
    fn refusal_reasons_and_words() {
        let r = |reason: &str| PairResult { ok: false, reason: Some(reason.into()), ..Default::default() };
        assert_eq!(refusal(&PairResult { tries_left: Some(4), ..r("code") }), PairError::WrongCode { tries_left: 4 });
        assert_eq!(refusal(&PairResult { tries_left: Some(-1), ..r("code") }), PairError::WrongCode { tries_left: 0 });
        assert_eq!(refusal(&r("expired")), PairError::Expired);
        assert_eq!(refusal(&r("stopped")), PairError::Stopped);
        assert_eq!(refusal(&PairResult { retry_after: Some(2.5), ..r("busy") }), PairError::Busy { retry_after: Some(2.5) });
        for known in ["closed", "shown", "openOnMac", "locked"] {
            assert_eq!(refusal(&r(known)), PairError::NotPairing);
        }
        // A reason this build does not know, with the Mac's words: those, cleaned.
        let said = PairResult { message: Some("Try later\u{202E}\nsecond".into()), ..r("someNewReason") };
        assert_eq!(refusal(&said), PairError::MacSaid("Try later".into()));
        // Unknown with no words: a guess of "not pairing".
        assert_eq!(refusal(&r("someNewReason")), PairError::NotPairing);
        assert_eq!(PairError::WrongCode { tries_left: 1 }.text("M"), "That code didn\u{2019}t work. Check the code on your Mac. 1\u{00A0}try left.");
        assert!(PairError::WrongCode { tries_left: 4 }.text("M").ends_with("4\u{00A0}tries left."));
        assert!(PairError::Stopped.text("Studio").starts_with("Studio stopped pairing after too many wrong codes."));
        assert!(PairError::NotPairing.text("Studio").starts_with("Studio isn\u{2019}t pairing right now."));
    }

    #[test]
    fn conclude_refuses_bad_ok_answers_and_saves_nothing() {
        let (m, d): (Fingerprint, Fingerprint) = (fp_mac().try_into().unwrap(), fp_dev().try_into().unwrap());
        let key = [1u8; 16];
        let ok = PairResult {
            ok: true,
            proof: Some(b64::url_encode(&mac_proof(&key, &m, &d))),
            mac_id: Some(mac_id(&m)),
            name: Some("  Studio\n".into()),
            recognition_key: Some(b64::url_encode(&[2u8; 32])),
            ..Default::default()
        };
        let go = |r: &PairResult| conclude(r, &key, &m, &d, "Fallback", vec![], 7455, "qr", 1.0);
        let saved = go(&ok).unwrap();
        assert_eq!((saved.name.as_str(), saved.mac_id.as_str(), saved.fingerprint, saved.recognition_key), ("Studio", "000G40R40M30E209", m, [2; 32]));
        assert_eq!(go(&PairResult { proof: None, ..ok.clone() }), Err(PairError::BadReply("ok without a proof".into())));
        let mut bad = mac_proof(&key, &m, &d);
        bad[31] ^= 0x80;
        assert_eq!(go(&PairResult { proof: Some(b64::url_encode(&bad)), ..ok.clone() }), Err(PairError::ProofFailed));
        assert!(matches!(go(&PairResult { mac_id: Some("0000000000000000".into()), ..ok.clone() }), Err(PairError::BadReply(_))));
        assert!(matches!(go(&PairResult { recognition_key: Some(b64::url_encode(&[2u8; 31])), ..ok.clone() }), Err(PairError::BadReply(_))));
        assert!(matches!(go(&PairResult { recognition_key: None, ..ok.clone() }), Err(PairError::BadReply(_))));
        assert_eq!(go(&PairResult { name: None, ..ok.clone() }).unwrap().name, "Fallback");
        assert_eq!(go(&PairResult { ok: false, reason: Some("code".into()), ..ok }), Err(PairError::WrongCode { tries_left: 0 }));
    }
}
