use std::fmt;

/// Message kinds (StreamMessage.swift). Kinds this build does not know decode as [`Kind::Unknown`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    ParameterSets,
    Frame,
    WindowList,
    Thumbnail,
    AppIcon,
    AppList,
    SelectSource,
    LaunchApp,
    Input,
    Viewport,
    Ping,
    Pong,
    ClientStats,
    Tick,
    CursorShape,
    WindowCommand,
    HostSettings,
    ChangeSettings,
    MacInfo,
    PairRequest,
    PairResult,
    PairingWanted,
    Goodbye,
    Hello,
    MacMenu,
    PressMenuItem,
    MacPointer,
    FetchMenu,
    Gesture,
    Clipboard,
    Audio,
    KeyframeRequest,
    Unknown,
}

impl Kind {
    pub fn from_u8(raw: u8) -> Kind {
        use Kind::*;
        match raw {
            0 => ParameterSets,
            1 => Frame,
            2 => WindowList,
            3 => Thumbnail,
            4 => AppIcon,
            5 => AppList,
            6 => SelectSource,
            7 => LaunchApp,
            8 => Input,
            9 => Viewport,
            10 => Ping,
            11 => Pong,
            12 => ClientStats,
            13 => Tick,
            14 => CursorShape,
            15 => WindowCommand,
            16 => HostSettings,
            17 => ChangeSettings,
            18 => MacInfo,
            19 => PairRequest,
            20 => PairResult,
            21 => PairingWanted,
            22 => Goodbye,
            23 => Hello,
            24 => MacMenu,
            25 => PressMenuItem,
            26 => MacPointer,
            27 => FetchMenu,
            28 => Gesture,
            32 => Clipboard,
            43 => Audio,
            44 => KeyframeRequest,
            _ => Unknown,
        }
    }

    /// The wire number; `None` for [`Kind::Unknown`], which is never sent.
    pub fn as_u8(self) -> Option<u8> {
        use Kind::*;
        Some(match self {
            ParameterSets => 0,
            Frame => 1,
            WindowList => 2,
            Thumbnail => 3,
            AppIcon => 4,
            AppList => 5,
            SelectSource => 6,
            LaunchApp => 7,
            Input => 8,
            Viewport => 9,
            Ping => 10,
            Pong => 11,
            ClientStats => 12,
            Tick => 13,
            CursorShape => 14,
            WindowCommand => 15,
            HostSettings => 16,
            ChangeSettings => 17,
            MacInfo => 18,
            PairRequest => 19,
            PairResult => 20,
            PairingWanted => 21,
            Goodbye => 22,
            Hello => 23,
            MacMenu => 24,
            PressMenuItem => 25,
            MacPointer => 26,
            FetchMenu => 27,
            Gesture => 28,
            Clipboard => 32,
            Audio => 43,
            KeyframeRequest => 44,
            Unknown => return None,
        })
    }
}

pub const HEADER_LENGTH: usize = 14;
/// Any client → host message, on both doors.
pub const MAX_CLIENT_PAYLOAD: usize = 1 << 20;
/// The one message of a pairing connection (kind 19).
pub const MAX_PAIRING_PAYLOAD: usize = 4096;
/// A video frame, as a device accepts it.
pub const MAX_FRAME_PAYLOAD: usize = 32 << 20;
/// Any other host → device message.
pub const MAX_OTHER_HOST_PAYLOAD: usize = 4 << 20;

/// The 14 bytes: kind(1) timestamp(8, f64) is_keyframe(1) payload_length(4), big endian.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Header {
    pub kind: Kind,
    /// The kind byte as it came, for a log line about an unknown kind.
    pub raw_kind: u8,
    /// Host wall clock, seconds since 1970 (a client's pings carry its own monotonic clock instead).
    pub timestamp: f64,
    pub is_keyframe: bool,
    pub payload_length: usize,
}

impl Header {
    /// Parse the first 14 bytes; `None` when fewer are given.
    pub fn parse(data: &[u8]) -> Option<Header> {
        if data.len() < HEADER_LENGTH {
            return None;
        }
        let raw_kind = data[0];
        let timestamp = f64::from_bits(u64::from_be_bytes(data[1..9].try_into().ok()?));
        let is_keyframe = data[9] == 1;
        let payload_length = u32::from_be_bytes(data[10..14].try_into().ok()?) as usize;
        Some(Header { kind: Kind::from_u8(raw_kind), raw_kind, timestamp, is_keyframe, payload_length })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub kind: Kind,
    pub timestamp: f64,
    pub is_keyframe: bool,
    pub payload: Vec<u8>,
}

impl Message {
    pub fn new(kind: Kind, timestamp: f64, is_keyframe: bool, payload: Vec<u8>) -> Message {
        Message { kind, timestamp, is_keyframe, payload }
    }

    /// A client message (timestamp 0, not a keyframe).
    pub fn client(kind: Kind, payload: Vec<u8>) -> Message {
        Message::new(kind, 0.0, false, payload)
    }

    /// The bytes to write. Panics on [`Kind::Unknown`], which is never sent.
    pub fn serialize(&self) -> Vec<u8> {
        let kind = self.kind.as_u8().expect("Kind::Unknown is never sent");
        let mut out = Vec::with_capacity(HEADER_LENGTH + self.payload.len());
        out.push(kind);
        out.extend_from_slice(&self.timestamp.to_bits().to_be_bytes());
        out.push(self.is_keyframe as u8);
        out.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.payload);
        out
    }
}

/// Which way the bytes flow, which sets the cap a header may announce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// What a client reads: frames up to 32 MiB, anything else 4 MiB.
    HostToClient,
    /// What a host reads: 1 MiB (the pairing connection's 4096 is the session layer's).
    ClientToHost,
}

impl Direction {
    pub fn cap(self, kind: Kind) -> usize {
        match self {
            Direction::HostToClient if kind == Kind::Frame => MAX_FRAME_PAYLOAD,
            Direction::HostToClient => MAX_OTHER_HOST_PAYLOAD,
            Direction::ClientToHost => MAX_CLIENT_PAYLOAD,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// A header announced more than the cap: the reader closes, before waiting for the bytes.
    TooBig { kind: Kind, announced: usize, cap: usize },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::TooBig { kind, announced, cap } => {
                write!(f, "{kind:?} announced {announced} bytes, over the cap of {cap}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Incremental reader: push whatever bytes arrive, take whole messages out. Messages of a kind this
/// build does not know are returned (kind [`Kind::Unknown`]) so a caller can log them; skipping them is
/// the caller's rule. Sans-I/O.
#[derive(Debug)]
pub struct Decoder {
    direction: Direction,
    buf: Vec<u8>,
    failed: Option<DecodeError>,
}

impl Decoder {
    pub fn new(direction: Direction) -> Decoder {
        Decoder { direction, buf: Vec::new(), failed: None }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.failed.is_none() {
            self.buf.extend_from_slice(bytes);
        }
    }

    /// The next whole message, `Ok(None)` when more bytes are needed. After an error every call
    /// returns it again: the stream cannot be resynchronised, the connection must close.
    pub fn next_message(&mut self) -> Result<Option<Message>, DecodeError> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        let Some(h) = Header::parse(&self.buf) else { return Ok(None) };
        let cap = self.direction.cap(h.kind);
        if h.payload_length > cap {
            let e = DecodeError::TooBig { kind: h.kind, announced: h.payload_length, cap };
            self.failed = Some(e.clone());
            self.buf.clear();
            return Err(e);
        }
        let total = HEADER_LENGTH + h.payload_length;
        if self.buf.len() < total {
            return Ok(None);
        }
        let payload = self.buf[HEADER_LENGTH..total].to_vec();
        self.buf.drain(..total);
        Ok(Some(Message { kind: h.kind, timestamp: h.timestamp, is_keyframe: h.is_keyframe, payload }))
    }
}

/// HEVC VPS/SPS/PPS and the NAL length-prefix size (kind 0): nalLen(1) count(1), then per set
/// length(2, BE) and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterSets {
    pub nal_unit_header_length: u8,
    pub sets: Vec<Vec<u8>>,
}

impl ParameterSets {
    pub fn encode(&self) -> Vec<u8> {
        let mut d = vec![self.nal_unit_header_length, self.sets.len() as u8];
        for s in &self.sets {
            d.extend_from_slice(&(s.len() as u16).to_be_bytes());
            d.extend_from_slice(s);
        }
        d
    }

    pub fn decode(d: &[u8]) -> Option<ParameterSets> {
        if d.len() < 2 {
            return None;
        }
        let (nal, count) = (d[0], d[1] as usize);
        let mut offset = 2;
        let mut sets = Vec::with_capacity(count);
        for _ in 0..count {
            let len = u16::from_be_bytes(d.get(offset..offset + 2)?.try_into().ok()?) as usize;
            offset += 2;
            sets.push(d.get(offset..offset + len)?.to_vec());
            offset += len;
        }
        Some(ParameterSets { nal_unit_header_length: nal, sets })
    }
}
