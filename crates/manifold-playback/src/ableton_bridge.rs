//! Ableton Live OSC bridge.
//!
//! Discovers Ableton's session via AbletonOSC (port 11000/11001), subscribes
//! to rack macro parameter changes, and writes incoming values to MANIFOLD
//! parameters in replace mode.
//!
//! Design philosophy: MANIFOLD is the active party — it reaches into Ableton,
//! reads the session, and pulls what it needs. Ableton stays untouched.

use ahash::{AHashMap, AHashSet};
use manifold_core::ableton_mapping::{
    AbletonMappingStatus, AbletonMappingTarget, AbletonParamMapping, AbletonSetContext,
    AbletonTrackSignature,
};
use manifold_core::project::Project;
use parking_lot::Mutex;
use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ── Constants ────────���────────────────────────────────────────────

const ABLETON_SEND_PORT: u16 = 11000;
const ABLETON_RECV_PORT: u16 = 11001;
const HEARTBEAT_INTERVAL_SECS: f64 = 0.75;
const CONNECTION_TIMEOUT_SECS: f64 = 1.5;
/// Timeout per discovery step before falling back to idle.
const DISCOVERY_STEP_TIMEOUT_SECS: f64 = 3.0;

/// Ableton rack device class names that contain macros.
const RACK_CLASS_NAMES: &[&str] = &[
    "InstrumentGroupDevice",
    "DrumGroupDevice",
    "AudioEffectGroupDevice",
    "MidiEffectGroupDevice",
];

/// Number of macro parameters on a rack device (always 0-7).
const RACK_MACRO_COUNT: usize = 8;

/// Timeout for considering transport listeners as "receiving" from Ableton.
const TRANSPORT_TIMEOUT_SECS: f64 = 2.0;
// Transport echo windows, redundant-send counts, confirmation windows, and
// the deferred play-seek all moved into the closed-loop state machine
// (`transport_sync.rs`, docs/ABLETON_TRANSPORT_SYNC_DESIGN.md D3/D7/D11) —
// commands are now acknowledged by value-matched observations, not guessed
// at by wall-clock windows.

// ── Session model (runtime only) ──────────────────────────────────

/// Discovered Ableton Live session state.
#[derive(Debug, Clone, Default)]
pub struct AbletonSession {
    pub connected: bool,
    pub tracks: Vec<AbletonTrack>,
    /// Locators / cue points fetched from Ableton, sorted by `time` ascending.
    /// `time` is in beats (absolute song position). Refreshed on connect /
    /// re-discovery. Used by performance-mode HUD to compute current section
    /// and countdown to next cue.
    pub cue_points: Vec<CuePoint>,
    /// Leaf tracks belonging to a hard-coded "PLAY" group, with their
    /// arrangement clip layouts. Populated after discovery if the project
    /// contains a top-level group track named "PLAY". The perform-mode HUD
    /// derives "currently playing" from these locally each frame.
    pub play_group: Option<GroupTracks>,
}

/// Tracks belonging to a named Ableton group, in display order.
#[derive(Debug, Clone)]
pub struct GroupTracks {
    pub name: String,
    pub tracks: Vec<TrackArrangement>,
}

/// One leaf track inside a group, with its static arrangement clip layout.
///
/// Both track-level and clip-level mute are honored when computing playback
/// state. Clip ranges are absolute beats from the start of the song.
///
/// Requires the AbletonOSC `arrangement_clips/end_time` and
/// `arrangement_clips/muted` endpoints — see
/// `assets/abletonosc-patches/README.md`.
#[derive(Debug, Clone)]
pub struct TrackArrangement {
    pub track_id: i32,
    pub name: String,
    pub muted: bool,
    pub clips: Vec<ArrangementClip>,
}

/// One clip inside a `TrackArrangement`.
///
/// `start` and `end` are absolute beat positions (Ableton's `clip.start_time`
/// and `clip.end_time`). `end` is the *visible* arrangement footprint of the
/// clip — for looped clips this is greater than `clip.length`.
#[derive(Debug, Clone, PartialEq)]
pub struct ArrangementClip {
    pub start: f64,
    pub end: f64,
    pub muted: bool,
}

/// An Ableton Live locator (cue point). Times are absolute beats from the
/// start of the song.
#[derive(Debug, Clone)]
pub struct CuePoint {
    /// Absolute beat position in the Ableton song.
    pub time: f64,
    /// User-set locator name (e.g. "Drop", "Verse 2").
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct AbletonTrack {
    pub track_id: i32,
    pub name: String,
    pub devices: Vec<AbletonDevice>,
}

#[derive(Debug, Clone)]
pub struct AbletonDevice {
    pub device_id: i32,
    pub name: String,
    pub class_name: String,
    pub macros: Vec<AbletonMacro>,
}

#[derive(Debug, Clone)]
pub struct AbletonMacro {
    pub param_id: i32,
    pub name: String,
    pub value: f32,
    pub min: f32,
    pub max: f32,
}

fn is_rack_device(class_name: &str) -> bool {
    RACK_CLASS_NAMES.contains(&class_name)
}

// ── Pending values from Ableton ───────────────────────────────────
// Keyed by (track_id, device_id, param_id) → latest raw value.
// Only the latest value per parameter is kept (backpressure).
type PendingValueMap = AHashMap<(i32, i32, i32), f32>;

// ── 1€ filter ─────────────────────────────────────────────────────
// Adaptive low-pass: heavy smoothing when the signal is slow (kills the
// ~10 Hz stair-stepping from Ableton automation listeners), near pass-through
// when the signal is fast (knob flicks stay snappy).
//
// Reference: Casiez et al., "1€ Filter: A Simple Speed-based Low-pass
// Filter for Noisy Input in Interactive Systems" (CHI 2012).
#[derive(Debug, Clone)]
struct OneEuroFilter {
    /// Cutoff at zero velocity (Hz). Lower → smoother slow signals, more lag.
    min_cutoff: f32,
    /// Speed coefficient. Higher → faster response when input is moving fast.
    beta: f32,
    /// Cutoff for the derivative low-pass (Hz).
    d_cutoff: f32,
    x_prev: f32,
    dx_prev: f32,
    last_time: f64,
    initialized: bool,
}

impl OneEuroFilter {
    fn new() -> Self {
        // Tuned for AbletonOSC: ~10 Hz automation cadence, 60 Hz knob input.
        // min_cutoff=1.5 → slow signals smoothed with ~100 ms lag.
        // beta=0.05 → fast signals (high derivative) pass through near-instantly.
        Self {
            min_cutoff: 1.5,
            beta: 0.05,
            d_cutoff: 1.0,
            x_prev: 0.0,
            dx_prev: 0.0,
            last_time: 0.0,
            initialized: false,
        }
    }

    fn alpha(cutoff: f32, dt: f32) -> f32 {
        let tau = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
        1.0 / (1.0 + tau / dt)
    }

    fn filter(&mut self, x: f32, now: f64) -> f32 {
        if !self.initialized {
            self.initialized = true;
            self.x_prev = x;
            self.dx_prev = 0.0;
            self.last_time = now;
            return x;
        }
        let dt = (now - self.last_time) as f32;
        self.last_time = now;
        if dt <= 0.0 {
            return self.x_prev;
        }
        let dx = (x - self.x_prev) / dt;
        let a_d = Self::alpha(self.d_cutoff, dt);
        let dx_hat = a_d * dx + (1.0 - a_d) * self.dx_prev;
        let cutoff = self.min_cutoff + self.beta * dx_hat.abs();
        let a = Self::alpha(cutoff, dt);
        let x_hat = a * x + (1.0 - a) * self.x_prev;
        self.x_prev = x_hat;
        self.dx_prev = dx_hat;
        x_hat
    }
}

/// Per-source filter state. We filter the raw Ableton value once per
/// (track, device, param), then fan it out to every WriteTarget.
#[derive(Debug, Clone)]
struct FilteredSource {
    filter: OneEuroFilter,
    /// Latest raw value received from Ableton (the filter target).
    target_raw: f32,
    /// Last filtered output we wrote to the project — used to decide whether
    /// the filter has settled and we can stop ticking this source.
    last_output: f32,
    /// Wall-clock time of the most recent inbound value. We keep ticking for
    /// a short window after the last update so the filter has time to settle.
    last_update: f64,
}

/// How long after the last inbound value we keep ticking the filter.
/// 0.5 s is comfortably longer than the filter's settling time at min_cutoff=1.5.
const FILTER_SETTLE_WINDOW_SECS: f64 = 0.5;
/// Output delta below this is considered "settled" — we stop writing.
const FILTER_SETTLE_EPSILON: f32 = 1.0e-5;

// ── Pending transport state from Ableton ──────────────────────────

/// Transport values received from AbletonOSC listener updates.
/// Written by the background receiver thread, drained on the content thread.
#[derive(Default)]
struct PendingTransportState {
    is_playing: Option<bool>,
    tempo: Option<f32>,
    /// Song position in beats from the `current_song_time` listener —
    /// the position-acknowledgment channel (design D4).
    song_time: Option<f32>,
}

// ── Write target (pre-built lookup for hot path) ──────────────────

#[derive(Debug, Clone)]
struct WriteTarget {
    target: AbletonMappingTarget,
    /// MANIFOLD param id (P4). Resolved from the mapping and written through the
    /// id funnel — replaces the P2 positional `param_index` that could not
    /// address user-added / registry-absent params.
    param_id: String,
    /// Ableton parameter range — the raw value arrives in [ableton_min, ableton_max]
    /// and must be normalized to 0-1 before applying range_min/range_max trim.
    ableton_min: f32,
    ableton_max: f32,
    /// User trim handles (0-1 normalized, applied after Ableton normalization).
    range_min: f32,
    range_max: f32,
    /// When true, the normalized value is inverted (1.0 - v) before trim mapping.
    inverted: bool,
    /// MANIFOLD parameter range — final 0-1 maps into [param_min, param_max].
    param_min: f32,
    param_max: f32,
}

// ── OSC message types ─────────────────────────────────────────────

/// A richer OSC message that preserves string arguments (needed for discovery).
#[derive(Debug, Clone)]
struct OscMessage {
    address: String,
    args: Vec<rosc::OscType>,
}

/// Thread-safe message queue for the Ableton receiver.
#[derive(Default)]
struct AbletonMessageQueue {
    messages: Vec<OscMessage>,
}

// ── Discovery state machine ─────────���─────────────────────────────

#[derive(Debug, Clone)]
enum DiscoveryState {
    /// Not discovering.
    Idle,
    /// Sent /live/song/get/num_tracks, waiting for response.
    WaitingTrackCount { started: f64 },
    /// Querying track names one by one.
    QueryingTracks {
        expected_count: i32,
        tracks: Vec<(i32, String)>,
        next_track: i32,
        started: f64,
    },
    /// Querying devices for each track.
    QueryingDevices {
        tracks: Vec<AbletonTrack>,
        pending_tracks: Vec<(i32, String)>,
        started: f64,
    },
    /// Querying macro parameters for rack devices.
    QueryingParams {
        tracks: Vec<AbletonTrack>,
        /// (track_id, device_id, device_name, class_name) for remaining racks.
        pending_racks: Vec<(i32, i32, String, String)>,
        started: f64,
    },
    /// Discovery complete.
    Complete,
}

// ── Ableton Bridge ──────────────���─────────────────────────────────

/// Bridge between MANIFOLD and Ableton Live via AbletonOSC.
pub struct AbletonBridge {
    // ── Networking
    send_socket: Option<UdpSocket>,
    recv_socket: Option<UdpSocket>,
    recv_thread: Option<std::thread::JoinHandle<()>>,
    shutdown_flag: Arc<AtomicBool>,
    /// Set by the recv thread if it panics inside catch_unwind.
    /// Checked each frame in `update()` to trigger automatic reconnection.
    recv_thread_panicked: Arc<AtomicBool>,
    message_queue: Arc<Mutex<AbletonMessageQueue>>,

    // ── Session state
    session: AbletonSession,
    session_version: u64,
    prev_session_version: u64,

    // ── Discovery
    discovery_state: DiscoveryState,

    // ── Connection
    connected: bool,
    last_heartbeat: f64,
    last_response: f64,

    // ── Parameter values from Ableton (content thread drains these)
    /// Keyed by (track_id, device_id, param_id) → latest raw value.
    /// Only the most recent value per parameter is kept (backpressure).
    pending_values: Arc<Mutex<PendingValueMap>>,

    // ── Pre-built fast lookup: (track_id, device_id, param_id) → write targets
    write_targets: AHashMap<(i32, i32, i32), Vec<WriteTarget>>,

    // ── 1€ filter state per (track_id, device_id, param_id).
    // Smooths Ableton's ~10 Hz automation cadence to 60 Hz without adding
    // perceptible latency to fast knob moves.
    filtered_sources: AHashMap<(i32, i32, i32), FilteredSource>,

    // ── Active listener subscriptions
    active_listeners: AHashSet<(i32, i32, i32)>,

    // ── Dirty flags for content thread
    /// Set when discovery completes so the content thread knows to call
    /// `validate_mappings` + `rebuild_listeners` and force a project snapshot.
    validation_dirty: bool,

    // ── Transport sync ───────────────────────────────────────────
    /// Pending transport state from receiver thread.
    pending_transport: Arc<Mutex<PendingTransportState>>,
    /// Whether transport listeners are subscribed.
    transport_enabled: bool,
    /// Last known is_playing state from Ableton (UI/HUD reads).
    ableton_is_playing: bool,
    /// Latest tempo from Ableton (UI/HUD reads).
    ableton_tempo: f32,
    /// Wall-clock time of last transport message from Ableton.
    transport_last_received: f64,
    /// Outbound: last known MANIFOLD play state (edge detection feeding
    /// the state machine's local-gesture inputs).
    transport_last_was_playing: bool,
    /// Closed-loop transport state machine (design §4). All echo/retry/
    /// confirmation logic lives here; the bridge just pumps I/O.
    transport_sync: crate::transport_sync::AbletonTransportSync,

    /// PLAY-group discovery accumulator. Populated after main discovery
    /// completes; the resulting `GroupTracks` is published into
    /// `session.play_group` once both phases are complete.
    play_group_discovery: PlayGroupDiscovery,
}

/// Hard-coded name of the Ableton group track we surface in the perform
/// HUD. The user can rename their group in Ableton to opt in/out.
const PLAY_GROUP_NAME: &str = "PLAY";

/// Two-phase accumulator for fetching the PLAY group's leaf tracks and
/// their arrangement clip layouts.
///
/// Phase 1: query `is_foldable` + `is_grouped` for every track in the
/// session. Once both maps are full, derive PLAY's leaf descendants by
/// walking the flat track list.
///
/// Phase 2: query `mute` and `arrangement_clips/{name,length,start_time}`
/// for each leaf. Once all four maps are complete for every leaf, build
/// the final `GroupTracks` and publish it into the session.
#[derive(Default)]
struct PlayGroupDiscovery {
    /// True while we're actively fetching. Cleared on completion or reset.
    in_progress: bool,
    /// Total tracks expected in the session (from `session.tracks.len()`
    /// at the moment we kick off).
    track_count: i32,
    // ── Phase 1: structure ────────────────────────────────────────
    foldable: AHashMap<i32, bool>,
    grouped: AHashMap<i32, bool>,
    structure_complete: bool,
    // ── Phase 2: leaf details ─────────────────────────────────────
    /// Leaf track indices (in display order) computed at end of phase 1.
    leaf_indices: Vec<i32>,
    /// Leaf names — copied from `session.tracks` (already known from main discovery).
    leaf_names: AHashMap<i32, String>,
    mutes: AHashMap<i32, bool>,
    clip_names: AHashMap<i32, Vec<String>>,
    clip_starts: AHashMap<i32, Vec<f64>>,
    clip_ends: AHashMap<i32, Vec<f64>>,
    clip_muted: AHashMap<i32, Vec<bool>>,
}

impl AbletonBridge {
    pub fn new() -> Self {
        Self {
            send_socket: None,
            recv_socket: None,
            recv_thread: None,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            recv_thread_panicked: Arc::new(AtomicBool::new(false)),
            message_queue: Arc::new(Mutex::new(AbletonMessageQueue::default())),
            session: AbletonSession::default(),
            session_version: 0,
            prev_session_version: 0,
            discovery_state: DiscoveryState::Idle,
            connected: false,
            last_heartbeat: 0.0,
            last_response: 0.0,
            pending_values: Arc::new(Mutex::new(AHashMap::new())),
            write_targets: AHashMap::new(),
            filtered_sources: AHashMap::new(),
            active_listeners: AHashSet::new(),
            validation_dirty: false,
            pending_transport: Arc::new(Mutex::new(PendingTransportState::default())),
            transport_enabled: false,
            ableton_is_playing: false,
            ableton_tempo: 120.0,
            transport_last_received: 0.0,
            transport_last_was_playing: false,
            transport_sync: crate::transport_sync::AbletonTransportSync::new(),
            play_group_discovery: PlayGroupDiscovery::default(),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn has_send_socket(&self) -> bool {
        self.send_socket.is_some()
    }

    pub fn session(&self) -> &AbletonSession {
        &self.session
    }

    /// Returns true if session version changed since last call.
    pub fn session_changed(&mut self) -> bool {
        if self.session_version != self.prev_session_version {
            self.prev_session_version = self.session_version;
            true
        } else {
            false
        }
    }

    // ── Lifecycle ─────────────────────────────────────────────────

    /// Start the bridge: bind sockets, start receiver thread.
    pub fn connect(&mut self) {
        if self.send_socket.is_some() {
            return;
        }

        // Bind send socket to ephemeral port, connect to Ableton
        match UdpSocket::bind("0.0.0.0:0") {
            Ok(sock) => {
                if let Err(e) = sock.connect(format!("127.0.0.1:{ABLETON_SEND_PORT}")) {
                    log::error!("[AbletonBridge] Failed to connect send socket: {e}");
                    return;
                }
                self.send_socket = Some(sock);
            }
            Err(e) => {
                log::error!("[AbletonBridge] Failed to bind send socket: {e}");
                return;
            }
        }

        // Bind receive socket on port 11001
        let recv_addr = format!("0.0.0.0:{ABLETON_RECV_PORT}");
        match UdpSocket::bind(&recv_addr) {
            Ok(sock) => {
                if let Err(e) = sock.set_read_timeout(Some(std::time::Duration::from_millis(100))) {
                    log::error!("[AbletonBridge] Failed to set recv timeout: {e}");
                    return;
                }
                let queue = Arc::clone(&self.message_queue);
                let pending_values = Arc::clone(&self.pending_values);
                let shutdown = Arc::clone(&self.shutdown_flag);
                let panicked = Arc::clone(&self.recv_thread_panicked);
                self.shutdown_flag.store(false, Ordering::Relaxed);
                self.recv_thread_panicked.store(false, Ordering::Relaxed);

                let handle = std::thread::spawn(move || {
                    let mut buf = [0u8; 65536];
                    loop {
                        if shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        let size = match sock.recv_from(&mut buf) {
                            Ok((sz, _)) => sz,
                            Err(ref e)
                                if e.kind() == std::io::ErrorKind::WouldBlock
                                    || e.kind() == std::io::ErrorKind::TimedOut =>
                            {
                                continue;
                            }
                            Err(e) => {
                                log::error!("[AbletonBridge] recv error: {e}");
                                continue;
                            }
                        };

                        // Wrap message processing in catch_unwind so a
                        // malformed packet can't kill the receiver thread.
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            // Fast path: parse parameter value updates
                            // directly from raw bytes — zero allocation,
                            // bypasses rosc + message queue entirely.
                            if let Some((tid, did, pid, val)) =
                                try_parse_param_value_fast(&buf[..size])
                            {
                                pending_values.lock().insert((tid, did, pid), val);
                                return;
                            }
                            // Slow path: full rosc decode for discovery,
                            // transport, and any other message types.
                            match rosc::decoder::decode_udp(&buf[..size]) {
                                Ok((_, packet)) => {
                                    Self::handle_packet_static(packet, &queue);
                                }
                                Err(e) => {
                                    log::error!(
                                        "[AbletonBridge] OSC decode \
                                             error: {e}"
                                    );
                                }
                            }
                        }));
                        if let Err(e) = result {
                            let msg = if let Some(s) = e.downcast_ref::<&str>() {
                                (*s).to_string()
                            } else if let Some(s) = e.downcast_ref::<String>() {
                                s.clone()
                            } else {
                                "unknown panic".to_string()
                            };
                            log::error!(
                                "[AbletonBridge] recv thread caught panic: \
                                 {msg} — signalling for reconnect"
                            );
                            panicked.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                });
                self.recv_thread = Some(handle);
            }
            Err(e) => {
                log::error!("[AbletonBridge] Failed to bind recv socket on {recv_addr}: {e}");
                self.send_socket = None;
                return;
            }
        }

        log::info!("[AbletonBridge] Connected — send:{ABLETON_SEND_PORT} recv:{ABLETON_RECV_PORT}");
    }

    /// Stop the bridge: shutdown receiver, clear state.
    pub fn disconnect(&mut self) {
        self.shutdown_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.recv_thread.take() {
            let _ = handle.join();
        }
        self.send_socket = None;
        self.recv_socket = None;
        self.connected = false;
        self.session = AbletonSession::default();
        self.session_version += 1;
        self.discovery_state = DiscoveryState::Idle;
        self.active_listeners.clear();
        self.write_targets.clear();
        self.filtered_sources.clear();
        self.message_queue.lock().messages.clear();
        self.pending_values.lock().clear();
        self.play_group_discovery = PlayGroupDiscovery::default();

        // Clear transport state
        self.transport_enabled = false;
        self.ableton_is_playing = false;
        self.transport_last_received = 0.0;
        self.transport_sync.reset();
        *self.pending_transport.lock() = PendingTransportState::default();
        log::info!("[AbletonBridge] Disconnected");
    }

    fn handle_packet_static(packet: rosc::OscPacket, queue: &Arc<Mutex<AbletonMessageQueue>>) {
        match packet {
            rosc::OscPacket::Message(msg) => {
                queue.lock().messages.push(OscMessage {
                    address: msg.addr,
                    args: msg.args,
                });
            }
            rosc::OscPacket::Bundle(bundle) => {
                for inner in bundle.content {
                    Self::handle_packet_static(inner, queue);
                }
            }
        }
    }

    // ── Per-frame update ──────────────��───────────────────────────

    /// Call once per content frame. Drains receiver, advances discovery,
    /// checks heartbeat/connection status.
    pub fn update(&mut self, realtime: f64) {
        if self.send_socket.is_none() {
            return;
        }

        // If the recv thread panicked, tear down and reconnect automatically.
        if self.recv_thread_panicked.load(Ordering::Relaxed) {
            log::warn!("[AbletonBridge] Recv thread died — reconnecting automatically");
            self.disconnect();
            self.connect();
            return;
        }

        // Drain received messages — take ownership to avoid per-message clone.
        let messages = {
            let mut q = self.message_queue.lock();
            std::mem::take(&mut q.messages)
        };

        // Process messages — we own the Vec, no clone needed.
        for msg in &messages {
            self.handle_message(msg, realtime);
        }

        // Heartbeat
        if realtime - self.last_heartbeat >= HEARTBEAT_INTERVAL_SECS {
            self.send_osc("/live/song/get/num_tracks", &[]);
            self.last_heartbeat = realtime;
        }

        // Connection timeout
        if self.connected && realtime - self.last_response > CONNECTION_TIMEOUT_SECS {
            log::info!("[AbletonBridge] Connection timeout — marking disconnected");
            self.connected = false;
            self.session.connected = false;
            self.session_version += 1;
        }

        // Discovery timeout
        self.check_discovery_timeout(realtime);

        // Drain pending transport state from receiver thread
        self.drain_transport(realtime);
    }

    /// Apply pending Ableton values to project parameters.
    ///
    /// Drains inbound values into per-source 1€ filters, then ticks every
    /// active filter at the content-thread rate (~60 Hz). This smooths the
    /// ~10 Hz cadence of AbletonOSC automation listeners into a continuous
    /// 60 Hz signal without adding perceptible latency to fast knob moves
    /// (the filter's adaptive cutoff lets fast input through near-instantly).
    ///
    /// Returns `true` if any values were written. Since the always-on
    /// ModulationSnapshot send (param-feed unification), the content thread
    /// no longer consults this — the snapshot ships every tick regardless.
    /// Kept for tests and future callers that want the dirty signal.
    pub fn apply(&mut self, project: &mut Project, now: f64) -> bool {
        // 1. Drain inbound raw values into the per-source filter targets.
        {
            let mut pending = self.pending_values.lock();
            for (key, raw_value) in pending.drain() {
                // Only track sources we actually have mappings for.
                if !self.write_targets.contains_key(&key) {
                    continue;
                }
                let entry = self
                    .filtered_sources
                    .entry(key)
                    .or_insert_with(|| FilteredSource {
                        filter: OneEuroFilter::new(),
                        target_raw: raw_value,
                        last_output: f32::NAN,
                        last_update: now,
                    });
                entry.target_raw = raw_value;
                entry.last_update = now;
            }
        }

        if self.filtered_sources.is_empty() {
            return false;
        }

        // 2. Tick every active filter and write the smoothed value out.
        let mut wrote_any = false;
        let mut to_remove: Vec<(i32, i32, i32)> = Vec::new();

        for (key, src) in self.filtered_sources.iter_mut() {
            let smoothed = src.filter.filter(src.target_raw, now);
            let delta = (smoothed - src.last_output).abs();
            let settled = delta < FILTER_SETTLE_EPSILON
                && (src.target_raw - smoothed).abs() < FILTER_SETTLE_EPSILON;
            let stale = now - src.last_update > FILTER_SETTLE_WINDOW_SECS;

            // Stop ticking once the filter has settled AND no new input has
            // arrived for the settle window — keeps the map bounded.
            if settled && stale {
                to_remove.push(*key);
                continue;
            }

            // Skip the project write if the output hasn't moved meaningfully
            // (avoids redundant set_base_param calls + UI dirty flags).
            if !src.last_output.is_nan() && delta < FILTER_SETTLE_EPSILON {
                continue;
            }
            src.last_output = smoothed;

            if let Some(targets) = self.write_targets.get(key) {
                for wt in targets {
                    // Normalize Ableton raw value into 0-1.
                    let span = wt.ableton_max - wt.ableton_min;
                    let mut normalized = if span > f32::EPSILON {
                        ((smoothed - wt.ableton_min) / span).clamp(0.0, 1.0)
                    } else {
                        smoothed.clamp(0.0, 1.0)
                    };
                    // Apply inversion before trim range mapping.
                    if wt.inverted {
                        normalized = 1.0 - normalized;
                    }
                    // Apply user trim handles.
                    let mapped = wt.range_min + (wt.range_max - wt.range_min) * normalized;
                    // Map into MANIFOLD parameter range.
                    let value = wt.param_min + (wt.param_max - wt.param_min) * mapped;
                    Self::write_to_project(project, &wt.target, &wt.param_id, value);
                }
                wrote_any = true;
            }
        }

        for key in &to_remove {
            self.filtered_sources.remove(key);
        }

        // Mirror current filter outputs into the session macro values so
        // the perform-mode HUD (and any other consumer that reads
        // `session.tracks[*].devices[*].macros[*].value`) sees fresh
        // data without subscribing to OSC directly. We mirror the
        // *smoothed* value rather than the raw inbound value so the
        // displayed bars track what's actually being applied to MANIFOLD
        // parameters, not the noisy underlying signal.
        //
        // Only bump session_version when at least one macro value moved
        // by a perceptible amount (~1/256 of full range) — bumping every
        // frame would force a per-frame Arc clone of the entire session
        // (28 tracks + devices) onto the UI channel. By gating the bump
        // we keep the cost paid only when the user is actually moving a
        // controller.
        const MACRO_MIRROR_EPSILON: f32 = 1.0 / 256.0;
        if !self.filtered_sources.is_empty() {
            let mut any_moved = false;
            for ((tid, did, pid), src) in self.filtered_sources.iter() {
                if src.last_output.is_nan() {
                    continue;
                }
                if let Some(track) = self.session.tracks.iter_mut().find(|t| t.track_id == *tid)
                    && let Some(device) = track.devices.iter_mut().find(|d| d.device_id == *did)
                    && let Some(macro_) = device.macros.iter_mut().find(|m| m.param_id == *pid)
                {
                    let span = (macro_.max - macro_.min).abs().max(f32::EPSILON);
                    let normalized_delta = (macro_.value - src.last_output).abs() / span;
                    if normalized_delta > MACRO_MIRROR_EPSILON {
                        macro_.value = src.last_output;
                        any_moved = true;
                    }
                }
            }
            if any_moved {
                self.session_version += 1;
            }
        }

        wrote_any
    }

    fn write_to_project(
        project: &mut Project,
        target: &AbletonMappingTarget,
        param_id: &str,
        value: f32,
    ) {
        // MacroSlot routes to the macro bank; the three host variants all
        // locate their PresetInstance through the shared dispatch and write by
        // param id (P4). If the id is absent on the instance, the funnel is a
        // no-op — safe under a stale mapping.
        if let AbletonMappingTarget::MacroSlot { slot_index } = target {
            manifold_core::macro_bank::MacroBank::apply_macro(project, *slot_index, value);
        } else if let Some(fx) = project.find_preset_instance_mut(target) {
            fx.set_base_param(param_id, value);
        }
    }

    // ── Discovery ─────────��───────────────────────────────────────

    /// Start session discovery.
    pub fn start_discovery(&mut self, realtime: f64) {
        self.discovery_state = DiscoveryState::WaitingTrackCount { started: realtime };
        self.send_osc("/live/song/get/num_tracks", &[]);
        log::info!("[AbletonBridge] Starting discovery...");
    }

    fn check_discovery_timeout(&mut self, realtime: f64) {
        let timed_out = match &self.discovery_state {
            DiscoveryState::Idle | DiscoveryState::Complete => false,
            DiscoveryState::WaitingTrackCount { started }
            | DiscoveryState::QueryingTracks { started, .. }
            | DiscoveryState::QueryingDevices { started, .. }
            | DiscoveryState::QueryingParams { started, .. } => {
                realtime - started > DISCOVERY_STEP_TIMEOUT_SECS
            }
        };
        if timed_out {
            log::warn!("[AbletonBridge] Discovery step timed out — returning to idle");
            self.discovery_state = DiscoveryState::Idle;
        }
    }

    fn handle_message(&mut self, msg: &OscMessage, realtime: f64) {
        self.last_response = realtime;

        // If we weren't connected, we are now — always re-discover after any disconnect.
        if !self.connected {
            self.connected = true;
            self.session.connected = true;
            self.start_discovery(realtime);
        }

        let addr = msg.address.as_str();

        // Route parameter listener updates to pending values
        if addr == "/live/device/get/parameter/value" {
            self.handle_param_value(msg);
            return;
        }

        // Route transport listener updates to pending transport state
        if self.transport_enabled {
            match addr {
                "/live/song/get/is_playing" => {
                    if let Some(val) = msg.args.first().and_then(osc_arg_int) {
                        self.pending_transport.lock().is_playing = Some(val != 0);
                    }
                    return;
                }
                "/live/song/get/tempo" => {
                    if let Some(val) = msg.args.first().and_then(osc_arg_float) {
                        self.pending_transport.lock().tempo = Some(val);
                    }
                    return;
                }
                "/live/song/get/current_song_time" => {
                    if let Some(val) = msg.args.first().and_then(osc_arg_float) {
                        self.pending_transport.lock().song_time = Some(val);
                    }
                    return;
                }
                _ => {}
            }
        }

        // Discovery responses
        match addr {
            "/live/song/get/num_tracks" => self.handle_track_count(msg, realtime),
            "/live/track/get/name" => self.handle_track_name(msg, realtime),
            "/live/track/get/devices/name" => self.handle_device_names(msg, realtime),
            "/live/track/get/devices/class_name" => {
                self.handle_device_classes(msg, realtime);
            }
            "/live/device/get/parameters/name" => {
                self.handle_param_names(msg, realtime);
            }
            "/live/device/get/parameters/min" => {
                self.handle_param_min(msg, realtime);
            }
            "/live/device/get/parameters/max" => {
                self.handle_param_max(msg, realtime);
            }
            "/live/song/get/cue_points" => self.handle_cue_points(msg),
            // PLAY-group discovery responses (only routed when actively fetching).
            "/live/track/get/is_foldable" if self.play_group_discovery.in_progress => {
                self.handle_play_group_bool(msg, /*is_foldable=*/ true);
            }
            "/live/track/get/is_grouped" if self.play_group_discovery.in_progress => {
                self.handle_play_group_bool(msg, /*is_foldable=*/ false);
            }
            "/live/track/get/mute" if self.play_group_discovery.in_progress => {
                self.handle_play_group_mute(msg);
            }
            "/live/track/get/arrangement_clips/name" if self.play_group_discovery.in_progress => {
                self.handle_play_group_clip_names(msg);
            }
            "/live/track/get/arrangement_clips/start_time"
                if self.play_group_discovery.in_progress =>
            {
                self.handle_play_group_clip_starts(msg);
            }
            "/live/track/get/arrangement_clips/end_time"
                if self.play_group_discovery.in_progress =>
            {
                self.handle_play_group_clip_ends(msg);
            }
            "/live/track/get/arrangement_clips/muted" if self.play_group_discovery.in_progress => {
                self.handle_play_group_clip_muted(msg);
            }
            // We use the binary parameter value (handled via fast path), not
            // the formatted string. Drop these silently — AbletonOSC sends
            // them whenever a parameter listener fires.
            "/live/device/get/parameter/value_string" => {}
            "/live/error" => {
                let detail = msg
                    .args
                    .first()
                    .and_then(osc_arg_string)
                    .unwrap_or_else(|| "(no detail)".to_string());
                // AbletonOSC emits "Observer not connected" repeatedly for
                // benign listener teardown races during discovery. Suppress
                // those; surface anything else.
                if !detail.contains("Observer not connected") {
                    eprintln!("[AbletonBridge] /live/error: {detail}");
                }
            }
            other => {
                log::debug!(
                    "[AbletonBridge] unhandled inbound address: {other} (args={})",
                    msg.args.len()
                );
            }
        }
    }

    /// Parse a `/live/song/get/cue_points` reply.
    ///
    /// AbletonOSC sends cue points as a flat list of alternating values.
    /// To stay tolerant of either ordering, we walk the args in pairs and
    /// classify each element as time-or-name by its OSC type rather than
    /// position. This way `[name, time, name, time, ...]` and
    /// `[time, name, time, name, ...]` both parse correctly.
    fn handle_cue_points(&mut self, msg: &OscMessage) {
        log::debug!(
            "[AbletonBridge] /live/song/get/cue_points response: {} arg(s)",
            msg.args.len()
        );
        let mut cues: Vec<CuePoint> = Vec::with_capacity(msg.args.len() / 2);
        let mut i = 0;
        while i + 1 < msg.args.len() {
            let a = &msg.args[i];
            let b = &msg.args[i + 1];
            let (time, name) = match (osc_arg_string(a), osc_arg_float(b)) {
                (Some(name), Some(time)) => (time as f64, name),
                _ => match (osc_arg_float(a), osc_arg_string(b)) {
                    (Some(time), Some(name)) => (time as f64, name),
                    _ => {
                        i += 2;
                        continue;
                    }
                },
            };
            cues.push(CuePoint { time, name });
            i += 2;
        }
        cues.sort_by(|a, b| {
            a.time
                .partial_cmp(&b.time)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        log::info!("[AbletonBridge] Parsed {} cue point(s)", cues.len());
        self.session.cue_points = cues;
        self.session_version += 1;
    }

    // ── PLAY-group discovery handlers ─────────────────────────────

    /// Phase 1 response: `(track_id, value)` for is_foldable or is_grouped.
    fn handle_play_group_bool(&mut self, msg: &OscMessage, is_foldable: bool) {
        if msg.args.len() < 2 {
            return;
        }
        let Some(tid) = osc_arg_int(&msg.args[0]) else {
            return;
        };
        let val = osc_arg_int(&msg.args[1]).map(|v| v != 0).unwrap_or(false);
        if is_foldable {
            self.play_group_discovery.foldable.insert(tid, val);
        } else {
            self.play_group_discovery.grouped.insert(tid, val);
        }
        self.maybe_advance_play_group_phase_1();
    }

    /// Phase 2 response: `(track_id, mute_value)`.
    fn handle_play_group_mute(&mut self, msg: &OscMessage) {
        if msg.args.len() < 2 {
            return;
        }
        let Some(tid) = osc_arg_int(&msg.args[0]) else {
            return;
        };
        if !self.play_group_discovery.leaf_indices.contains(&tid) {
            return; // not one of ours
        }
        let val = osc_arg_int(&msg.args[1]).map(|v| v != 0).unwrap_or(false);
        self.play_group_discovery.mutes.insert(tid, val);
        self.maybe_advance_play_group_phase_2();
    }

    /// Phase 2 response: `(track_id, name1, name2, ...)`.
    fn handle_play_group_clip_names(&mut self, msg: &OscMessage) {
        if msg.args.is_empty() {
            return;
        }
        let Some(tid) = osc_arg_int(&msg.args[0]) else {
            return;
        };
        if !self.play_group_discovery.leaf_indices.contains(&tid) {
            return;
        }
        let names: Vec<String> = msg.args[1..].iter().filter_map(osc_arg_string).collect();
        self.play_group_discovery.clip_names.insert(tid, names);
        self.maybe_advance_play_group_phase_2();
    }

    /// Phase 2 response: `(track_id, end1, end2, ...)`.
    fn handle_play_group_clip_ends(&mut self, msg: &OscMessage) {
        if msg.args.is_empty() {
            return;
        }
        let Some(tid) = osc_arg_int(&msg.args[0]) else {
            return;
        };
        if !self.play_group_discovery.leaf_indices.contains(&tid) {
            return;
        }
        let ends: Vec<f64> = msg.args[1..]
            .iter()
            .filter_map(|a| osc_arg_float(a).map(|v| v as f64))
            .collect();
        self.play_group_discovery.clip_ends.insert(tid, ends);
        self.maybe_advance_play_group_phase_2();
    }

    /// Phase 2 response: `(track_id, muted1, muted2, ...)`.
    fn handle_play_group_clip_muted(&mut self, msg: &OscMessage) {
        if msg.args.is_empty() {
            return;
        }
        let Some(tid) = osc_arg_int(&msg.args[0]) else {
            return;
        };
        if !self.play_group_discovery.leaf_indices.contains(&tid) {
            return;
        }
        let muted: Vec<bool> = msg.args[1..]
            .iter()
            .filter_map(|a| osc_arg_int(a).map(|v| v != 0))
            .collect();
        self.play_group_discovery.clip_muted.insert(tid, muted);
        self.maybe_advance_play_group_phase_2();
    }

    /// Phase 2 response: `(track_id, start1, start2, ...)`.
    fn handle_play_group_clip_starts(&mut self, msg: &OscMessage) {
        if msg.args.is_empty() {
            return;
        }
        let Some(tid) = osc_arg_int(&msg.args[0]) else {
            return;
        };
        if !self.play_group_discovery.leaf_indices.contains(&tid) {
            return;
        }
        let starts: Vec<f64> = msg.args[1..]
            .iter()
            .filter_map(|a| osc_arg_float(a).map(|v| v as f64))
            .collect();
        self.play_group_discovery.clip_starts.insert(tid, starts);
        self.maybe_advance_play_group_phase_2();
    }

    fn handle_param_value(&self, msg: &OscMessage) {
        // Args: track_id, device_id, param_id, value
        if msg.args.len() < 4 {
            return;
        }
        let track_id = osc_arg_int(&msg.args[0]).unwrap_or(-1);
        let device_id = osc_arg_int(&msg.args[1]).unwrap_or(-1);
        let param_id = osc_arg_int(&msg.args[2]).unwrap_or(-1);
        let value = osc_arg_float(&msg.args[3]).unwrap_or(0.0);

        let key = (track_id, device_id, param_id);
        if self.write_targets.contains_key(&key) {
            self.pending_values.lock().insert(key, value);
        }
    }

    fn handle_track_count(&mut self, msg: &OscMessage, realtime: f64) {
        let count = msg.args.first().and_then(osc_arg_int).unwrap_or(0);

        // Check if track count changed (re-discovery needed)
        if matches!(
            self.discovery_state,
            DiscoveryState::Idle | DiscoveryState::Complete
        ) && self.session.tracks.len() as i32 != count
            && !self.session.tracks.is_empty()
        {
            log::info!(
                "[AbletonBridge] Track count changed ({} → {count}) — re-discovering",
                self.session.tracks.len()
            );
            self.start_discovery(realtime);
            return;
        }

        if let DiscoveryState::WaitingTrackCount { .. } = self.discovery_state {
            if count == 0 {
                self.finish_discovery(Vec::new());
                return;
            }
            self.discovery_state = DiscoveryState::QueryingTracks {
                expected_count: count,
                tracks: Vec::with_capacity(count as usize),
                next_track: count, // all sent
                started: realtime,
            };
            // Send ALL name queries at once — don't wait for each response
            for i in 0..count {
                send_osc_to(
                    &self.send_socket,
                    "/live/track/get/name",
                    &[rosc::OscType::Int(i)],
                );
            }
        }
    }

    fn handle_track_name(&mut self, msg: &OscMessage, realtime: f64) {
        let DiscoveryState::QueryingTracks {
            expected_count,
            ref mut tracks,
            ref mut next_track,
            ref mut started,
        } = self.discovery_state
        else {
            return;
        };

        let track_id = msg
            .args
            .first()
            .and_then(osc_arg_int)
            .unwrap_or(*next_track);
        let name = msg.args.get(1).and_then(osc_arg_string).unwrap_or_default();

        tracks.push((track_id, name.clone()));
        *next_track = track_id + 1;

        *started = realtime;
        if tracks.len() as i32 >= expected_count {
            // All names received — sort by track_id and burst-query devices
            let mut track_list: Vec<(i32, String)> = tracks.clone();
            track_list.sort_by_key(|(id, _)| *id);
            // Send device name + class_name queries for all tracks
            for (tid, _) in &track_list {
                send_osc_to(
                    &self.send_socket,
                    "/live/track/get/devices/name",
                    &[rosc::OscType::Int(*tid)],
                );
                send_osc_to(
                    &self.send_socket,
                    "/live/track/get/devices/class_name",
                    &[rosc::OscType::Int(*tid)],
                );
            }
            let pending: Vec<(i32, String)> = track_list.into_iter().rev().collect();
            self.discovery_state = DiscoveryState::QueryingDevices {
                tracks: Vec::new(),
                pending_tracks: pending,
                started: realtime,
            };
        }
    }

    /// Handle `/live/track/get/devices/name` response.
    /// Stores device names keyed by track_id in the pending state.
    fn handle_device_names(&mut self, msg: &OscMessage, realtime: f64) {
        let DiscoveryState::QueryingDevices {
            ref mut tracks,
            ref mut pending_tracks,
            ref mut started,
            ..
        } = self.discovery_state
        else {
            return;
        };

        let tid = msg.args.first().and_then(osc_arg_int).unwrap_or(-1);
        let names: Vec<String> = msg.args[1..].iter().filter_map(osc_arg_string).collect();

        // Store names on the track entry (create if needed)
        if let Some(track) = tracks.iter_mut().find(|t| t.track_id == tid) {
            // Names arrived first or second — merge
            for (i, name) in names.iter().enumerate() {
                if i < track.devices.len() {
                    track.devices[i].name = name.clone();
                } else {
                    track.devices.push(AbletonDevice {
                        device_id: i as i32,
                        name: name.clone(),
                        class_name: String::new(),
                        macros: Vec::new(),
                    });
                }
            }
        } else {
            // First response for this track — create entry
            let track_name = pending_tracks
                .iter()
                .find(|(id, _)| *id == tid)
                .map(|(_, n)| n.clone())
                .unwrap_or_default();
            tracks.push(AbletonTrack {
                track_id: tid,
                name: track_name,
                devices: names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| AbletonDevice {
                        device_id: i as i32,
                        name: n.clone(),
                        class_name: String::new(),
                        macros: Vec::new(),
                    })
                    .collect(),
            });
        }
        *started = realtime;
        self.try_finish_device_query();
    }

    /// Handle `/live/track/get/devices/class_name` response.
    fn handle_device_classes(&mut self, msg: &OscMessage, realtime: f64) {
        let DiscoveryState::QueryingDevices {
            ref mut tracks,
            ref mut pending_tracks,
            ref mut started,
            ..
        } = self.discovery_state
        else {
            return;
        };

        let tid = msg.args.first().and_then(osc_arg_int).unwrap_or(-1);
        let classes: Vec<String> = msg.args[1..].iter().filter_map(osc_arg_string).collect();

        if let Some(track) = tracks.iter_mut().find(|t| t.track_id == tid) {
            for (i, cls) in classes.iter().enumerate() {
                if i < track.devices.len() {
                    track.devices[i].class_name = cls.clone();
                } else {
                    track.devices.push(AbletonDevice {
                        device_id: i as i32,
                        name: String::new(),
                        class_name: cls.clone(),
                        macros: Vec::new(),
                    });
                }
            }
        } else {
            let track_name = pending_tracks
                .iter()
                .find(|(id, _)| *id == tid)
                .map(|(_, n)| n.clone())
                .unwrap_or_default();
            tracks.push(AbletonTrack {
                track_id: tid,
                name: track_name,
                devices: classes
                    .iter()
                    .enumerate()
                    .map(|(i, c)| AbletonDevice {
                        device_id: i as i32,
                        name: String::new(),
                        class_name: c.clone(),
                        macros: Vec::new(),
                    })
                    .collect(),
            });
        }
        *started = realtime;
        self.try_finish_device_query();
    }

    /// Check if all device name + class_name responses have arrived.
    /// We expect 2 responses per track (name + class_name). When all tracks
    /// have devices with non-empty class_names, device query is complete.
    fn try_finish_device_query(&mut self) {
        let DiscoveryState::QueryingDevices {
            ref tracks,
            ref pending_tracks,
            ..
        } = self.discovery_state
        else {
            return;
        };

        // We need responses for every track in pending_tracks
        let all_received = pending_tracks.iter().all(|(tid, _)| {
            tracks.iter().any(|t| {
                t.track_id == *tid
                    && !t.devices.is_empty()
                    && t.devices.iter().all(|d| !d.class_name.is_empty())
            })
        });

        // Also accept if we have as many track entries as pending
        // (tracks with zero devices won't have class_names)
        let count_match = tracks.len() >= pending_tracks.len();

        if !all_received && !count_match {
            return;
        }

        // Extract state for the transition
        let DiscoveryState::QueryingDevices {
            tracks: ref final_tracks,
            pending_tracks: _,
            started,
            ..
        } = self.discovery_state
        else {
            return;
        };
        let realtime = started;
        let final_tracks = final_tracks.clone();

        // Collect ALL rack devices across all tracks for param queries
        let mut all_racks: Vec<(i32, i32, String, String)> = Vec::new();
        for track in &final_tracks {
            for device in &track.devices {
                if is_rack_device(&device.class_name) {
                    all_racks.push((
                        track.track_id,
                        device.device_id,
                        device.name.clone(),
                        device.class_name.clone(),
                    ));
                }
            }
        }

        if all_racks.is_empty() {
            // No rack devices — discovery complete
            self.finish_discovery(final_tracks);
        } else {
            // Burst-send param name/min/max queries for ALL rack devices
            for (rtid, rdid, _, _) in &all_racks {
                let args = &[rosc::OscType::Int(*rtid), rosc::OscType::Int(*rdid)];
                send_osc_to(&self.send_socket, "/live/device/get/parameters/name", args);
                send_osc_to(&self.send_socket, "/live/device/get/parameters/min", args);
                send_osc_to(&self.send_socket, "/live/device/get/parameters/max", args);
            }
            let pending_racks: Vec<(i32, i32, String, String)> =
                all_racks.into_iter().rev().collect();
            self.discovery_state = DiscoveryState::QueryingParams {
                tracks: final_tracks,
                pending_racks,
                started: realtime,
            };
        }
    }

    /// Handle `/live/device/get/parameters/name` — store macro names on device.
    fn handle_param_names(&mut self, msg: &OscMessage, realtime: f64) {
        let DiscoveryState::QueryingParams {
            ref mut tracks,
            ref mut started,
            ..
        } = self.discovery_state
        else {
            return;
        };

        let tid = msg.args.first().and_then(osc_arg_int).unwrap_or(-1);
        let did = msg.args.get(1).and_then(osc_arg_int).unwrap_or(-1);
        let names: Vec<String> = msg.args[2..].iter().filter_map(osc_arg_string).collect();

        if let Some(track) = tracks.iter_mut().find(|t| t.track_id == tid)
            && let Some(device) = track.devices.iter_mut().find(|d| d.device_id == did)
        {
            // Skip param 0 ("Device On") — rack macros are at indices 1-8.
            for (i, name) in names.iter().skip(1).take(RACK_MACRO_COUNT).enumerate() {
                let param_id = (i + 1) as i32; // 1-based index in Ableton
                if i < device.macros.len() {
                    device.macros[i].name = name.clone();
                    device.macros[i].param_id = param_id;
                } else {
                    device.macros.push(AbletonMacro {
                        param_id,
                        name: name.clone(),
                        value: 0.0,
                        min: 0.0,
                        max: 1.0,
                    });
                }
            }
        }
        *started = realtime;
        self.try_finish_param_query();
    }

    /// Handle `/live/device/get/parameters/min` — store min values.
    fn handle_param_min(&mut self, msg: &OscMessage, realtime: f64) {
        let DiscoveryState::QueryingParams {
            ref mut tracks,
            ref mut started,
            ..
        } = self.discovery_state
        else {
            return;
        };

        let tid = msg.args.first().and_then(osc_arg_int).unwrap_or(-1);
        let did = msg.args.get(1).and_then(osc_arg_int).unwrap_or(-1);
        let mins: Vec<f32> = msg.args[2..].iter().filter_map(osc_arg_float).collect();

        if let Some(track) = tracks.iter_mut().find(|t| t.track_id == tid)
            && let Some(device) = track.devices.iter_mut().find(|d| d.device_id == did)
        {
            // Skip param 0 ("Device On") — macros start at index 1.
            for (i, &min_val) in mins.iter().skip(1).take(RACK_MACRO_COUNT).enumerate() {
                if i < device.macros.len() {
                    device.macros[i].min = min_val;
                }
            }
        }
        *started = realtime;
        self.try_finish_param_query();
    }

    /// Handle `/live/device/get/parameters/max` — store max values.
    fn handle_param_max(&mut self, msg: &OscMessage, realtime: f64) {
        let DiscoveryState::QueryingParams {
            ref mut tracks,
            ref mut started,
            ..
        } = self.discovery_state
        else {
            return;
        };

        let tid = msg.args.first().and_then(osc_arg_int).unwrap_or(-1);
        let did = msg.args.get(1).and_then(osc_arg_int).unwrap_or(-1);
        let maxs: Vec<f32> = msg.args[2..].iter().filter_map(osc_arg_float).collect();

        if let Some(track) = tracks.iter_mut().find(|t| t.track_id == tid)
            && let Some(device) = track.devices.iter_mut().find(|d| d.device_id == did)
        {
            // Skip param 0 ("Device On") — macros start at index 1.
            for (i, &max_val) in maxs.iter().skip(1).take(RACK_MACRO_COUNT).enumerate() {
                if i < device.macros.len() {
                    device.macros[i].max = max_val;
                }
            }
        }
        *started = realtime;
        self.try_finish_param_query();
    }

    /// Check if all param name/min/max responses have arrived for all rack devices.
    /// We sent 3 queries per rack (name, min, max). A rack is "complete" when
    /// its macros have non-empty names (name response arrived).
    fn try_finish_param_query(&mut self) {
        let DiscoveryState::QueryingParams {
            ref tracks,
            ref pending_racks,
            ..
        } = self.discovery_state
        else {
            return;
        };

        // Check if all racks have macros with names filled in
        let all_done = pending_racks.iter().all(|(rtid, rdid, _, _)| {
            tracks
                .iter()
                .find(|t| t.track_id == *rtid)
                .and_then(|t| t.devices.iter().find(|d| d.device_id == *rdid))
                .is_some_and(|d| {
                    !d.macros.is_empty() && d.macros.iter().all(|m| !m.name.is_empty())
                })
        });

        if !all_done {
            return;
        }

        let DiscoveryState::QueryingParams { ref tracks, .. } = self.discovery_state else {
            return;
        };
        let final_tracks = tracks.clone();
        self.finish_discovery(final_tracks);
    }

    fn finish_discovery(&mut self, tracks: Vec<AbletonTrack>) {
        self.session.tracks = tracks;
        self.session.connected = true;
        self.session_version += 1;
        self.discovery_state = DiscoveryState::Complete;
        self.validation_dirty = true;

        // Fetch locators / cue points so the perform-mode HUD has them.
        // Reply arrives async on /live/song/get/cue_points and is handled
        // in handle_message → handle_cue_points.
        log::debug!("[AbletonBridge] Requesting cue points (/live/song/get/cue_points)");
        send_osc_to(&self.send_socket, "/live/song/get/cue_points", &[]);

        // Kick off PLAY-group discovery in parallel.
        self.start_play_group_discovery();
    }

    /// Phase 1 of PLAY-group discovery: query is_foldable + is_grouped for
    /// every track. Once both maps are full we identify PLAY's leaf
    /// descendants and advance to phase 2.
    fn start_play_group_discovery(&mut self) {
        let n = self.session.tracks.len() as i32;
        if n == 0 {
            return;
        }
        self.play_group_discovery = PlayGroupDiscovery {
            in_progress: true,
            track_count: n,
            ..Default::default()
        };
        // Cache leaf names from the main discovery's track list.
        for t in &self.session.tracks {
            self.play_group_discovery
                .leaf_names
                .insert(t.track_id, t.name.clone());
        }
        log::debug!(
            "[AbletonBridge] Starting PLAY-group discovery (querying is_foldable + is_grouped for {n} tracks)"
        );
        for tid in 0..n {
            send_osc_to(
                &self.send_socket,
                "/live/track/get/is_foldable",
                &[rosc::OscType::Int(tid)],
            );
            send_osc_to(
                &self.send_socket,
                "/live/track/get/is_grouped",
                &[rosc::OscType::Int(tid)],
            );
        }
    }

    /// Called whenever a phase 1 response arrives. If both maps are now
    /// complete, derive PLAY's leaves and start phase 2.
    fn maybe_advance_play_group_phase_1(&mut self) {
        let pg = &self.play_group_discovery;
        if !pg.in_progress || pg.structure_complete {
            return;
        }
        let n = pg.track_count as usize;
        if pg.foldable.len() < n || pg.grouped.len() < n {
            return;
        }
        // Both structure maps are full — derive leaves.
        let leaves = self.derive_play_group_leaves();
        let pg = &mut self.play_group_discovery;
        pg.structure_complete = true;
        pg.leaf_indices = leaves.clone();
        log::info!(
            "[AbletonBridge] PLAY-group structure complete: {} leaf track(s)",
            leaves.len()
        );
        if leaves.is_empty() {
            // No PLAY group (or it's empty) — finalize as None.
            self.finalize_play_group_discovery();
            return;
        }
        // Phase 2: query mute + arrangement clips (start, end, muted, name) for each leaf.
        for tid in &leaves {
            let arg = [rosc::OscType::Int(*tid)];
            send_osc_to(&self.send_socket, "/live/track/get/mute", &arg);
            send_osc_to(
                &self.send_socket,
                "/live/track/get/arrangement_clips/name",
                &arg,
            );
            send_osc_to(
                &self.send_socket,
                "/live/track/get/arrangement_clips/start_time",
                &arg,
            );
            send_osc_to(
                &self.send_socket,
                "/live/track/get/arrangement_clips/end_time",
                &arg,
            );
            send_osc_to(
                &self.send_socket,
                "/live/track/get/arrangement_clips/muted",
                &arg,
            );
        }
    }

    /// Walk the flat track list and find leaf (non-foldable) descendants
    /// of the first top-level group whose normalized name matches
    /// `PLAY_GROUP_NAME`. Normalization strips leading digits + whitespace
    /// so "1 PLAY", "12 PLAY", and "PLAY" all match — users commonly
    /// number-prefix track names in Ableton.
    fn derive_play_group_leaves(&self) -> Vec<i32> {
        let pg = &self.play_group_discovery;
        let tracks = &self.session.tracks;

        // Diagnostic: list every top-level foldable track's normalized name.
        let candidates: Vec<(i32, String, String)> = tracks
            .iter()
            .filter(|t| {
                pg.foldable.get(&t.track_id).copied().unwrap_or(false)
                    && !pg.grouped.get(&t.track_id).copied().unwrap_or(false)
            })
            .map(|t| (t.track_id, t.name.clone(), normalize_group_name(&t.name)))
            .collect();
        log::debug!("[AbletonBridge] PLAY-group: top-level foldable candidates = {candidates:?}");

        let play_idx = tracks.iter().position(|t| {
            pg.foldable.get(&t.track_id).copied().unwrap_or(false)
                && !pg.grouped.get(&t.track_id).copied().unwrap_or(false)
                && normalize_group_name(&t.name).eq_ignore_ascii_case(PLAY_GROUP_NAME)
        });
        let Some(play_pos) = play_idx else {
            return Vec::new();
        };
        // Walk forward from PLAY+1; collect leaf descendants until we hit
        // a track that's back at the top level (is_grouped=false).
        let mut leaves = Vec::new();
        for t in tracks.iter().skip(play_pos + 1) {
            let is_grouped = pg.grouped.get(&t.track_id).copied().unwrap_or(false);
            if !is_grouped {
                break;
            }
            let is_foldable = pg.foldable.get(&t.track_id).copied().unwrap_or(false);
            if !is_foldable {
                leaves.push(t.track_id);
            }
        }
        leaves
    }

    /// Called whenever a phase 2 response arrives. If all required maps are
    /// complete for every leaf, build the GroupTracks and publish it.
    fn maybe_advance_play_group_phase_2(&mut self) {
        let pg = &self.play_group_discovery;
        if !pg.in_progress || !pg.structure_complete {
            return;
        }
        let needed = pg.leaf_indices.len();
        if pg.mutes.len() < needed
            || pg.clip_names.len() < needed
            || pg.clip_starts.len() < needed
            || pg.clip_ends.len() < needed
            || pg.clip_muted.len() < needed
        {
            return;
        }
        self.finalize_play_group_discovery();
    }

    fn finalize_play_group_discovery(&mut self) {
        let pg = &mut self.play_group_discovery;
        if !pg.in_progress {
            return;
        }
        let mut tracks: Vec<TrackArrangement> = Vec::with_capacity(pg.leaf_indices.len());
        for tid in &pg.leaf_indices {
            let name = pg.leaf_names.get(tid).cloned().unwrap_or_default();
            let muted = pg.mutes.get(tid).copied().unwrap_or(false);
            let starts = pg.clip_starts.get(tid).cloned().unwrap_or_default();
            let ends = pg.clip_ends.get(tid).cloned().unwrap_or_default();
            let clip_muted = pg.clip_muted.get(tid).cloned().unwrap_or_default();
            // Parallel arrays — zip on the shortest. AbletonOSC should
            // always return matching lengths but we defend against
            // partial responses (UDP loss / response truncation).
            let count = starts.len().min(ends.len()).min(clip_muted.len());
            let mut clips: Vec<ArrangementClip> = Vec::with_capacity(count);
            for i in 0..count {
                clips.push(ArrangementClip {
                    start: starts[i],
                    end: ends[i],
                    muted: clip_muted[i],
                });
            }
            tracks.push(TrackArrangement {
                track_id: *tid,
                name,
                muted,
                clips,
            });
        }
        let group = if tracks.is_empty() {
            None
        } else {
            Some(GroupTracks {
                name: PLAY_GROUP_NAME.to_string(),
                tracks,
            })
        };
        let n_tracks = group.as_ref().map(|g| g.tracks.len()).unwrap_or(0);
        let n_clips: usize = group
            .as_ref()
            .map(|g| g.tracks.iter().map(|t| t.clips.len()).sum())
            .unwrap_or(0);
        log::info!(
            "[AbletonBridge] PLAY-group complete: {n_tracks} track(s), {n_clips} clip(s) total"
        );
        self.session.play_group = group;
        self.session_version += 1;
        // Reset accumulator so a future re-discovery starts fresh.
        self.play_group_discovery = PlayGroupDiscovery::default();
    }

    /// Returns true (and clears the flag) when discovery just completed and the
    /// caller should run `validate_mappings` + `rebuild_listeners`.
    pub fn take_validation_dirty(&mut self) -> bool {
        let dirty = self.validation_dirty;
        self.validation_dirty = false;
        dirty
    }

    // ── Structural validation ─────────────────────────────────────

    /// Validate all Ableton mappings in the project against the current session.
    /// Auto-updates when unambiguous, flags when ambiguous.
    pub fn validate_mappings(&self, project: &mut Project) {
        if !self.connected || self.session.tracks.is_empty() {
            // Mark all dormant
            Self::set_all_mapping_status(project, AbletonMappingStatus::Dormant);
            return;
        }

        // Validate master effects
        for fx in &mut project.settings.master_effects {
            if let Some(mappings) = &mut fx.ableton_mappings {
                for mapping in mappings.iter_mut() {
                    mapping.status = self.validate_single_mapping(mapping);
                }
            }
        }

        // Validate layer effects and gen params
        for layer in project.timeline.layers.iter_mut() {
            if let Some(effects) = &mut layer.effects {
                for fx in effects.iter_mut() {
                    if let Some(mappings) = &mut fx.ableton_mappings {
                        for mapping in mappings.iter_mut() {
                            mapping.status = self.validate_single_mapping(mapping);
                        }
                    }
                }
            }
            if let Some(gp) = layer.gen_params_mut()
                && let Some(mappings) = &mut gp.ableton_mappings
            {
                for mapping in mappings.iter_mut() {
                    mapping.status = self.validate_single_mapping(mapping);
                }
            }
        }

        // Validate macro slots
        for slot in &mut project.settings.macro_bank.slots {
            if let Some(mapping) = &mut slot.ableton_mapping {
                mapping.status = self.validate_single_mapping(mapping);
            }
        }
    }

    /// Resolve a mapping against the current session and update its
    /// numeric IDs in place. Names are the canonical identity; IDs are a
    /// frame-local cache that can shift whenever the user edits the
    /// Ableton project.
    ///
    /// Resolution order (each step is logged when it fires):
    ///
    /// 1. **Canonical: by name + slot.** Find the track named
    ///    `track_name`, then the device named `device_name` inside it
    ///    (with matching class), then macro at slot `param_id`. Survives
    ///    track reorders, device reorders, and macro renames.
    /// 2. **By name + macro name.** Same lookup but the macro is found
    ///    by `macro_name` instead of slot — handles the case where the
    ///    user reordered macros inside the rack.
    /// 3. **Legacy: stored IDs + class.** The pre-name resolver. Used
    ///    only when names are empty (legacy projects) — resolves and
    ///    backfills the names so the next save persists them.
    /// 4. **Class-only fuzzy search.** Last resort when nothing else
    ///    matches: a unique device of the right class with a macro at
    ///    the same slot. Predates the rename clue but kept for parity
    ///    with the previous resolver's "auto-update on structural
    ///    change" behavior.
    /// 5. **Dormant** — none of the above resolved. Mapping is visibly
    ///    broken; no parameter writes will fire for it.
    fn validate_single_mapping(&self, mapping: &mut AbletonParamMapping) -> AbletonMappingStatus {
        let target_class = mapping.address.device_identity.device_class_name.clone();
        let stored_track_name = mapping.address.track_name.clone();
        let stored_device_name = mapping.address.device_name.clone();
        let stored_macro_name = mapping.address.macro_name.clone();
        let stored_param_id = mapping.address.param_id;

        let have_names = !stored_track_name.is_empty() && !stored_device_name.is_empty();

        // ── 1+2. Canonical: by (track_name, device_name) then macro ──
        //
        // Inside the resolved device we disambiguate two cases that both
        // change `macros[stored_param_id].name`:
        //
        // - **Rename:** the user renamed the macro at the stored slot.
        //   The original `macro_name` does NOT appear elsewhere in the
        //   rack, so we trust the slot and update the cached name.
        //
        // - **Reorder:** the user dragged macros around inside the rack.
        //   The original `macro_name` still exists in the device — at a
        //   different slot — so we follow the *name* and update the slot.
        //
        // Either way, the user's intent ("the macro I clicked on") is
        // preserved.
        if have_names
            && let Some(track) = self
                .session
                .tracks
                .iter()
                .find(|t| t.name == stored_track_name)
            && let Some(device) = track
                .devices
                .iter()
                .find(|d| d.name == stored_device_name && d.class_name == target_class)
        {
            // Look for the macro by NAME first — if it's still there
            // under the original name, that's the canonical match
            // regardless of which slot it lives in now.
            if !stored_macro_name.is_empty()
                && let Some(mac) = device.macros.iter().find(|m| m.name == stored_macro_name)
            {
                let slot_changed = mac.param_id != stored_param_id;
                mapping.address.track_id = track.track_id;
                mapping.address.device_id = device.device_id;
                mapping.address.param_id = mac.param_id;
                if slot_changed {
                    log::info!(
                        "[AbletonBridge] Macro slot moved: \
                         {stored_track_name} > {stored_device_name} > \
                         {stored_macro_name} (slot {stored_param_id} → {})",
                        mac.param_id,
                    );
                }
                return AbletonMappingStatus::Active;
            }
            // Original macro_name not found in the device → either it
            // was renamed (trust the slot) or removed entirely.
            if let Some(mac) = device.macros.iter().find(|m| m.param_id == stored_param_id) {
                if mac.name != stored_macro_name {
                    log::info!(
                        "[AbletonBridge] Macro renamed: \
                         {stored_track_name} > {stored_device_name} > \
                         '{stored_macro_name}' → '{}'",
                        mac.name,
                    );
                }
                mapping.address.track_id = track.track_id;
                mapping.address.device_id = device.device_id;
                mapping.address.macro_name = mac.name.clone();
                return AbletonMappingStatus::Active;
            }
        }

        // ── 3. Legacy backfill: stored numeric IDs + class match ────
        // Only for projects saved before name-based resolution existed.
        // On success, backfill the names so the next save migrates the
        // mapping forward and never falls back here again.
        if !have_names {
            let stored_track_id = mapping.address.track_id;
            let stored_device_id = mapping.address.device_id;
            if let Some(track) = self
                .session
                .tracks
                .iter()
                .find(|t| t.track_id == stored_track_id)
                && let Some(device) = track
                    .devices
                    .iter()
                    .find(|d| d.device_id == stored_device_id && d.class_name == target_class)
            {
                mapping.address.track_name = track.name.clone();
                mapping.address.device_name = device.name.clone();
                if let Some(mac) = device.macros.iter().find(|m| m.param_id == stored_param_id) {
                    mapping.address.macro_name = mac.name.clone();
                }
                log::info!(
                    "[AbletonBridge] Backfilled legacy mapping with names: \
                     {} > {}",
                    mapping.address.track_name,
                    mapping.address.device_name,
                );
                return AbletonMappingStatus::Active;
            }
        }

        // ── 4. Class-only fuzzy search (last resort) ────────────────
        // Used when names exist but no track/device matches them — a
        // track or device was renamed in Ableton. We accept the match
        // ONLY if it's unique; otherwise the user must re-link by hand.
        let mut matches: Vec<(i32, i32, String, String, String)> = Vec::new();
        for track in &self.session.tracks {
            for device in &track.devices {
                if device.class_name == target_class
                    && let Some(mac) = device.macros.iter().find(|m| m.param_id == stored_param_id)
                {
                    matches.push((
                        track.track_id,
                        device.device_id,
                        track.name.clone(),
                        device.name.clone(),
                        mac.name.clone(),
                    ));
                }
            }
        }

        match matches.len() {
            0 => AbletonMappingStatus::Dormant,
            1 => {
                let (tid, did, tname, dname, mname) = &matches[0];
                mapping.address.track_id = *tid;
                mapping.address.device_id = *did;
                mapping.address.track_name = tname.clone();
                mapping.address.device_name = dname.clone();
                mapping.address.macro_name = mname.clone();
                log::info!(
                    "[AbletonBridge] Fuzzy auto-resolved (class-only): \
                     {tname} > {dname} > {mname}",
                );
                AbletonMappingStatus::Active
            }
            _ => {
                log::warn!(
                    "[AbletonBridge] Ambiguous mapping for '{stored_track_name}' > \
                     '{stored_device_name}': {} class-only matches — re-link required",
                    matches.len(),
                );
                AbletonMappingStatus::Ambiguous
            }
        }
    }

    fn set_all_mapping_status(project: &mut Project, status: AbletonMappingStatus) {
        for fx in &mut project.settings.master_effects {
            if let Some(mappings) = &mut fx.ableton_mappings {
                for m in mappings.iter_mut() {
                    m.status = status;
                }
            }
        }
        for layer in project.timeline.layers.iter_mut() {
            if let Some(effects) = &mut layer.effects {
                for fx in effects.iter_mut() {
                    if let Some(mappings) = &mut fx.ableton_mappings {
                        for m in mappings.iter_mut() {
                            m.status = status;
                        }
                    }
                }
            }
            if let Some(gp) = layer.gen_params_mut()
                && let Some(mappings) = &mut gp.ableton_mappings
            {
                for m in mappings.iter_mut() {
                    m.status = status;
                }
            }
        }
        for slot in &mut project.settings.macro_bank.slots {
            if let Some(m) = &mut slot.ableton_mapping {
                m.status = status;
            }
        }
    }

    // ── Listener management ──────────────��────────────────────────

    /// Look up the [min, max] range that AbletonOSC uses for a given parameter.
    /// Rack macros are typically 0-127 (raw MIDI) but may have custom ranges.
    fn ableton_param_range(&self, track_id: i32, device_id: i32, param_id: i32) -> (f32, f32) {
        for track in &self.session.tracks {
            if track.track_id != track_id {
                continue;
            }
            for device in &track.devices {
                if device.device_id != device_id {
                    continue;
                }
                for mac in &device.macros {
                    if mac.param_id == param_id {
                        let hi = if mac.max > mac.min {
                            mac.max
                        } else {
                            mac.min + 1.0
                        };
                        return (mac.min, hi);
                    }
                }
            }
        }
        (0.0, 1.0) // unknown — treat as already normalized
    }

    /// Scan all Active mappings in the project and subscribe/unsubscribe accordingly.
    pub fn rebuild_listeners(&mut self, project: &Project) {
        let mut needed: AHashSet<(i32, i32, i32)> = AHashSet::new();
        let mut new_write_targets: AHashMap<(i32, i32, i32), Vec<WriteTarget>> = AHashMap::new();

        // Collect from master effects
        for fx in &project.settings.master_effects {
            if let Some(mappings) = &fx.ableton_mappings {
                for mapping in mappings {
                    if mapping.status != AbletonMappingStatus::Active {
                        continue;
                    }
                    let key = (
                        mapping.address.track_id,
                        mapping.address.device_id,
                        mapping.address.param_id,
                    );
                    needed.insert(key);

                    // Resolve the param on the LIVE manifest (P4) — user-added
                    // and registry-absent (glb-import) params resolve here where
                    // the frozen registry's id_to_index would miss and drop them.
                    let Some(param) = fx.params.get(mapping.param_id.as_ref()) else {
                        continue;
                    };
                    let (pmin, pmax) = (param.spec.min, param.spec.max);
                    let (abl_min, abl_max) = self.ableton_param_range(
                        mapping.address.track_id,
                        mapping.address.device_id,
                        mapping.address.param_id,
                    );

                    new_write_targets.entry(key).or_default().push(WriteTarget {
                        target: AbletonMappingTarget::MasterEffect {
                            effect_type: fx.effect_type().clone(),
                            param_id: mapping.param_id.clone(),
                        },
                        param_id: mapping.param_id.to_string(),
                        ableton_min: abl_min,
                        ableton_max: abl_max,
                        range_min: mapping.range_min,
                        range_max: mapping.range_max,
                        inverted: mapping.inverted,
                        param_min: pmin,
                        param_max: pmax,
                    });
                }
            }
        }

        // Collect from layers
        for layer in project.timeline.layers.iter() {
            let layer_id = layer.layer_id.clone();

            if let Some(effects) = &layer.effects {
                for fx in effects {
                    if let Some(mappings) = &fx.ableton_mappings {
                        for mapping in mappings {
                            if mapping.status != AbletonMappingStatus::Active {
                                continue;
                            }
                            let key = (
                                mapping.address.track_id,
                                mapping.address.device_id,
                                mapping.address.param_id,
                            );
                            needed.insert(key);

                            // Live-manifest resolution (P4) — see master-effect
                            // path; user-added / registry-absent params resolve
                            // here instead of being dropped by an id_to_index miss.
                            let Some(param) = fx.params.get(mapping.param_id.as_ref())
                            else {
                                continue;
                            };
                            let (pmin, pmax) = (param.spec.min, param.spec.max);
                            let (abl_min, abl_max) = self.ableton_param_range(
                                mapping.address.track_id,
                                mapping.address.device_id,
                                mapping.address.param_id,
                            );

                            new_write_targets.entry(key).or_default().push(WriteTarget {
                                target: AbletonMappingTarget::LayerEffect {
                                    layer_id: layer_id.clone(),
                                    effect_type: fx.effect_type().clone(),
                                    param_id: mapping.param_id.clone(),
                                },
                                param_id: mapping.param_id.to_string(),
                                ableton_min: abl_min,
                                ableton_max: abl_max,
                                range_min: mapping.range_min,
                                range_max: mapping.range_max,
                                inverted: mapping.inverted,
                                param_min: pmin,
                                param_max: pmax,
                            });
                        }
                    }
                }
            }

            if let Some(gp) = layer.gen_params()
                && let Some(mappings) = &gp.ableton_mappings
            {
                for mapping in mappings {
                    if mapping.status != AbletonMappingStatus::Active {
                        continue;
                    }
                    let key = (
                        mapping.address.track_id,
                        mapping.address.device_id,
                        mapping.address.param_id,
                    );
                    needed.insert(key);

                    // Live-manifest resolution (P4) — generator params, including
                    // glb-import sliders absent from the registry, resolve here.
                    let Some(param) = gp.params.get(mapping.param_id.as_ref()) else {
                        continue;
                    };
                    let (pmin, pmax) = (param.spec.min, param.spec.max);
                    let (abl_min, abl_max) = self.ableton_param_range(
                        mapping.address.track_id,
                        mapping.address.device_id,
                        mapping.address.param_id,
                    );

                    new_write_targets.entry(key).or_default().push(WriteTarget {
                        target: AbletonMappingTarget::GenParam {
                            layer_id: layer_id.clone(),
                            param_id: mapping.param_id.clone(),
                        },
                        param_id: mapping.param_id.to_string(),
                        ableton_min: abl_min,
                        ableton_max: abl_max,
                        range_min: mapping.range_min,
                        range_max: mapping.range_max,
                        inverted: mapping.inverted,
                        param_min: pmin,
                        param_max: pmax,
                    });
                }
            }
        }

        // Collect from macro slots
        for (i, slot) in project.settings.macro_bank.slots.iter().enumerate() {
            if let Some(mapping) = &slot.ableton_mapping {
                if mapping.status != AbletonMappingStatus::Active {
                    continue;
                }
                let key = (
                    mapping.address.track_id,
                    mapping.address.device_id,
                    mapping.address.param_id,
                );
                needed.insert(key);
                let (abl_min, abl_max) = self.ableton_param_range(
                    mapping.address.track_id,
                    mapping.address.device_id,
                    mapping.address.param_id,
                );

                new_write_targets.entry(key).or_default().push(WriteTarget {
                    target: AbletonMappingTarget::MacroSlot { slot_index: i },
                    // Macros route via slot_index; the id funnel is unused here.
                    param_id: String::new(),
                    ableton_min: abl_min,
                    ableton_max: abl_max,
                    range_min: mapping.range_min,
                    range_max: mapping.range_max,
                    inverted: mapping.inverted,
                    param_min: 0.0,
                    param_max: 1.0,
                });
            }
        }

        // Unsubscribe stale listeners
        for key in &self.active_listeners {
            if !needed.contains(key) {
                self.send_osc(
                    "/live/device/stop_listen/parameter/value",
                    &[
                        rosc::OscType::Int(key.0),
                        rosc::OscType::Int(key.1),
                        rosc::OscType::Int(key.2),
                    ],
                );
            }
        }

        // Subscribe new listeners
        for key in &needed {
            if !self.active_listeners.contains(key) {
                self.send_osc(
                    "/live/device/start_listen/parameter/value",
                    &[
                        rosc::OscType::Int(key.0),
                        rosc::OscType::Int(key.1),
                        rosc::OscType::Int(key.2),
                    ],
                );
            }
        }

        self.active_listeners = needed;
        self.write_targets = new_write_targets;
        // Drop filter state for sources that no longer have a write target.
        self.filtered_sources
            .retain(|key, _| self.write_targets.contains_key(key));
    }

    /// Build an `AbletonSetContext` from the current session for project storage.
    pub fn build_set_context(&self) -> AbletonSetContext {
        AbletonSetContext {
            track_signatures: self
                .session
                .tracks
                .iter()
                .map(|t| AbletonTrackSignature {
                    device_classes: t.devices.iter().map(|d| d.class_name.clone()).collect(),
                })
                .collect(),
        }
    }

    // ── Transport sync ─────────────────────────────────────────────

    /// Subscribe to transport listeners on Ableton.
    /// Call when entering AbletonOSC sync mode while connected.
    pub fn enable_transport_sync(&mut self) {
        if self.transport_enabled {
            log::info!("[AbletonBridge] Transport sync already enabled");
            return;
        }
        if self.send_socket.is_none() {
            log::warn!("[AbletonBridge] Cannot enable transport sync — no send socket");
            return;
        }
        self.transport_enabled = true;
        self.transport_sync.reset();
        self.send_osc("/live/song/start_listen/is_playing", &[]);
        self.send_osc("/live/song/start_listen/tempo", &[]);
        // Position-acknowledgment channel (design D4): song time streams at
        // Live's listener cadence (~10 Hz) and confirms play/seek commands.
        self.send_osc("/live/song/start_listen/current_song_time", &[]);
        // Seed initial state
        self.send_osc("/live/song/get/is_playing", &[]);
        self.send_osc("/live/song/get/tempo", &[]);
        self.send_osc("/live/song/get/current_song_time", &[]);
        log::info!(
            "[AbletonBridge] Transport sync enabled (closed-loop commands + song-time acks; MIDI CLK owns timing)"
        );
    }

    /// Unsubscribe transport listeners.
    /// Call when leaving AbletonOSC sync mode or disconnecting.
    pub fn disable_transport_sync(&mut self) {
        if !self.transport_enabled {
            return;
        }
        if self.send_socket.is_some() {
            self.send_osc("/live/song/stop_listen/is_playing", &[]);
            self.send_osc("/live/song/stop_listen/tempo", &[]);
            self.send_osc("/live/song/stop_listen/current_song_time", &[]);
        }
        self.transport_enabled = false;
        self.ableton_is_playing = false;
        self.transport_last_received = 0.0;
        self.transport_sync.reset();
        *self.pending_transport.lock() = PendingTransportState::default();
        log::info!("[AbletonBridge] Transport sync disabled");
    }

    pub fn is_transport_enabled(&self) -> bool {
        self.transport_enabled
    }

    /// Whether transport data is actively arriving from Ableton.
    pub fn is_transport_receiving(&self, realtime: f64) -> bool {
        self.transport_enabled
            && self.connected
            && self.transport_last_received > 0.0
            && realtime - self.transport_last_received < TRANSPORT_TIMEOUT_SECS
    }

    pub fn ableton_is_playing(&self) -> bool {
        self.ableton_is_playing
    }

    pub fn ableton_tempo(&self) -> f32 {
        self.ableton_tempo
    }

    /// Drain pending transport state from the receiver thread into the
    /// state machine. Call once per frame from `update()`.
    pub fn drain_transport(&mut self, realtime: f64) {
        if !self.transport_enabled {
            return;
        }

        let pending = {
            let mut pt = self.pending_transport.lock();
            PendingTransportState {
                is_playing: pt.is_playing.take(),
                tempo: pt.tempo.take(),
                song_time: pt.song_time.take(),
            }
        };

        if pending.is_playing.is_some()
            || pending.tempo.is_some()
            || pending.song_time.is_some()
        {
            self.transport_last_received = realtime;
        }

        // Feed observations to the state machine — ordering matters for ack
        // checks: tempo first (dead-reckoning input), then playing, then
        // position (the ack evaluation reads the full observed snapshot).
        if let Some(tempo) = pending.tempo {
            self.ableton_tempo = tempo;
            self.transport_sync.on_osc_tempo(tempo, realtime);
        }
        if let Some(playing) = pending.is_playing {
            self.ableton_is_playing = playing;
            self.transport_sync.on_osc_is_playing(playing, realtime);
        }
        if let Some(beats) = pending.song_time {
            self.transport_sync.on_osc_song_time(beats, realtime);
        }
    }

    /// Outbound transport: detect MANIFOLD transport edges, feed the state
    /// machine, pump retries/timeouts, and send whatever it emits.
    /// Called after engine tick (same timing slot as OscPositionSender::late_update).
    ///
    /// All echo/confirmation/retry decisions live in `transport_sync.rs`
    /// (docs/ABLETON_TRANSPORT_SYNC_DESIGN.md §4) — this method is I/O only.
    pub fn late_update_transport(
        &mut self,
        is_playing: bool,
        current_beat: f32,
        realtime: f64,
        clk_receiving: bool,
    ) {
        if !self.transport_enabled || self.send_socket.is_none() {
            return;
        }

        // Engine transport edges are local gestures (D12 value-matching in
        // the machine decides whether they need a command — a relayed remote
        // play converging back through here emits nothing). Seeks arrive
        // explicitly via `notify_local_seek` (D6), never inferred from beat
        // divergence.
        if is_playing != self.transport_last_was_playing {
            if is_playing {
                self.transport_sync.on_local_play(current_beat, realtime);
            } else {
                self.transport_sync.on_local_stop(realtime);
            }
            self.transport_last_was_playing = is_playing;
        } else {
            self.transport_sync
                .on_local_transport(is_playing, current_beat, realtime);
        }

        self.transport_sync.tick(realtime, clk_receiving);

        while let Some(msg) = self.transport_sync.pop_out() {
            use crate::transport_sync::OutMsg;
            match msg {
                OutMsg::StartPlaying => {
                    log::debug!("[ABL-TRANSPORT] → start_playing (beat {:.1})", current_beat);
                    self.send_osc("/live/song/start_playing", &[]);
                }
                OutMsg::StopPlaying => {
                    log::debug!("[ABL-TRANSPORT] → stop_playing");
                    self.send_osc("/live/song/stop_playing", &[]);
                }
                OutMsg::SetSongTime(beat) => {
                    log::debug!("[ABL-TRANSPORT] → set/current_song_time {:.1}", beat);
                    self.send_osc(
                        "/live/song/set/current_song_time",
                        &[rosc::OscType::Float(beat)],
                    );
                }
                OutMsg::QueryIsPlaying => {
                    self.send_osc("/live/song/get/is_playing", &[]);
                }
                OutMsg::QuerySongTime => {
                    self.send_osc("/live/song/get/current_song_time", &[]);
                }
            }
        }
    }

    /// An explicit user seek (ruler scrub, click-seek) — the command
    /// handlers call this so seeks are commanded, never inferred (D6).
    pub fn notify_local_seek(&mut self, beat: f32, playing: bool, realtime: f64) {
        if !self.transport_enabled || self.send_socket.is_none() {
            return;
        }
        self.transport_sync.on_local_seek(beat, playing, realtime);
    }

    /// True while a transport command awaits acknowledgment — the content
    /// thread gates the MIDI-clock position plane on this (D5).
    pub fn transport_sync_pending(&self) -> bool {
        self.transport_enabled && self.transport_sync.sync_pending()
    }

    /// Sync health for the UI (D9/D10).
    pub fn transport_sync_status(&self) -> crate::transport_sync::TransportSyncStatus {
        self.transport_sync.status()
    }

    /// Drain one engine intent from the machine (inbound relay / degraded
    /// position). The content thread routes these through `SyncArbiter`.
    pub fn pop_transport_action(&mut self) -> Option<crate::transport_sync::EngineAction> {
        self.transport_sync.pop_action()
    }

    // ── OSC send helper ───────────────────────────────────────────

    fn send_osc(&self, address: &str, args: &[rosc::OscType]) {
        send_osc_to(&self.send_socket, address, args);
    }
}

impl Default for AbletonBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AbletonBridge {
    fn drop(&mut self) {
        self.disconnect();
    }
}

// ── OSC argument extraction helpers ────────────���──────────────────

/// Whether the last `send_osc_to` attempt failed. Gates the warn/debug
/// throttle below — flips back to `false` (and logs a reconnect) the moment
/// a send succeeds again.
///
/// A bare `static` (not per-`AbletonBridge`-instance state) because
/// `send_osc_to` is a free function called from many sites that already
/// hold a mutable borrow of some other `AbletonBridge` field (see the
/// doc comment on the function) — threading a `&mut self` field through
/// every call site would fight those borrows for no benefit; there is one
/// bridge per process and the atomic only throttles log volume, never
/// drives behavior.
static OSC_SEND_FAILED: AtomicBool = AtomicBool::new(false);

/// What `send_osc_to` should log for this outcome, decided by
/// [`note_send_outcome`]. Split out as a pure state transition (no I/O, no
/// logging) so it can be unit-tested without touching a real socket or the
/// global log sink.
#[derive(Debug, PartialEq, Eq)]
enum SendLogAction {
    /// Nothing worth logging (steady-state success, or a failure that's
    /// already been reported and downgraded).
    Silent,
    /// First failure since the last success — log at WARN.
    WarnFirst,
    /// A repeat of an already-reported failure — log at DEBUG.
    DebugRepeat,
    /// A send just succeeded after one or more failures — log at INFO.
    InfoReconnected,
}

/// Advance the throttle state machine for one send outcome. `flag` tracks
/// "the previous attempt failed" — `swap` both reads and updates it
/// atomically in one step, so this is safe to call from a single-threaded
/// context (as `send_osc_to` is) without a lock.
fn note_send_outcome(flag: &AtomicBool, ok: bool) -> SendLogAction {
    if ok {
        if flag.swap(false, Ordering::Relaxed) {
            SendLogAction::InfoReconnected
        } else {
            SendLogAction::Silent
        }
    } else if flag.swap(true, Ordering::Relaxed) {
        SendLogAction::DebugRepeat
    } else {
        SendLogAction::WarnFirst
    }
}

/// Free function to send OSC — avoids borrow conflicts when called alongside
/// mutable borrows of other `AbletonBridge` fields.
///
/// When Ableton isn't running, `sock.send` fails with "Connection refused"
/// on every heartbeat (~1.5s) — BUG-038: this spammed WARN indefinitely.
/// Now: warn once on the first failure, downgrade repeated identical
/// failures to DEBUG, and log an INFO "reconnected" the moment a send
/// succeeds again.
fn send_osc_to(socket: &Option<UdpSocket>, address: &str, args: &[rosc::OscType]) {
    if let Some(sock) = socket {
        let msg = rosc::OscMessage {
            addr: address.to_string(),
            args: args.to_vec(),
        };
        let packet = rosc::OscPacket::Message(msg);
        match rosc::encoder::encode(&packet) {
            Ok(buf) => match sock.send(&buf) {
                Ok(_) => {
                    if note_send_outcome(&OSC_SEND_FAILED, true) == SendLogAction::InfoReconnected
                    {
                        log::info!("[AbletonBridge] OSC send reconnected");
                    }
                }
                Err(e) => match note_send_outcome(&OSC_SEND_FAILED, false) {
                    SendLogAction::DebugRepeat => {
                        log::debug!("[AbletonBridge] OSC send failed for {address}: {e}");
                    }
                    _ => {
                        log::warn!("[AbletonBridge] OSC send failed for {address}: {e}");
                    }
                },
            },
            Err(e) => {
                log::error!("[AbletonBridge] OSC encode error: {e:?}");
            }
        }
    }
}

/// Strip a leading "N " or "N. " prefix from an Ableton track name so the
/// PLAY group lookup matches both "PLAY" and "1 PLAY".
fn normalize_group_name(s: &str) -> String {
    let mut chars = s.chars().peekable();
    let mut consumed_digit = false;
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            chars.next();
            consumed_digit = true;
        } else {
            break;
        }
    }
    if !consumed_digit {
        return s.trim().to_string();
    }
    // Optional separator after the digits.
    if let Some(&c) = chars.peek()
        && (c == '.' || c == ':' || c == '-')
    {
        chars.next();
    }
    chars.collect::<String>().trim().to_string()
}

fn osc_arg_int(arg: &rosc::OscType) -> Option<i32> {
    match arg {
        rosc::OscType::Int(i) => Some(*i),
        rosc::OscType::Float(f) => Some(*f as i32),
        rosc::OscType::Bool(b) => Some(if *b { 1 } else { 0 }),
        _ => None,
    }
}

fn osc_arg_float(arg: &rosc::OscType) -> Option<f32> {
    match arg {
        rosc::OscType::Float(f) => Some(*f),
        rosc::OscType::Int(i) => Some(*i as f32),
        rosc::OscType::Double(d) => Some(*d as f32),
        _ => None,
    }
}

fn osc_arg_string(arg: &rosc::OscType) -> Option<String> {
    match arg {
        rosc::OscType::String(s) => Some(s.clone()),
        _ => None,
    }
}

// ── Zero-alloc fast-path decoder for parameter values ────────────

/// OSC address for parameter value listener updates.
/// 32 chars + null = 33 bytes. We match these 33 bytes; the 3 pad bytes
/// after the null are always zero but we don't need to check them.
const PARAM_VALUE_ADDR: &[u8] = b"/live/device/get/parameter/value\0";
/// Byte offset where the type tag starts: 33 bytes padded to 36.
const PARAM_VALUE_TAG_OFFSET: usize = 36;
/// Byte offset where arguments start (after address + padded type tag).
/// Type tag is `,iiif\0` or `,iiii\0` = 6 bytes, padded to 8.
const PARAM_VALUE_ARGS_OFFSET: usize = 44;
/// Minimum packet size: address(36) + type_tag(8) + 4 args × 4 bytes(16) = 60.
const PARAM_VALUE_MIN_SIZE: usize = 60;

/// Try to parse a `/live/device/get/parameter/value` message directly from
/// raw UDP bytes without any heap allocation. Returns `(track_id, device_id,
/// param_id, value)` on success, or `None` to fall through to `rosc`.
fn try_parse_param_value_fast(buf: &[u8]) -> Option<(i32, i32, i32, f32)> {
    if buf.len() < PARAM_VALUE_MIN_SIZE {
        return None;
    }
    // Check address prefix (including null terminator).
    if &buf[..PARAM_VALUE_ADDR.len()] != PARAM_VALUE_ADDR {
        return None;
    }
    // Verify type tag starts with ','.
    if buf[PARAM_VALUE_TAG_OFFSET] != b',' {
        return None;
    }

    let args = PARAM_VALUE_ARGS_OFFSET;
    let track_id = i32::from_be_bytes([buf[args], buf[args + 1], buf[args + 2], buf[args + 3]]);
    let device_id =
        i32::from_be_bytes([buf[args + 4], buf[args + 5], buf[args + 6], buf[args + 7]]);
    let param_id =
        i32::from_be_bytes([buf[args + 8], buf[args + 9], buf[args + 10], buf[args + 11]]);

    // The value arg may be float ('f') or int ('i') depending on the parameter.
    let value_type = buf[PARAM_VALUE_TAG_OFFSET + 4]; // 4th type char after ','
    let value_bytes = [
        buf[args + 12],
        buf[args + 13],
        buf[args + 14],
        buf[args + 15],
    ];
    let value = match value_type {
        b'f' => f32::from_be_bytes(value_bytes),
        b'i' => i32::from_be_bytes(value_bytes) as f32,
        b'd' if buf.len() >= PARAM_VALUE_ARGS_OFFSET + 20 => {
            // Double is 8 bytes — need 4 more bytes than the minimum size.
            let d_bytes: [u8; 8] = buf[args + 12..args + 20].try_into().ok()?;
            f64::from_be_bytes(d_bytes) as f32
        }
        _ => return None, // Unknown type — fall through to rosc
    };

    Some((track_id, device_id, param_id, value))
}

// ── Tests ─────────────────────────────���───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_rack_device_detection() {
        assert!(is_rack_device("InstrumentGroupDevice"));
        assert!(is_rack_device("DrumGroupDevice"));
        assert!(is_rack_device("AudioEffectGroupDevice"));
        assert!(is_rack_device("MidiEffectGroupDevice"));
        assert!(!is_rack_device("OriginalSimpler"));
        assert!(!is_rack_device("Compressor2"));
    }

    // ── Resolver fixtures ──────────────────────────────────────────

    fn macro_(param_id: i32, name: &str) -> AbletonMacro {
        AbletonMacro {
            param_id,
            name: name.to_string(),
            value: 0.0,
            min: 0.0,
            max: 1.0,
        }
    }

    fn device_(
        device_id: i32,
        name: &str,
        class: &str,
        macros: Vec<AbletonMacro>,
    ) -> AbletonDevice {
        AbletonDevice {
            device_id,
            name: name.to_string(),
            class_name: class.to_string(),
            macros,
        }
    }

    fn track_(track_id: i32, name: &str, devices: Vec<AbletonDevice>) -> AbletonTrack {
        AbletonTrack {
            track_id,
            name: name.to_string(),
            devices,
        }
    }

    fn bridge_with_session(tracks: Vec<AbletonTrack>) -> AbletonBridge {
        let mut b = AbletonBridge::new();
        b.session.tracks = tracks;
        b.session.connected = true;
        b
    }

    fn mapping_(
        track_id: i32,
        device_id: i32,
        param_id: i32,
        track_name: &str,
        device_name: &str,
        macro_name: &str,
        class: &str,
    ) -> AbletonParamMapping {
        use manifold_core::ableton_mapping::{AbletonDeviceIdentity, AbletonMacroAddress};
        AbletonParamMapping {
            param_id: std::borrow::Cow::Borrowed("amount"),
            address: AbletonMacroAddress {
                track_id,
                device_id,
                param_id,
                device_identity: AbletonDeviceIdentity {
                    device_class_name: class.to_string(),
                },
                track_name: track_name.to_string(),
                device_name: device_name.to_string(),
                macro_name: macro_name.to_string(),
            },
            range_min: 0.0,
            range_max: 1.0,
            inverted: false,
            legacy_param_index: None,
            last_value: 0.0,
            status: AbletonMappingStatus::Dormant,
        }
    }

    // ── P4: manifest-id resolution fixtures ────────────────────────

    fn user_spec(id: &str) -> manifold_core::effect_graph_def::ParamSpecDef {
        manifold_core::effect_graph_def::ParamSpecDef {
            id: id.to_string(),
            name: id.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: manifold_core::macro_bank::MacroCurve::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
        }
    }

    fn active_mapping_for(
        param_id: &str,
        track_id: i32,
        device_id: i32,
        aparam: i32,
    ) -> AbletonParamMapping {
        use manifold_core::ableton_mapping::{AbletonDeviceIdentity, AbletonMacroAddress};
        AbletonParamMapping {
            param_id: std::borrow::Cow::Owned(param_id.to_string()),
            address: AbletonMacroAddress {
                track_id,
                device_id,
                param_id: aparam,
                device_identity: AbletonDeviceIdentity {
                    device_class_name: "InstrumentGroupDevice".to_string(),
                },
                track_name: "T".to_string(),
                device_name: "D".to_string(),
                macro_name: "M".to_string(),
            },
            range_min: 0.0,
            range_max: 1.0,
            inverted: false,
            legacy_param_index: None,
            last_value: 0.0,
            status: AbletonMappingStatus::Active,
        }
    }

    /// REPRO — design acceptance (a): an Ableton mapping on a user-added param
    /// must resolve to a write target. Before P4 the resolver looked the id up
    /// in the frozen registry's `id_to_index` and `continue`d on miss, silently
    /// dropping the mapping — the glb-generator "unmappable slider" bug.
    /// (Runnable-red against pre-P4 code.)
    #[test]
    fn ableton_rebuild_creates_write_target_for_user_param() {
        use manifold_core::effects::PresetInstance;
        use manifold_core::params::{Param, ParamManifest};

        let mut project = manifold_core::project::Project::default();
        let mut fx = PresetInstance::new(manifold_core::PresetTypeId::BLOOM);
        fx.params = ParamManifest::from_params(vec![Param::user_added(user_spec("user_glow"))]);
        fx.ableton_mappings = Some(vec![active_mapping_for("user_glow", 3, 1, 2)]);
        project.settings.master_effects.push(fx);

        let mut bridge = AbletonBridge::new();
        bridge.rebuild_listeners(&project);

        assert!(
            bridge.write_targets.contains_key(&(3, 1, 2)),
            "user-added param mapping must produce a write target; keys = {:?}",
            bridge.write_targets.keys().collect::<Vec<_>>()
        );

        // ...and the write reaches the user param through the id funnel.
        let wt = &bridge.write_targets[&(3, 1, 2)][0];
        assert_eq!(wt.param_id, "user_glow");
        AbletonBridge::write_to_project(&mut project, &wt.target, &wt.param_id, 0.42);
        let v = project.settings.master_effects[0]
            .params
            .get("user_glow")
            .unwrap()
            .value;
        assert!((v - 0.42).abs() < 1e-6, "write must reach the user param; got {v}");
    }

    // ── Resolver: canonical name path ──────────────────────────────

    #[test]
    fn resolver_finds_by_name_when_ids_have_shifted() {
        // Original: track_id=2, device_id=1. After Ableton edit, the
        // same rack lives at track_id=5, device_id=3. Resolver should
        // find it via name and update the IDs.
        let bridge = bridge_with_session(vec![
            track_(0, "Bass", vec![]), // unrelated
            track_(
                5,
                "Lead Synth",
                vec![
                    device_(0, "Compressor", "Compressor2", vec![]),
                    device_(
                        3,
                        "SERUM CHORDS",
                        "InstrumentGroupDevice",
                        vec![macro_(1, "LFO (X)"), macro_(2, "DETUNE (Y)")],
                    ),
                ],
            ),
        ]);
        let mut m = mapping_(
            2,
            1,
            1,
            "Lead Synth",
            "SERUM CHORDS",
            "LFO (X)",
            "InstrumentGroupDevice",
        );
        let s = bridge.validate_single_mapping(&mut m);
        assert_eq!(s, AbletonMappingStatus::Active);
        assert_eq!(m.address.track_id, 5);
        assert_eq!(m.address.device_id, 3);
        assert_eq!(m.address.param_id, 1);
        assert_eq!(m.address.macro_name, "LFO (X)");
    }

    #[test]
    fn resolver_does_not_silently_grab_wrong_rack_at_same_id() {
        // The bug from the user's project: a totally different rack now
        // lives at the stored numeric ID. Old resolver passed the
        // class_name check and silently rebound. New resolver uses
        // names, so the wrong-rack-at-same-id is rejected and we fall
        // through to fuzzy search (which here finds the right one).
        let bridge = bridge_with_session(vec![
            track_(
                5,
                "Some Other Track",
                vec![
                    // Same class as the original, same numeric IDs, but wrong
                    // names. This is what the broken resolver was matching.
                    device_(
                        2,
                        "Wrong Rack",
                        "InstrumentGroupDevice",
                        vec![macro_(1, "Macro 1")],
                    ),
                ],
            ),
            track_(
                7,
                "Lead Synth",
                vec![device_(
                    0,
                    "SERUM CHORDS",
                    "InstrumentGroupDevice",
                    vec![macro_(1, "LFO (X)")],
                )],
            ),
        ]);
        let mut m = mapping_(
            5,
            2,
            1,
            "Lead Synth",
            "SERUM CHORDS",
            "LFO (X)",
            "InstrumentGroupDevice",
        );
        let s = bridge.validate_single_mapping(&mut m);
        assert_eq!(s, AbletonMappingStatus::Active);
        assert_eq!(m.address.track_id, 7);
        assert_eq!(m.address.device_id, 0);
        assert_eq!(m.address.macro_name, "LFO (X)");
    }

    #[test]
    fn resolver_handles_macro_reorder_within_rack() {
        // User dragged macros around in the rack. Slot index changed but
        // the macro name is still findable in the same rack.
        let bridge = bridge_with_session(vec![track_(
            5,
            "Lead Synth",
            vec![device_(
                0,
                "SERUM CHORDS",
                "InstrumentGroupDevice",
                vec![
                    // "LFO (X)" used to be slot 1, now it's slot 4
                    macro_(1, "DETUNE (Y)"),
                    macro_(2, "PHASE"),
                    macro_(3, "DELAY"),
                    macro_(4, "LFO (X)"),
                ],
            )],
        )]);
        let mut m = mapping_(
            5,
            0,
            1,
            "Lead Synth",
            "SERUM CHORDS",
            "LFO (X)",
            "InstrumentGroupDevice",
        );
        let s = bridge.validate_single_mapping(&mut m);
        assert_eq!(s, AbletonMappingStatus::Active);
        assert_eq!(m.address.param_id, 4); // slot updated
        assert_eq!(m.address.macro_name, "LFO (X)");
    }

    #[test]
    fn resolver_handles_macro_rename_in_place() {
        // Macro at the stored slot was renamed. Original name no longer
        // appears in the rack at all. Resolver trusts the slot and
        // updates the cached name.
        let bridge = bridge_with_session(vec![track_(
            5,
            "Lead Synth",
            vec![device_(
                0,
                "SERUM CHORDS",
                "InstrumentGroupDevice",
                vec![
                    macro_(1, "OSC LFO"), // was "LFO (X)"
                ],
            )],
        )]);
        let mut m = mapping_(
            5,
            0,
            1,
            "Lead Synth",
            "SERUM CHORDS",
            "LFO (X)",
            "InstrumentGroupDevice",
        );
        let s = bridge.validate_single_mapping(&mut m);
        assert_eq!(s, AbletonMappingStatus::Active);
        assert_eq!(m.address.param_id, 1); // slot unchanged
        assert_eq!(m.address.macro_name, "OSC LFO"); // name updated
    }

    #[test]
    fn resolver_legacy_backfills_names_from_ids() {
        // Project saved before name-resolution existed: names are empty.
        // Resolver falls back to numeric IDs + class match and writes
        // the names so the next save migrates the mapping forward.
        let bridge = bridge_with_session(vec![track_(
            5,
            "Lead Synth",
            vec![device_(
                2,
                "SERUM CHORDS",
                "InstrumentGroupDevice",
                vec![macro_(1, "LFO (X)")],
            )],
        )]);
        let mut m = mapping_(5, 2, 1, "", "", "", "InstrumentGroupDevice");
        let s = bridge.validate_single_mapping(&mut m);
        assert_eq!(s, AbletonMappingStatus::Active);
        assert_eq!(m.address.track_name, "Lead Synth");
        assert_eq!(m.address.device_name, "SERUM CHORDS");
        assert_eq!(m.address.macro_name, "LFO (X)");
    }

    #[test]
    fn resolver_dormant_when_track_renamed_and_class_ambiguous() {
        // User renamed the track. Names don't match anymore. Multiple
        // racks of the same class exist, so the fuzzy search is
        // ambiguous → mapping is marked Ambiguous (not silently rebound).
        let bridge = bridge_with_session(vec![
            track_(
                0,
                "Lead (renamed)",
                vec![device_(
                    0,
                    "Rack A",
                    "InstrumentGroupDevice",
                    vec![macro_(1, "X")],
                )],
            ),
            track_(
                1,
                "Bass",
                vec![device_(
                    0,
                    "Rack B",
                    "InstrumentGroupDevice",
                    vec![macro_(1, "Y")],
                )],
            ),
        ]);
        let mut m = mapping_(
            0,
            0,
            1,
            "Lead Synth",
            "SERUM CHORDS",
            "LFO (X)",
            "InstrumentGroupDevice",
        );
        let s = bridge.validate_single_mapping(&mut m);
        assert_eq!(s, AbletonMappingStatus::Ambiguous);
    }

    #[test]
    fn resolver_dormant_when_no_match_at_all() {
        // Mapping references a class that doesn't exist in the session.
        let bridge = bridge_with_session(vec![track_(
            0,
            "Bass",
            vec![device_(0, "Compressor", "Compressor2", vec![])],
        )]);
        let mut m = mapping_(
            0,
            0,
            1,
            "Lead Synth",
            "SERUM CHORDS",
            "LFO (X)",
            "InstrumentGroupDevice",
        );
        let s = bridge.validate_single_mapping(&mut m);
        assert_eq!(s, AbletonMappingStatus::Dormant);
    }

    #[test]
    fn bridge_default_state() {
        let bridge = AbletonBridge::new();
        assert!(!bridge.is_connected());
        assert!(bridge.session().tracks.is_empty());
    }

    #[test]
    fn osc_arg_extraction() {
        assert_eq!(osc_arg_int(&rosc::OscType::Int(42)), Some(42));
        assert_eq!(osc_arg_int(&rosc::OscType::Float(3.7)), Some(3));
        assert_eq!(osc_arg_float(&rosc::OscType::Float(1.5)), Some(1.5));
        assert_eq!(osc_arg_float(&rosc::OscType::Int(2)), Some(2.0));
        assert_eq!(
            osc_arg_string(&rosc::OscType::String("hello".to_string())),
            Some("hello".to_string())
        );
        assert_eq!(osc_arg_string(&rosc::OscType::Int(0)), None);
    }

    // Echo handling is value-matched in the transport state machine now —
    // covered by transport_sync::tests (T5/T6/T9) and the
    // ableton_transport_sync integration harness (F3), not by a
    // wall-clock-window test here.

    #[test]
    fn write_target_value_mapping() {
        let wt = WriteTarget {
            target: AbletonMappingTarget::MasterEffect {
                effect_type: manifold_core::PresetTypeId::BLOOM,
                param_id: std::borrow::Cow::Borrowed("amount"),
            },
            param_id: "amount".to_string(),
            // Rack macros send 0-127 from Ableton
            ableton_min: 0.0,
            ableton_max: 127.0,
            range_min: 0.25,
            range_max: 0.75,
            inverted: false,
            param_min: 0.0,
            param_max: 2.0,
        };
        // Ableton sends 63.5 (50% of 0-127)
        // → normalized = 0.5
        // → mapped = 0.25 + 0.5 * 0.5 = 0.5
        // → value = 0 + 2 * 0.5 = 1.0
        let ableton_val = 63.5_f32;
        let span = wt.ableton_max - wt.ableton_min;
        let normalized = ((ableton_val - wt.ableton_min) / span).clamp(0.0, 1.0);
        let mapped = wt.range_min + (wt.range_max - wt.range_min) * normalized;
        let value = wt.param_min + (wt.param_max - wt.param_min) * mapped;
        assert!((normalized - 0.5).abs() < 0.01);
        assert!((mapped - 0.5).abs() < 0.01);
        assert!((value - 1.0).abs() < 0.01);
    }

    // ── Fast-path decoder tests ──────────────────────────────────

    /// Build a raw OSC packet for `/live/device/get/parameter/value`
    /// with args (int, int, int, float).
    fn build_param_value_packet(
        track_id: i32,
        device_id: i32,
        param_id: i32,
        value: f32,
    ) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64);
        // Address: 32 chars + null = 33 bytes, padded to 36
        buf.extend_from_slice(b"/live/device/get/parameter/value\0\0\0\0");
        // Type tag: ",iiif\0" padded to 8 bytes
        buf.extend_from_slice(b",iiif\0\0\0");
        // Arguments: 3 ints + 1 float, big-endian
        buf.extend_from_slice(&track_id.to_be_bytes());
        buf.extend_from_slice(&device_id.to_be_bytes());
        buf.extend_from_slice(&param_id.to_be_bytes());
        buf.extend_from_slice(&value.to_be_bytes());
        assert_eq!(buf.len(), 60); // sanity check
        buf
    }

    #[test]
    fn fast_parse_param_value_float() {
        let packet = build_param_value_packet(3, 1, 5, 0.75);
        let result = try_parse_param_value_fast(&packet);
        assert_eq!(result, Some((3, 1, 5, 0.75)));
    }

    #[test]
    fn fast_parse_param_value_int_arg() {
        let mut buf = Vec::with_capacity(64);
        // Address: 32 chars + null = 33 bytes, padded to 36
        buf.extend_from_slice(b"/live/device/get/parameter/value\0\0\0\0");
        // Type tag with int value: ",iiii\0" padded to 8
        buf.extend_from_slice(b",iiii\0\0\0");
        buf.extend_from_slice(&2_i32.to_be_bytes());
        buf.extend_from_slice(&0_i32.to_be_bytes());
        buf.extend_from_slice(&3_i32.to_be_bytes());
        buf.extend_from_slice(&64_i32.to_be_bytes()); // int 64 → 64.0
        let result = try_parse_param_value_fast(&buf);
        assert_eq!(result, Some((2, 0, 3, 64.0)));
    }

    #[test]
    fn fast_parse_rejects_wrong_address() {
        let mut buf = Vec::with_capacity(64);
        // Different address (track name)
        buf.extend_from_slice(b"/live/track/get/name\0\0\0\0");
        buf.extend_from_slice(b",is\0");
        buf.extend_from_slice(&0_i32.to_be_bytes());
        buf.extend_from_slice(b"Bass\0\0\0\0");
        assert_eq!(try_parse_param_value_fast(&buf), None);
    }

    #[test]
    fn fast_parse_rejects_short_packet() {
        let buf = b"/live/device/get/parameter/value\0,iiif\0\0\0";
        // Only 44 bytes — missing args
        assert_eq!(try_parse_param_value_fast(buf), None);
    }

    #[test]
    fn fast_parse_matches_rosc_decode() {
        // Verify fast path produces the same result as rosc for the same packet.
        let packet = build_param_value_packet(5, 2, 7, 0.333);

        // Fast path
        let fast = try_parse_param_value_fast(&packet).unwrap();

        // rosc path
        let (_, osc_packet) = rosc::decoder::decode_udp(&packet).unwrap();
        let msg = match osc_packet {
            rosc::OscPacket::Message(m) => m,
            _ => panic!("expected message"),
        };
        let rosc_tid = osc_arg_int(&msg.args[0]).unwrap();
        let rosc_did = osc_arg_int(&msg.args[1]).unwrap();
        let rosc_pid = osc_arg_int(&msg.args[2]).unwrap();
        let rosc_val = osc_arg_float(&msg.args[3]).unwrap();

        assert_eq!(fast.0, rosc_tid);
        assert_eq!(fast.1, rosc_did);
        assert_eq!(fast.2, rosc_pid);
        assert!((fast.3 - rosc_val).abs() < f32::EPSILON);
    }

    // BUG-038: repeated identical OSC-send failures (Ableton not running)
    // must warn once, then downgrade to DEBUG until a send succeeds again,
    // at which point a single INFO reconnect log fires. Uses a local
    // AtomicBool (not the module `OSC_SEND_FAILED` static) so this test is
    // isolated from others running in parallel.
    #[test]
    fn osc_send_throttle_warns_once_then_downgrades_then_reconnects() {
        let flag = AtomicBool::new(false);

        // First failure — WARN.
        assert_eq!(note_send_outcome(&flag, false), SendLogAction::WarnFirst);
        // Repeated failures while still down — DEBUG, not WARN again.
        assert_eq!(note_send_outcome(&flag, false), SendLogAction::DebugRepeat);
        assert_eq!(note_send_outcome(&flag, false), SendLogAction::DebugRepeat);

        // A send succeeds — one INFO reconnect log.
        assert_eq!(
            note_send_outcome(&flag, true),
            SendLogAction::InfoReconnected
        );
        // Steady-state success afterward is silent.
        assert_eq!(note_send_outcome(&flag, true), SendLogAction::Silent);

        // A fresh failure after recovering warns again (not DEBUG).
        assert_eq!(note_send_outcome(&flag, false), SendLogAction::WarnFirst);
    }
}
