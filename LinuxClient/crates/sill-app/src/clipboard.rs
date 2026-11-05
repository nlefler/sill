//! The local clipboard behind a trait (so tests use a fake) and the worker thread that looks at it and
//! writes to it: a blocking read of the system clipboard must never stall the session task.
//!
//! The pure rules (what is sent, what is not echoed, the rate) are `sill_ui_state::ClipboardSync`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

pub trait ClipboardBackend: Send + 'static {
    /// The text on the clipboard, `None` when there is none (or it cannot be read).
    fn read_text(&mut self) -> Option<String>;
    fn write_text(&mut self, text: &str) -> bool;
}

/// The system clipboard through arboard (Wayland's data-control protocol, or X11). `None` when there is
/// no display to talk to (a test run, a console).
pub fn system() -> Option<Box<dyn ClipboardBackend>> {
    if std::env::var_os("SILL_NO_CLIPBOARD").is_some() {
        return None;
    }
    arboard::Clipboard::new().ok().map(|c| Box::new(Arboard(c)) as Box<dyn ClipboardBackend>)
}

struct Arboard(arboard::Clipboard);
impl ClipboardBackend for Arboard {
    fn read_text(&mut self) -> Option<String> {
        self.0.get_text().ok()
    }
    fn write_text(&mut self, text: &str) -> bool {
        self.0.set_text(text.to_string()).is_ok()
    }
}

enum Req {
    Look,
    Write(String),
}

pub struct ClipboardWorker {
    tx: Sender<Req>,
    busy: Arc<AtomicBool>,
    /// Bumped by each write: a look that began before it is out of date and is dropped.
    generation: Arc<AtomicU64>,
}

impl ClipboardWorker {
    /// `on_look` gets each look's result (`None`: no text), on the worker thread.
    pub fn spawn(mut backend: Box<dyn ClipboardBackend>, on_look: impl Fn(Option<String>) + Send + 'static) -> ClipboardWorker {
        let (tx, rx) = channel::<Req>();
        let busy = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(0));
        let (b, g) = (busy.clone(), generation.clone());
        std::thread::spawn(move || {
            while let Ok(req) = rx.recv() {
                match req {
                    Req::Look => {
                        let before = g.load(Ordering::SeqCst);
                        let text = backend.read_text();
                        if g.load(Ordering::SeqCst) == before {
                            on_look(text);
                        }
                        b.store(false, Ordering::SeqCst);
                    }
                    Req::Write(t) => {
                        backend.write_text(&t);
                    }
                }
            }
        });
        ClipboardWorker { tx, busy, generation }
    }

    /// Ask for a look unless one is already under way.
    pub fn look(&self) {
        if !self.busy.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(Req::Look);
        }
    }

    pub fn write(&self, text: String) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.tx.send(Req::Write(text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    struct Fake(Arc<Mutex<Option<String>>>);
    impl ClipboardBackend for Fake {
        fn read_text(&mut self) -> Option<String> {
            self.0.lock().unwrap().clone()
        }
        fn write_text(&mut self, text: &str) -> bool {
            *self.0.lock().unwrap() = Some(text.to_string());
            true
        }
    }

    fn wait_for(f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(3), "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn looks_report_and_writes_land() {
        let board = Arc::new(Mutex::new(Some("one".to_string())));
        let seen = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let s2 = seen.clone();
        let w = ClipboardWorker::spawn(Box::new(Fake(board.clone())), move |t| s2.lock().unwrap().push(t));
        w.look();
        wait_for(|| seen.lock().unwrap().len() == 1);
        assert_eq!(seen.lock().unwrap()[0].as_deref(), Some("one"));
        w.write("two".into());
        wait_for(|| board.lock().unwrap().as_deref() == Some("two"));
        w.look();
        wait_for(|| seen.lock().unwrap().len() == 2);
        assert_eq!(seen.lock().unwrap()[1].as_deref(), Some("two"));
    }

    #[test]
    fn a_look_that_began_before_a_write_is_dropped() {
        struct Slow(Arc<Mutex<Option<String>>>, Arc<AtomicBool>);
        impl ClipboardBackend for Slow {
            fn read_text(&mut self) -> Option<String> {
                let v = self.0.lock().unwrap().clone();
                while !self.1.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(2));
                }
                v
            }
            fn write_text(&mut self, _: &str) -> bool {
                true
            }
        }
        let go = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(0));
        let s2 = seen.clone();
        let w = ClipboardWorker::spawn(Box::new(Slow(Arc::new(Mutex::new(Some("old".into()))), go.clone())), move |_| *s2.lock().unwrap() += 1);
        w.look();
        std::thread::sleep(Duration::from_millis(30));
        w.write("new".into());
        go.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(*seen.lock().unwrap(), 0, "the stale look is not reported");
    }
}
