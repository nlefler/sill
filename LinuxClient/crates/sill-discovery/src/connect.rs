//! Dialing: a saved Mac pinned to its key over the discovered then the saved addresses, first
//! success wins, attempts staggered; and a manual add by address.

use sill_net::saved::SavedMac;
use sill_net::session::{connect, ConnectError, ConnectOptions, Session};
use sill_net::tls::PinMode;
use std::time::Duration;
use tokio::task::JoinSet;

/// `RemoteTLS.dialTimeout`.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// Happy-eyeballs-lite: the next address starts this long after the last, or as soon as it failed.
pub const STAGGER: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct DialConfig {
    pub stagger: Duration,
    pub timeout: Duration,
}
impl Default for DialConfig {
    fn default() -> Self {
        DialConfig { stagger: STAGGER, timeout: DIAL_TIMEOUT }
    }
}

pub enum ConnectOutcome {
    Connected { session: Session, address: (String, u16) },
    /// A Mac answered with another key than the one paired: it has a new key since (set up again).
    /// Nothing was sent. Pair again; do not retry.
    NewKey { address: (String, u16) },
    /// The record says the Mac removed this device or asks it to pair.
    NotAllowed,
    /// Nothing answered; one line per address.
    Unreachable(Vec<String>),
}

impl ConnectOutcome {
    pub fn words(&self, mac: &str) -> Option<String> {
        match self {
            ConnectOutcome::NewKey { .. } => {
                Some(format!("{mac} has a new key since this computer paired. Tap it to pair again."))
            }
            ConnectOutcome::NotAllowed => Some(format!("{mac} no longer takes this computer. Tap it to pair again.")),
            ConnectOutcome::Unreachable(_) => Some(format!("{mac} didn\u{2019}t answer.")),
            ConnectOutcome::Connected { .. } => None,
        }
    }
}

/// The addresses to try: discovery's first, then the record's `dial_order`, each once.
pub fn candidates(mac: &SavedMac, discovered: &[(String, u16)]) -> Vec<(String, u16)> {
    let mut out: Vec<(String, u16)> = vec![];
    for a in discovered.iter().cloned().chain(mac.dial_order(None).into_iter().map(|a| (a.host, a.port))) {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// Dial `mac` pinned to its fingerprint. `opts.pin` is replaced by the pin; its connect timeout by `cfg`'s.
pub async fn connect_saved(
    mac: &SavedMac,
    discovered: &[(String, u16)],
    mut opts: ConnectOptions,
    cfg: &DialConfig,
) -> ConnectOutcome {
    if !mac.can_connect() {
        return ConnectOutcome::NotAllowed;
    }
    opts.pin = PinMode::Pinned(mac.fingerprint);
    opts.config.connect_timeout = cfg.timeout;
    race(candidates(mac, discovered), opts, cfg.stagger).await
}

/// Manual add: one address, with the pin the caller has (`TrustOnFirstUse` for an unpaired open door).
pub async fn connect_by_address(host: &str, port: u16, pin: PinMode, mut opts: ConnectOptions, cfg: &DialConfig) -> ConnectOutcome {
    opts.pin = pin;
    opts.config.connect_timeout = cfg.timeout;
    race(vec![(host.to_string(), port)], opts, cfg.stagger).await
}

async fn race(addrs: Vec<(String, u16)>, opts: ConnectOptions, stagger: Duration) -> ConnectOutcome {
    let mut set: JoinSet<Attempt> = JoinSet::new();
    let mut next = 0;
    let mut deadline = tokio::time::Instant::now();
    let mut errors: Vec<String> = vec![];
    let mut new_key: Option<(String, u16)> = None;
    loop {
        if next < addrs.len() && (set.is_empty() || next == 0) {
            start(&mut set, &addrs[next], &opts);
            next += 1;
            deadline = tokio::time::Instant::now() + stagger;
            continue;
        }
        if set.is_empty() {
            break;
        }
        let timer = async {
            if next < addrs.len() { tokio::time::sleep_until(deadline).await } else { std::future::pending().await }
        };
        tokio::select! {
            joined = set.join_next() => {
                let Some(Ok((res, addr))) = joined else { continue };
                match res {
                    Ok(session) => {
                        set.abort_all();
                        return ConnectOutcome::Connected { session, address: addr };
                    }
                    Err(ConnectError::PinMismatch { .. }) => {
                        // Another key answered: the Mac is set up again. Do not retry anywhere.
                        set.abort_all();
                        new_key.get_or_insert(addr);
                        break;
                    }
                    Err(e) => errors.push(format!("{}:{}: {e}", addr.0, addr.1)),
                }
            }
            _ = timer => {
                start(&mut set, &addrs[next], &opts);
                next += 1;
                deadline = tokio::time::Instant::now() + stagger;
            }
        }
    }
    match new_key {
        Some(address) => ConnectOutcome::NewKey { address },
        None => ConnectOutcome::Unreachable(errors),
    }
}

type Attempt = (Result<Session, ConnectError>, (String, u16));

fn start(set: &mut JoinSet<Attempt>, addr: &(String, u16), opts: &ConnectOptions) {
    let (addr, opts) = (addr.clone(), opts.clone());
    set.spawn(async move { (connect((&addr.0, addr.1), opts).await, addr) });
}

impl std::fmt::Debug for ConnectOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectOutcome::Connected { address, .. } => write!(f, "Connected({address:?})"),
            ConnectOutcome::NewKey { address } => write!(f, "NewKey({address:?})"),
            ConnectOutcome::NotAllowed => write!(f, "NotAllowed"),
            ConnectOutcome::Unreachable(e) => write!(f, "Unreachable({e:?})"),
        }
    }
}
