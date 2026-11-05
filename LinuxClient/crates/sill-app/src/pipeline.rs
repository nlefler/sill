//! The decode thread: wire frames in, the newest picture out. Never a queue of pictures: a picture the
//! window has not taken yet is replaced by the next one (latency beats quality).

use crate::seam::{UiEvent, UiSink};
use sill_media::{Decoder, DecoderPref, StatsCollector, VideoFrame};
use sill_proto::ParameterSets;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug)]
pub enum DecodeMsg {
    Params(ParameterSets),
    Frame { payload: Vec<u8>, timestamp: f64, keyframe: bool },
}

#[derive(Debug, PartialEq)]
pub enum DecodeNote {
    Interrupted(String),
    PictureBack,
}

/// The newest decoded picture, taken by the window.
#[derive(Clone, Default)]
pub struct FrameSlot(Arc<Mutex<Option<VideoFrame>>>);

impl FrameSlot {
    pub fn take(&self) -> Option<VideoFrame> {
        self.0.lock().unwrap().take()
    }
    /// Put a picture in; `true` when it replaced one nobody took.
    fn put(&self, f: VideoFrame) -> bool {
        self.0.lock().unwrap().replace(f).is_some()
    }
}

/// What the thread tells about itself, for the HUD.
#[derive(Clone, Default)]
pub struct DecoderInfo(Arc<Mutex<String>>);
impl DecoderInfo {
    pub fn get(&self) -> String {
        self.0.lock().unwrap().clone()
    }
    fn set(&self, s: String) {
        *self.0.lock().unwrap() = s;
    }
}

/// Deltas waiting this long behind the decoder (0.5 s at 60 fps) are thrown away and a keyframe asked for.
pub const MAX_BACKLOG: usize = 30;

pub struct Plan {
    pub msgs: Vec<DecodeMsg>,
    pub dropped: usize,
    /// Deltas were thrown away with no keyframe to resume from: the picture is lost.
    pub behind: bool,
}

/// What to decode of everything that queued while the decoder was busy. A keyframe in the batch
/// makes everything before it moot (only the sets that precede it stay); more than `max_backlog`
/// deltas with no keyframe are dropped, and the picture counts as lost.
pub fn plan_batch(batch: Vec<DecodeMsg>, max_backlog: usize) -> Plan {
    let last_key = batch.iter().rposition(|m| matches!(m, DecodeMsg::Frame { keyframe: true, .. }));
    let mut dropped = 0;
    if let Some(k) = last_key {
        let last_params = batch[..k].iter().rposition(|m| matches!(m, DecodeMsg::Params(_)));
        let mut msgs = vec![];
        for (i, m) in batch.into_iter().enumerate() {
            if i >= k || Some(i) == last_params {
                msgs.push(m);
            } else if matches!(m, DecodeMsg::Frame { .. }) {
                dropped += 1;
            }
        }
        return Plan { msgs, dropped, behind: false };
    }
    let frames = batch.iter().filter(|m| matches!(m, DecodeMsg::Frame { .. })).count();
    if frames > max_backlog {
        let msgs: Vec<DecodeMsg> = batch.into_iter().filter(|m| matches!(m, DecodeMsg::Params(_))).collect();
        return Plan { msgs, dropped: frames, behind: true };
    }
    Plan { msgs: batch, dropped: 0, behind: false }
}

pub struct DecodeThread {
    pub tx: Sender<DecodeMsg>,
    pub slot: FrameSlot,
    pub info: DecoderInfo,
    pub handle: std::thread::JoinHandle<()>,
}

pub fn spawn(
    pref: DecoderPref,
    stats: Arc<Mutex<StatsCollector>>,
    clock: Instant,
    notes: UnboundedSender<DecodeNote>,
    ui: UiSink,
) -> DecodeThread {
    let (tx, rx) = std::sync::mpsc::channel();
    let slot = FrameSlot::default();
    let info = DecoderInfo::default();
    let (s2, i2) = (slot.clone(), info.clone());
    let handle = std::thread::Builder::new()
        .name("sill-decode".into())
        .spawn(move || run(pref, rx, s2, i2, stats, clock, notes, ui))
        .expect("spawn decode thread");
    DecodeThread { tx, slot, info, handle }
}

#[allow(clippy::too_many_arguments)]
fn run(
    pref: DecoderPref,
    rx: Receiver<DecodeMsg>,
    slot: FrameSlot,
    info: DecoderInfo,
    stats: Arc<Mutex<StatsCollector>>,
    clock: Instant,
    notes: UnboundedSender<DecodeNote>,
    ui: UiSink,
) {
    let mut dec = match Decoder::new(pref) {
        Ok(d) => d,
        Err(e) => {
            info.set(format!("none ({e})"));
            ui(UiEvent::Status(format!("Video decoder unavailable: {e}")));
            return;
        }
    };
    let describe = |d: &Decoder| d.path_name();
    let mut shown = describe(&dec);
    info.set(shown.clone());
    let mut waited = pref == DecoderPref::Software;
    while let Ok(first) = rx.recv() {
        if !waited {
            // The window's GPU says whether pictures may stay on it; deciding the decoder before that would
            // restart it a moment later. Waits at most 2 s, and not at all when software is forced.
            waited = true;
            let t = Instant::now();
            while sill_media::zerocopy::state() == sill_media::zerocopy::State::Unknown && t.elapsed().as_millis() < 2000 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        let mut batch = vec![first];
        while let Ok(m) = rx.try_recv() {
            batch.push(m);
        }
        let plan = plan_batch(batch, MAX_BACKLOG);
        let now = clock.elapsed().as_secs_f64();
        for _ in 0..plan.dropped {
            stats.lock().unwrap().record_dropped(now);
        }
        if plan.behind {
            if let Some(why) = dec.lose_picture("the decoder fell behind the stream") {
                let _ = notes.send(DecodeNote::Interrupted(why));
            }
        }
        for m in plan.msgs {
            match m {
                DecodeMsg::Params(ps) => dec.set_parameter_sets(&ps),
                DecodeMsg::Frame { payload, timestamp, keyframe } => {
                    let r = dec.decode(&payload, timestamp, keyframe);
                    let now = clock.elapsed().as_secs_f64();
                    if let Some(why) = r.interruption {
                        let _ = notes.send(DecodeNote::Interrupted(why));
                    }
                    if r.picture_back {
                        let _ = notes.send(DecodeNote::PictureBack);
                    }
                    if let Some(f) = r.frame {
                        stats.lock().unwrap().record_decoded(now, r.decode_ms);
                        if slot.put(f) {
                            stats.lock().unwrap().record_dropped(now);
                        } else {
                            ui(UiEvent::FrameReady);
                        }
                    }
                }
            }
        }
        let now_path = describe(&dec);
        if now_path != shown {
            eprintln!("decode path: {now_path}");
            shown = now_path.clone();
        }
        info.set(now_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(key: bool) -> DecodeMsg {
        DecodeMsg::Frame { payload: vec![], timestamp: 0.0, keyframe: key }
    }
    fn p() -> DecodeMsg {
        DecodeMsg::Params(ParameterSets { nal_unit_header_length: 4, sets: vec![] })
    }
    fn shape(m: &[DecodeMsg]) -> String {
        m.iter().map(|m| match m { DecodeMsg::Params(_) => 'P', DecodeMsg::Frame { keyframe: true, .. } => 'K', _ => 'd' }).collect()
    }

    #[test]
    fn a_keyframe_makes_what_came_before_moot() {
        let pl = plan_batch(vec![p(), f(true), f(false), f(false), p(), f(true), f(false)], 30);
        assert_eq!(shape(&pl.msgs), "PKd");
        assert_eq!((pl.dropped, pl.behind), (3, false));
    }

    #[test]
    fn small_backlogs_are_decoded_whole() {
        let pl = plan_batch(vec![f(false), f(false), f(false)], 30);
        assert_eq!((shape(&pl.msgs).as_str(), pl.dropped, pl.behind), ("ddd", 0, false));
    }

    #[test]
    fn a_long_backlog_without_a_keyframe_is_dropped_and_the_picture_lost() {
        let mut v = vec![p()];
        v.extend((0..31).map(|_| f(false)));
        let pl = plan_batch(v, 30);
        assert_eq!((shape(&pl.msgs).as_str(), pl.dropped, pl.behind), ("P", 31, true));
        let mut v = vec![];
        v.extend((0..30).map(|_| f(false)));
        assert!(!plan_batch(v, 30).behind);
    }

    #[test]
    fn slot_replaces_and_reports() {
        let s = FrameSlot::default();
        let fr = || VideoFrame { width: 2, height: 2, y: vec![0; 4], uv: vec![0; 2], timestamp: 0.0, color: Default::default(), prime: None };
        assert!(!s.put(fr()));
        assert!(s.put(fr()));
        assert!(s.take().is_some() && s.take().is_none());
    }
}
