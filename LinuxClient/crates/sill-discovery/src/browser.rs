//! Browsing `_sill._tcp`: a [`Browser`] trait (so tests can use a fake), the mdns-sd implementation,
//! the table of rows with added/updated/removed events, and the supervisor that makes a failed browser
//! again after 1, 2, 4, 8, 16, then 30 s.

use crate::policy::{browser_backoff, HomeDoor, Row, TXT_DOOR, TXT_TAG};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub const DEFAULT_SERVICE_TYPE: &str = "_sill._tcp";

/// What a browser reports.
#[derive(Clone, Debug, PartialEq)]
pub enum BrowserEvent {
    /// A service resolved (or resolved again with new addresses or TXT).
    Resolved(Row),
    /// The instance name went away.
    Removed(String),
    /// The browser died; make it again.
    Failed(String),
}

pub trait Browser: Send + Sync + 'static {
    /// Start one browse. Its channel closing counts as a failure.
    fn browse(&self) -> Result<mpsc::UnboundedReceiver<BrowserEvent>, String>;
}

/// What the row table tells the app.
#[derive(Clone, Debug, PartialEq)]
pub enum RowEvent {
    Added(Row),
    Updated(Row),
    Removed(String),
}

/// Rows by instance name.
#[derive(Default, Debug)]
pub struct RowTable {
    rows: BTreeMap<String, Row>,
}

impl RowTable {
    pub fn rows(&self) -> Vec<Row> {
        self.rows.values().cloned().collect()
    }
    pub fn get(&self, name: &str) -> Option<&Row> {
        self.rows.get(name)
    }
    pub fn apply(&mut self, e: &BrowserEvent) -> Option<RowEvent> {
        match e {
            BrowserEvent::Resolved(r) => match self.rows.insert(r.name.clone(), r.clone()) {
                None => Some(RowEvent::Added(r.clone())),
                Some(old) if old != *r => Some(RowEvent::Updated(r.clone())),
                Some(_) => None,
            },
            BrowserEvent::Removed(n) => self.rows.remove(n).map(|_| RowEvent::Removed(n.clone())),
            BrowserEvent::Failed(_) => None,
        }
    }
}

/// Run `browser` until `out` closes, making it again after a failure with the policy's backoff. A
/// browse that delivers a row resets the failure count. `sleep` is injected so tests need no waiting.
pub async fn run_browser(
    browser: Arc<dyn Browser>,
    out: mpsc::UnboundedSender<BrowserEvent>,
    sleep: impl Fn(Duration) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send,
) {
    let mut failures = 0u32;
    loop {
        if let Ok(mut rx) = browser.browse() {
            {
                while let Some(ev) = rx.recv().await {
                    match ev {
                        BrowserEvent::Failed(_) => break,
                        other => {
                            if matches!(other, BrowserEvent::Resolved(_)) {
                                failures = 0;
                            }
                            if out.send(other).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        }
        if out.is_closed() {
            return;
        }
        failures += 1;
        sleep(Duration::from_secs(browser_backoff(failures))).await;
    }
}

/// The real browser: the mdns-sd daemon.
pub struct MdnsBrowser {
    service_type: String,
}

impl MdnsBrowser {
    /// `service_type` like `_sill._tcp` (or `_silltest._tcp` in tests).
    pub fn new(service_type: &str) -> MdnsBrowser {
        MdnsBrowser { service_type: service_type.trim_end_matches('.').trim_end_matches(".local").to_string() }
    }
    fn full_type(&self) -> String {
        format!("{}.local.", self.service_type)
    }
}

impl Default for MdnsBrowser {
    fn default() -> Self {
        MdnsBrowser::new(DEFAULT_SERVICE_TYPE)
    }
}

/// The instance name of a fullname like "Noah's Mac._sill._tcp.local.".
pub fn instance_name(fullname: &str, full_type: &str) -> String {
    fullname.strip_suffix(full_type).map(|s| s.trim_end_matches('.')).unwrap_or(fullname).to_string()
}

fn row_of(info: &mdns_sd::ServiceInfo, full_type: &str) -> Row {
    let door = HomeDoor::from_txt(info.get_property(TXT_DOOR).map(|p| p.val().and_then(|v| std::str::from_utf8(v).ok())));
    let tag = info.get_property(TXT_TAG).map(|p| p.val_str().to_string());
    let mut addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
    addrs.sort();
    Row { name: instance_name(info.get_fullname(), full_type), addrs, port: info.get_port(), tag, door }
}

impl Browser for MdnsBrowser {
    fn browse(&self) -> Result<mpsc::UnboundedReceiver<BrowserEvent>, String> {
        let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| e.to_string())?;
        let ty = self.full_type();
        let rx = daemon.browse(&ty).map_err(|e| e.to_string())?;
        let (tx, out) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            use mdns_sd::ServiceEvent::*;
            loop {
                let ev = match rx.recv() {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = tx.send(BrowserEvent::Failed(e.to_string()));
                        break;
                    }
                };
                let sent = match ev {
                    ServiceResolved(info) => tx.send(BrowserEvent::Resolved(row_of(&info, &ty))),
                    ServiceRemoved(_, full) => tx.send(BrowserEvent::Removed(instance_name(&full, &ty))),
                    SearchStopped(_) => {
                        let _ = tx.send(BrowserEvent::Failed("search stopped".into()));
                        break;
                    }
                    _ => Ok(()),
                };
                if sent.is_err() {
                    break; // nobody listens any more
                }
            }
            let _ = daemon.shutdown();
        });
        Ok(out)
    }
}

/// A browser driven by hand, for tests.
#[derive(Default)]
pub struct FakeBrowser {
    inner: std::sync::Mutex<FakeInner>,
}
#[derive(Default)]
struct FakeInner {
    tx: Option<mpsc::UnboundedSender<BrowserEvent>>,
    starts: usize,
    fail_next: usize,
}

impl FakeBrowser {
    pub fn push(&self, e: BrowserEvent) {
        if let Some(tx) = &self.inner.lock().unwrap().tx {
            let _ = tx.send(e);
        }
    }
    pub fn starts(&self) -> usize {
        self.inner.lock().unwrap().starts
    }
    /// The next `n` calls to `browse` return an error.
    pub fn fail_next(&self, n: usize) {
        self.inner.lock().unwrap().fail_next = n;
    }
}

impl Browser for FakeBrowser {
    fn browse(&self) -> Result<mpsc::UnboundedReceiver<BrowserEvent>, String> {
        let mut g = self.inner.lock().unwrap();
        g.starts += 1;
        if g.fail_next > 0 {
            g.fail_next -= 1;
            return Err("fake failure".into());
        }
        let (tx, rx) = mpsc::unbounded_channel();
        g.tx = Some(tx);
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(n: &str, port: u16) -> Row {
        Row { name: n.into(), addrs: vec![], port, tag: None, door: HomeDoor::Open }
    }

    #[test]
    fn table_events() {
        let mut t = RowTable::default();
        assert_eq!(t.apply(&BrowserEvent::Resolved(row("a", 1))), Some(RowEvent::Added(row("a", 1))));
        assert_eq!(t.apply(&BrowserEvent::Resolved(row("a", 1))), None);
        assert_eq!(t.apply(&BrowserEvent::Resolved(row("a", 2))), Some(RowEvent::Updated(row("a", 2))));
        assert_eq!(t.apply(&BrowserEvent::Removed("a".into())), Some(RowEvent::Removed("a".into())));
        assert_eq!(t.apply(&BrowserEvent::Removed("a".into())), None);
        assert!(t.rows().is_empty());
    }

    #[test]
    fn instance_names() {
        assert_eq!(instance_name("Noah's Mac._sill._tcp.local.", "_sill._tcp.local."), "Noah's Mac");
        assert_eq!(MdnsBrowser::new("_silltest._tcp").full_type(), "_silltest._tcp.local.");
        assert_eq!(MdnsBrowser::new("_sill._tcp.local.").full_type(), "_sill._tcp.local.");
    }

    #[tokio::test(start_paused = true)]
    async fn restarts_with_backoff() {
        let fake = Arc::new(FakeBrowser::default());
        fake.fail_next(7);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let waits = Arc::new(std::sync::Mutex::new(vec![]));
        let w = waits.clone();
        let b: Arc<dyn Browser> = fake.clone();
        let h = tokio::spawn(run_browser(b, tx, move |d| {
            w.lock().unwrap().push(d.as_secs());
            Box::pin(async {})
        }));
        // After 7 failures the 8th browse works: deliver a row.
        for _ in 0..50 {
            tokio::task::yield_now().await;
            if fake.starts() >= 8 {
                break;
            }
        }
        fake.push(BrowserEvent::Resolved(row("a", 1)));
        assert!(matches!(rx.recv().await, Some(BrowserEvent::Resolved(_))));
        assert_eq!(*waits.lock().unwrap(), vec![1, 2, 4, 8, 16, 30, 30]);
        // A Failed after a row restarts at 1 s again.
        fake.push(BrowserEvent::Failed("x".into()));
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        assert_eq!(waits.lock().unwrap().last(), Some(&1));
        assert_eq!(fake.starts(), 9);
        drop(rx);
        fake.push(BrowserEvent::Failed("x".into()));
        let _ = h.await;
    }
}
