//! Sync rig log sink (FRAME_EXACT_TRANSPORT_DESIGN P3, commit C0).
//!
//! Instrumentation only: records what the external clocks did, stamped where
//! they arrived, so the D7 noise models, the D19 delays and the D20 filter can be
//! checked against Peter's rig before P3 freezes its constants. Nothing reads
//! these records back into playback.
//!
//! Hot threads (content, MIDI callback, OSC receive, Metal completion) call
//! [`emit`]: one `try_send` into a bounded, preallocated channel. A full channel
//! drops the record and counts it; nothing blocks and nothing allocates. The app
//! owns the writer thread and the file (`manifold-app` `sync_rig_log`). Until
//! [`install`] runs, [`emit`] does nothing. The sink is write-only, like the
//! `log` facade: no thread reads state through it.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Instant;

use manifold_core::types::ClockAuthority;

/// Records the channel holds before it drops: about 2 minutes of the busiest
/// stream (MIDI clock at 300 BPM plus 60 fps ticks) if the writer stalls.
pub const CHANNEL_CAPACITY: usize = 16_384;

/// Who asked for a transport change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportSource {
    User,
    Link,
    MidiClock,
    Osc,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Play,
    Pause,
    Stop,
    Seek,
    SeekToBeat,
    /// Play aligned to the MIDI clock's beat (`content_commands.rs` Play).
    LocateToClock,
}

/// One fact, stamped in nanoseconds since the sink's epoch ([`now_ns`]).
/// `Copy` and fixed size so sending it never allocates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RigRecord {
    /// A MIDI message as midir delivered it: midir's own timestamp (µs) and
    /// the `Instant` taken in the callback.
    ClkByte {
        at_ns: u64,
        midir_us: u64,
        status: u8,
        d1: u8,
        d2: u8,
    },
    /// AbletonOSC `current_song_time`, stamped on the receive thread.
    OscSongTime { at_ns: u64, beats: f32 },
    /// AbletonOSC tempo listener, stamped on the receive thread.
    OscTempo { at_ns: u64, bpm: f32 },
    /// One content tick, after the engine tick. `link_*` are NaN without Link.
    Tick {
        frame: u64,
        wake_ns: u64,
        deadline_ns: u64,
        frames: u32,
        index: i64,
        beat: f64,
        authority: ClockAuthority,
        playing: bool,
        link_beat: f64,
        link_bpm: f64,
        link_peers: i32,
    },
    /// Every 5 s on the content thread: each source's own view of the beat.
    /// Values a source can't give are NaN.
    Phase {
        at_ns: u64,
        engine_beat: f64,
        link_beat: f64,
        link_offset: f64,
        clk_beat: f32,
        clk_bpm: f32,
        osc_beat: f32,
        osc_age_s: f64,
        osc_bpm: f32,
    },
    /// The output window's drawable for content frame `frame` was presented.
    PresentIssued {
        frame: u64,
        drawable_id: u64,
        at_ns: u64,
    },
    /// Metal's presented handler: `presented_s` is host time in seconds
    /// (`mach_absolute_time` base), 0 when the drawable was never shown.
    Presented {
        drawable_id: u64,
        presented_s: f64,
        at_ns: u64,
    },
    Authority {
        at_ns: u64,
        from: ClockAuthority,
        to: ClockAuthority,
    },
    /// A transport change that passed the arbiter's gate, or a user command.
    Transport {
        at_ns: u64,
        source: TransportSource,
        kind: TransportKind,
        value: f64,
    },
}

/// One sender plus its drop counter. The global sink is one of these; tests
/// build their own.
pub struct RigSink {
    tx: SyncSender<RigRecord>,
    dropped: AtomicU64,
}

impl RigSink {
    pub fn new(capacity: usize) -> (Self, Receiver<RigRecord>) {
        let (tx, rx) = sync_channel(capacity);
        (
            Self {
                tx,
                dropped: AtomicU64::new(0),
            },
            rx,
        )
    }

    /// Never blocks: a full or closed channel drops the record and counts it.
    #[inline]
    pub fn emit(&self, record: RigRecord) {
        match self.tx.try_send(record) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

static SINK: OnceLock<RigSink> = OnceLock::new();
static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Install the global sink. Returns the receiver for the writer thread, or
/// `None` if a sink is already installed.
pub fn install(capacity: usize) -> Option<Receiver<RigRecord>> {
    epoch();
    let (sink, rx) = RigSink::new(capacity);
    SINK.set(sink).ok().map(|()| rx)
}

/// Send to the global sink; a no-op until [`install`].
#[inline]
pub fn emit(record: RigRecord) {
    if let Some(sink) = SINK.get() {
        sink.emit(record);
    }
}

/// True once a sink is installed, so callers can skip gathering values.
#[inline]
pub fn enabled() -> bool {
    SINK.get().is_some()
}

/// Records dropped by the global sink so far.
pub fn dropped() -> u64 {
    SINK.get().map_or(0, RigSink::dropped)
}

/// The instant every `*_ns` stamp counts from.
pub fn epoch() -> Instant {
    *EPOCH.get_or_init(Instant::now)
}

#[inline]
pub fn ns_since_epoch(at: Instant) -> u64 {
    at.saturating_duration_since(epoch()).as_nanos() as u64
}

#[inline]
pub fn now_ns() -> u64 {
    ns_since_epoch(Instant::now())
}

fn authority_name(a: ClockAuthority) -> &'static str {
    match a {
        ClockAuthority::Internal => "internal",
        ClockAuthority::Link => "link",
        ClockAuthority::MidiClock => "clk",
        ClockAuthority::Osc => "osc",
        ClockAuthority::Audio => "audio",
    }
}

fn source_name(s: TransportSource) -> &'static str {
    match s {
        TransportSource::User => "user",
        TransportSource::Link => "link",
        TransportSource::MidiClock => "clk",
        TransportSource::Osc => "osc",
        TransportSource::Audio => "audio",
    }
}

fn kind_name(k: TransportKind) -> &'static str {
    match k {
        TransportKind::Play => "play",
        TransportKind::Pause => "pause",
        TransportKind::Stop => "stop",
        TransportKind::Seek => "seek",
        TransportKind::SeekToBeat => "seek_beat",
        TransportKind::LocateToClock => "locate_clk",
    }
}

impl TransportSource {
    pub fn from_authority(a: ClockAuthority) -> Self {
        match a {
            ClockAuthority::Internal => Self::User,
            ClockAuthority::Link => Self::Link,
            ClockAuthority::MidiClock => Self::MidiClock,
            ClockAuthority::Osc => Self::Osc,
            ClockAuthority::Audio => Self::Audio,
        }
    }
}

/// One line per record, `kind key=value ...`, appended to `out` (which the
/// writer reuses). `scripts/sync_rig_report.py` parses exactly this. Floats use
/// `{:?}` so they round-trip bit for bit and NaN stays `NaN`.
pub fn format_line(record: &RigRecord, out: &mut String) {
    use std::fmt::Write;
    let _ = match *record {
        RigRecord::ClkByte {
            at_ns,
            midir_us,
            status,
            d1,
            d2,
        } => writeln!(
            out,
            "clk t={at_ns} midir_us={midir_us} st={status:02x} d1={d1} d2={d2}"
        ),
        RigRecord::OscSongTime { at_ns, beats } => {
            writeln!(out, "osc_song t={at_ns} beats={beats:?}")
        }
        RigRecord::OscTempo { at_ns, bpm } => writeln!(out, "osc_tempo t={at_ns} bpm={bpm:?}"),
        RigRecord::Tick {
            frame,
            wake_ns,
            deadline_ns,
            frames,
            index,
            beat,
            authority,
            playing,
            link_beat,
            link_bpm,
            link_peers,
        } => writeln!(
            out,
            "tick f={frame} wake={wake_ns} dl={deadline_ns} n={frames} i={index} b={beat:?} a={} p={} lb={link_beat:?} lt={link_bpm:?} lp={link_peers}",
            authority_name(authority),
            u8::from(playing)
        ),
        RigRecord::Phase {
            at_ns,
            engine_beat,
            link_beat,
            link_offset,
            clk_beat,
            clk_bpm,
            osc_beat,
            osc_age_s,
            osc_bpm,
        } => writeln!(
            out,
            "phase t={at_ns} b={engine_beat:?} lb={link_beat:?} lo={link_offset:?} cb={clk_beat:?} ct={clk_bpm:?} ob={osc_beat:?} oage={osc_age_s:?} ot={osc_bpm:?}"
        ),
        RigRecord::PresentIssued {
            frame,
            drawable_id,
            at_ns,
        } => {
            writeln!(out, "present f={frame} d={drawable_id} t={at_ns}")
        }
        RigRecord::Presented {
            drawable_id,
            presented_s,
            at_ns,
        } => {
            writeln!(
                out,
                "presented d={drawable_id} ps={presented_s:?} t={at_ns}"
            )
        }
        RigRecord::Authority { at_ns, from, to } => writeln!(
            out,
            "authority t={at_ns} from={} to={}",
            authority_name(from),
            authority_name(to)
        ),
        RigRecord::Transport {
            at_ns,
            source,
            kind,
            value,
        } => writeln!(
            out,
            "transport t={at_ns} src={} kind={} v={value:?}",
            source_name(source),
            kind_name(kind)
        ),
    };
}

#[cfg(test)]
mod tests;
