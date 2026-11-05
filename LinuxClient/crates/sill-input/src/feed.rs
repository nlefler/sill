//! The Mac's pointer reports (kind 26) against what this device has sent: a report carries `seen`, the
//! input messages (kind 8) the host had read on this connection when it built it, and one that has
//! read fewer than this device sent since is stale (PointerPresence.isFresh,
//! SessionLink.inputsOnSession). The count restarts with each connection.

use sill_proto::MacPointer;

#[derive(Debug, Default, Clone)]
pub struct PointerFeed {
    sent: i64,
}

impl PointerFeed {
    pub fn new() -> PointerFeed {
        PointerFeed::default()
    }

    /// One input message was handed to the connection.
    pub fn input_sent(&mut self) {
        self.sent += 1;
    }

    pub fn sent(&self) -> i64 {
        self.sent
    }

    /// Whether the host had read everything this device sent when it built `report` (a missing `seen` counts as 0).
    pub fn is_fresh(&self, report: &MacPointer) -> bool {
        report.seen.unwrap_or(0) >= self.sent
    }

    /// The position to draw, if the report is fresh and the Mac's pointer is inside the picture.
    pub fn accept(&self, report: &MacPointer) -> Option<Option<(f64, f64)>> {
        self.is_fresh(report).then(|| report.position())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(seen: Option<i64>) -> MacPointer {
        MacPointer { x: Some(0.5), y: Some(0.5), inside: Some(true), seen }
    }

    #[test]
    fn a_report_older_than_this_devices_input_is_dropped() {
        let mut f = PointerFeed::new();
        assert!(f.is_fresh(&report(None)), "nothing sent yet");
        f.input_sent();
        f.input_sent();
        assert!(!f.is_fresh(&report(None)));
        assert!(!f.is_fresh(&report(Some(1))));
        assert!(f.is_fresh(&report(Some(2))));
        assert_eq!(f.accept(&report(Some(3))), Some(Some((0.5, 0.5))));
        assert_eq!(f.accept(&report(Some(0))), None);
        let off = MacPointer { inside: Some(false), seen: Some(5), ..Default::default() };
        assert_eq!(f.accept(&off), Some(None));
    }
}
