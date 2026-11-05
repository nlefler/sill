//! Sill client media: the wire's HEVC framing as a decoder needs it, the decoder itself (VAAPI or
//! software through ffmpeg), the keyframe-recovery rule, viewport reporting, the Mac's cursor, and the
//! statistics the HUD and the stats report share.

pub mod clip;
pub mod cursor;
pub mod decoder;
pub mod nal;
pub mod recovery;
pub mod stats;
pub mod viewport;
pub mod zerocopy;

pub use decoder::{ColorInfo, DecodeResult, Decoder, DecoderKind, DecoderPref, Matrix, PrimeFrame, VideoFrame};
pub use recovery::{Decision, KeyframeRecovery};
pub use stats::{Snapshot, StatsCollector};
pub use viewport::{Panel, ViewportSender};
