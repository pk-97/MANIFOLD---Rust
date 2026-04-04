//! Ableton Live OSC bridge.
//!
//! Discovers Ableton's session via AbletonOSC (port 11000/11001), subscribes
//! to rack macro parameter changes, and writes incoming values to MANIFOLD
//! parameters in replace mode.
//!
//! Design philosophy: MANIFOLD is the active party — it reaches into Ableton,
//! reads the session, and pulls what it needs. Ableton stays untouched.

use ahash::{AHashMap, AHashSet};
use manifold_core::Seconds;
use manifold_core::ableton_mapping::{
    AbletonMappingStatus, AbletonMappingTarget, AbletonParamMapping, AbletonSetContext,
    AbletonTrackSignature,
};
use manifold_core::project::Project;
use crate::sync::SyncArbiter;
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

/// State machine retry timeout — resend command if no confirmation after this.
const SM_RETRY_TIMEOUT_SECS: f64 = 0.050;
/// Maximum retries before giving up on a transport/seek command.
const SM_MAX_RETRIES: u8 = 3;
/// Beat tolerance for seek confirmation (position response within this = confirmed).
const SEEK_CONFIRM_TOLERANCE: f32 = 0.5;
/// Interval for position monitor polling (Ableton-initiated seek detection).
const POSITION_POLL_INTERVAL_SECS: f64 = 0.250;
/// Beat delta threshold for position monitor to trigger an inbound seek relay.
const POSITION_RELAY_THRESHOLD: f32 = 0.5;

// ── Session model (runtime only) ──────────────────────────────────

/// Discovered Ableton Live session state.
#[derive(Debug, Clone, Default)]
pub struct AbletonSession {
    pub connected: bool,
    pub set_file_path: String,
    pub tracks: Vec<AbletonTrack>,
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

// ── Pending value from Ableton ────────────────────────────────────

struct AbletonPendingValue {
    track_id: i32,
    device_id: i32,
    param_id: i32,
    value: f32,
}

// ── Pending transport state from Ableton ──────────────────────────

/// Transport values received from AbletonOSC listener updates.
/// Written by the background receiver thread, drained on the content thread.
#[derive(Default)]
struct PendingTransportState {
    is_playing: Option<bool>,
    tempo: Option<f32>,
    current_song_time: Option<f32>,
}

// ── Transport state machine types ────────────────────────────────

/// Actions the content thread should apply to the engine after
/// `process_transport()`. No heap allocation — at most one play/pause
/// and one seek per frame.
pub struct TransportActions {
    pub play: bool,
    pub pause: bool,
    pub seek_beat: Option<f32>,
}

impl TransportActions {
    fn none() -> Self {
        Self { play: false, pause: false, seek_beat: None }
    }
}

/// Transport (play/stop) state machine.
/// Tracks pending outbound commands and waits for confirmation from Ableton.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TransportCommand {
    Idle,
    PlaySent {
        sent_at: f64,
        retries: u8,
        /// Beat position sent alongside play (for retries).
        beat: f32,
    },
    StopSent {
        sent_at: f64,
        retries: u8,
        /// Beat position sent alongside stop (to preserve position on retries).
        beat: f32,
    },
}

/// Seek (position) state machine.
/// Tracks pending outbound seek commands and waits for position confirmation.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SeekCommand {
    Idle,
    Sent {
        target_beat: f32,
        sent_at: f64,
        retries: u8,
    },
}

// ── Write target (pre-built lookup for hot path) ──────────────────

#[derive(Debug, Clone)]
struct WriteTarget {
    target: AbletonMappingTarget,
    param_index: usize,
    /// Ableton parameter range — the raw value arrives in [ableton_min, ableton_max]
    /// and must be normalized to 0-1 before applying range_min/range_max trim.
    ableton_min: f32,
    ableton_max: f32,
    /// User trim handles (0-1 normalized, applied after Ableton normalization).
    range_min: f32,
    range_max: f32,
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
    WaitingTrackCount {
        started: f64,
    },
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
    pending_values: Arc<Mutex<Vec<AbletonPendingValue>>>,

    // ── Pre-built fast lookup: (track_id, device_id, param_id) → write targets
    write_targets: AHashMap<(i32, i32, i32), Vec<WriteTarget>>,

    // ── Active listener subscriptions
    active_listeners: AHashSet<(i32, i32, i32)>,

    // ── Dispatch buffer (avoid alloc per frame)
    dispatch_buffer: Vec<OscMessage>,

    // ── Dirty flags for content thread
    /// Set when discovery completes so the content thread knows to call
    /// `validate_mappings` + `rebuild_listeners` and force a project snapshot.
    validation_dirty: bool,

    // ── Transport sync (state machine) ─────────────────────────────
    /// Pending transport state from receiver thread.
    pending_transport: Arc<Mutex<PendingTransportState>>,
    /// Whether transport listeners are subscribed.
    transport_enabled: bool,
    /// Current is_playing state from Ableton (updated each frame by drain).
    ableton_is_playing: bool,
    /// Previous frame's Ableton is_playing — for edge detection.
    last_ableton_is_playing: bool,
    /// Latest tempo from Ableton.
    ableton_tempo: f32,
    /// Latest position from Ableton (beats), from position polls.
    ableton_position_beats: f32,
    /// Wall-clock time of last transport message from Ableton.
    transport_last_received: f64,
    /// Transport (play/stop) state machine.
    transport_sm: TransportCommand,
    /// Seek (position) state machine.
    seek_sm: SeekCommand,
    /// What the SM expects MANIFOLD's play state to be.
    /// Updated on relay (external→engine) and on outbound detection.
    /// Prevents echoing relayed events back as outbound commands.
    last_known_manifold_playing: bool,
    /// Wall-clock time of last position monitor poll.
    last_position_poll_time: f64,
}

impl AbletonBridge {
    pub fn new() -> Self {
        Self {
            send_socket: None,
            recv_socket: None,
            recv_thread: None,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            message_queue: Arc::new(Mutex::new(AbletonMessageQueue::default())),
            session: AbletonSession::default(),
            session_version: 0,
            prev_session_version: 0,
            discovery_state: DiscoveryState::Idle,
            connected: false,
            last_heartbeat: 0.0,
            last_response: 0.0,
            pending_values: Arc::new(Mutex::new(Vec::new())),
            write_targets: AHashMap::new(),
            active_listeners: AHashSet::new(),
            dispatch_buffer: Vec::new(),
            validation_dirty: false,
            pending_transport: Arc::new(Mutex::new(PendingTransportState::default())),
            transport_enabled: false,
            ableton_is_playing: false,
            last_ableton_is_playing: false,
            ableton_tempo: 120.0,
            ableton_position_beats: 0.0,
            transport_last_received: 0.0,
            transport_sm: TransportCommand::Idle,
            seek_sm: SeekCommand::Idle,
            last_known_manifold_playing: false,
            last_position_poll_time: 0.0,
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
                if let Err(e) =
                    sock.set_read_timeout(Some(std::time::Duration::from_millis(100)))
                {
                    log::error!("[AbletonBridge] Failed to set recv timeout: {e}");
                    return;
                }
                let queue = Arc::clone(&self.message_queue);
                let shutdown = Arc::clone(&self.shutdown_flag);
                self.shutdown_flag.store(false, Ordering::Relaxed);

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
                        match rosc::decoder::decode_udp(&buf[..size]) {
                            Ok((_, packet)) => {
                                Self::handle_packet_static(packet, &queue);
                            }
                            Err(e) => {
                                log::error!("[AbletonBridge] OSC decode error: {e}");
                            }
                        }
                    }
                });
                self.recv_thread = Some(handle);
            }
            Err(e) => {
                log::error!(
                    "[AbletonBridge] Failed to bind recv socket on {recv_addr}: {e}"
                );
                self.send_socket = None;
                return;
            }
        }

        log::info!(
            "[AbletonBridge] Connected — send:{ABLETON_SEND_PORT} recv:{ABLETON_RECV_PORT}"
        );
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
        self.message_queue.lock().messages.clear();
        self.pending_values.lock().clear();
        // Clear transport state — reset SMs to Idle
        self.transport_enabled = false;
        self.ableton_is_playing = false;
        self.last_ableton_is_playing = false;
        self.ableton_tempo = 120.0;
        self.ableton_position_beats = 0.0;
        self.transport_last_received = 0.0;
        self.transport_sm = TransportCommand::Idle;
        self.seek_sm = SeekCommand::Idle;
        self.last_known_manifold_playing = false;
        self.last_position_poll_time = 0.0;
        *self.pending_transport.lock() = PendingTransportState::default();
        eprintln!("[ABL-TRANSPORT] Disconnected, all SMs → Idle");
        log::info!("[AbletonBridge] Disconnected");
    }

    fn handle_packet_static(
        packet: rosc::OscPacket,
        queue: &Arc<Mutex<AbletonMessageQueue>>,
    ) {
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

        // Drain received messages
        {
            let mut q = self.message_queue.lock();
            self.dispatch_buffer.append(&mut q.messages);
        }

        // Process messages
        for i in 0..self.dispatch_buffer.len() {
            let msg = self.dispatch_buffer[i].clone();
            self.handle_message(&msg, realtime);
        }
        self.dispatch_buffer.clear();

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
    }

    /// Apply pending Ableton values to project parameters.
    /// Returns `true` if any values were written (so the content thread can
    /// flag `modulation_active` and send a UI update this frame).
    pub fn apply(&self, project: &mut Project) -> bool {
        let mut pending = self.pending_values.lock();
        if pending.is_empty() {
            return false;
        }

        for pv in pending.drain(..) {
            let key = (pv.track_id, pv.device_id, pv.param_id);
            if let Some(targets) = self.write_targets.get(&key) {
                for wt in targets {
                    // Normalize Ableton raw value into 0-1.
                    let span = wt.ableton_max - wt.ableton_min;
                    let normalized = if span > f32::EPSILON {
                        ((pv.value - wt.ableton_min) / span).clamp(0.0, 1.0)
                    } else {
                        pv.value.clamp(0.0, 1.0)
                    };
                    // Apply user trim handles.
                    let mapped =
                        wt.range_min + (wt.range_max - wt.range_min) * normalized;
                    // Map into MANIFOLD parameter range.
                    let value = wt.param_min + (wt.param_max - wt.param_min) * mapped;
                    Self::write_to_project(project, &wt.target, wt.param_index, value);
                }
            }
        }
        true
    }

    fn write_to_project(
        project: &mut Project,
        target: &AbletonMappingTarget,
        param_index: usize,
        value: f32,
    ) {
        match target {
            AbletonMappingTarget::MasterEffect {
                effect_type,
                param_index: _,
            } => {
                if let Some(fx) = project
                    .settings
                    .master_effects
                    .iter_mut()
                    .find(|f| f.effect_type() == effect_type)
                {
                    fx.set_base_param(param_index, value);
                }
            }
            AbletonMappingTarget::LayerEffect {
                layer_id,
                effect_type,
                param_index: _,
            } => {
                if let Some((_, layer)) =
                    project.timeline.find_layer_by_id_mut(layer_id.as_str())
                    && let Some(effects) = &mut layer.effects
                    && let Some(fx) = effects
                        .iter_mut()
                        .find(|f| f.effect_type() == effect_type)
                {
                    fx.set_base_param(param_index, value);
                }
            }
            AbletonMappingTarget::GenParam {
                layer_id,
                param_index: _,
            } => {
                if let Some((_, layer)) =
                    project.timeline.find_layer_by_id_mut(layer_id.as_str())
                    && let Some(gp) = layer.gen_params_mut()
                {
                    gp.set_param_base(param_index, value);
                }
            }
            AbletonMappingTarget::MacroSlot { slot_index } => {
                manifold_core::macro_bank::MacroBank::apply_macro(
                    project, *slot_index, value,
                );
            }
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
            // Debug: log any is_playing or transport-related messages
            if addr.contains("is_playing") || addr.contains("start_playing")
                || addr.contains("stop_playing")
            {
                eprintln!(
                    "[ABL-DEBUG] Received transport msg: addr={} args={:?}",
                    addr, msg.args
                );
            }
            match addr {
                "/live/song/get/is_playing" => {
                    if let Some(val) = msg.args.first().and_then(osc_arg_int) {
                        self.pending_transport.lock().is_playing = Some(val != 0);
                        eprintln!(
                            "[ABL-DEBUG] is_playing routed: val={}",
                            val != 0
                        );
                    } else {
                        eprintln!(
                            "[ABL-DEBUG] is_playing arg parse FAILED: {:?}",
                            msg.args
                        );
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
                        self.pending_transport.lock().current_song_time = Some(val);
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
            "/live/error" => {
                // Ableton reported an error — ignore silently
                let _ = &msg.args;
            }
            _ => {}
        }
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
            self.pending_values.lock().push(AbletonPendingValue {
                track_id,
                device_id,
                param_id,
                value,
            });
        }
    }

    fn handle_track_count(&mut self, msg: &OscMessage, realtime: f64) {
        let count = msg.args.first().and_then(osc_arg_int).unwrap_or(0);

        // Check if track count changed (re-discovery needed)
        if matches!(self.discovery_state, DiscoveryState::Idle | DiscoveryState::Complete)
            && self.session.tracks.len() as i32 != count
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

        let track_id =
            msg.args.first().and_then(osc_arg_int).unwrap_or(*next_track);
        let name = msg
            .args
            .get(1)
            .and_then(osc_arg_string)
            .unwrap_or_default();

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
            let pending: Vec<(i32, String)> =
                track_list.into_iter().rev().collect();
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
        let names: Vec<String> = msg.args[1..]
            .iter()
            .filter_map(osc_arg_string)
            .collect();

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
        let classes: Vec<String> = msg.args[1..]
            .iter()
            .filter_map(osc_arg_string)
            .collect();

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
                send_osc_to(
                    &self.send_socket,
                    "/live/device/get/parameters/name",
                    args,
                );
                send_osc_to(
                    &self.send_socket,
                    "/live/device/get/parameters/min",
                    args,
                );
                send_osc_to(
                    &self.send_socket,
                    "/live/device/get/parameters/max",
                    args,
                );
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
        let names: Vec<String> = msg.args[2..]
            .iter()
            .filter_map(osc_arg_string)
            .collect();

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
        let mins: Vec<f32> = msg.args[2..]
            .iter()
            .filter_map(osc_arg_float)
            .collect();

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
        let maxs: Vec<f32> = msg.args[2..]
            .iter()
            .filter_map(osc_arg_float)
            .collect();

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
                .is_some_and(|d| !d.macros.is_empty() && d.macros.iter().all(|m| !m.name.is_empty()))
        });

        if !all_done {
            return;
        }

        let DiscoveryState::QueryingParams {
            ref tracks, ..
        } = self.discovery_state
        else {
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

    fn validate_single_mapping(
        &self,
        mapping: &mut AbletonParamMapping,
    ) -> AbletonMappingStatus {
        let stored_track_id = mapping.address.track_id;
        let stored_device_id = mapping.address.device_id;
        let stored_param_id = mapping.address.param_id;
        let target_class = mapping.address.device_identity.device_class_name.clone();

        // 1. Check if track at stored index has the right device at stored index
        if let Some(track) = self
            .session
            .tracks
            .iter()
            .find(|t| t.track_id == stored_track_id)
            && let Some(device) = track
                .devices
                .iter()
                .find(|d| d.device_id == stored_device_id)
            && device.class_name == target_class
        {
            mapping.address.track_name = track.name.clone();
            mapping.address.device_name = device.name.clone();
            if let Some(mac) =
                device.macros.iter().find(|m| m.param_id == stored_param_id)
            {
                mapping.address.macro_name = mac.name.clone();
            }
            return AbletonMappingStatus::Active;
        }

        // 2. Track/device not at expected index — search for unique structural match
        let mut matches: Vec<(i32, i32, String, String, String)> = Vec::new();
        for track in &self.session.tracks {
            for device in &track.devices {
                if device.class_name == target_class
                    && let Some(mac) =
                        device.macros.iter().find(|m| m.param_id == stored_param_id)
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
                // Unambiguous — auto-update indices and names
                let (tid, did, tname, dname, mname) = &matches[0];
                mapping.address.track_id = *tid;
                mapping.address.device_id = *did;
                mapping.address.track_name = tname.clone();
                mapping.address.device_name = dname.clone();
                mapping.address.macro_name = mname.clone();
                log::info!(
                    "[AbletonBridge] Auto-resolved mapping: {tname} > {dname} > {mname}",
                );
                AbletonMappingStatus::Active
            }
            _ => {
                log::warn!(
                    "[AbletonBridge] Ambiguous mapping: {} matches for class '{target_class}'",
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
            if track.track_id != track_id { continue; }
            for device in &track.devices {
                if device.device_id != device_id { continue; }
                for mac in &device.macros {
                    if mac.param_id == param_id {
                        let hi = if mac.max > mac.min { mac.max } else { mac.min + 1.0 };
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
        let mut new_write_targets: AHashMap<(i32, i32, i32), Vec<WriteTarget>> =
            AHashMap::new();

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

                    let param_def = manifold_core::effect_definition_registry::try_get(
                        fx.effect_type(),
                    )
                    .and_then(|def| def.param_defs.get(mapping.param_index));
                    let (pmin, pmax) = param_def
                        .map(|pd| (pd.min, pd.max))
                        .unwrap_or((0.0, 1.0));
                    let (abl_min, abl_max) = self.ableton_param_range(
                        mapping.address.track_id,
                        mapping.address.device_id,
                        mapping.address.param_id,
                    );

                    new_write_targets
                        .entry(key)
                        .or_default()
                        .push(WriteTarget {
                            target: AbletonMappingTarget::MasterEffect {
                                effect_type: fx.effect_type().clone(),
                                param_index: mapping.param_index,
                            },
                            param_index: mapping.param_index,
                            ableton_min: abl_min,
                            ableton_max: abl_max,
                            range_min: mapping.range_min,
                            range_max: mapping.range_max,
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

                            let param_def =
                                manifold_core::effect_definition_registry::try_get(
                                    fx.effect_type(),
                                )
                                .and_then(|def| def.param_defs.get(mapping.param_index));
                            let (pmin, pmax) = param_def
                                .map(|pd| (pd.min, pd.max))
                                .unwrap_or((0.0, 1.0));
                            let (abl_min, abl_max) = self.ableton_param_range(
                                mapping.address.track_id,
                                mapping.address.device_id,
                                mapping.address.param_id,
                            );

                            new_write_targets
                                .entry(key)
                                .or_default()
                                .push(WriteTarget {
                                    target: AbletonMappingTarget::LayerEffect {
                                        layer_id: layer_id.clone(),
                                        effect_type: fx.effect_type().clone(),
                                        param_index: mapping.param_index,
                                    },
                                    param_index: mapping.param_index,
                                    ableton_min: abl_min,
                                    ableton_max: abl_max,
                                    range_min: mapping.range_min,
                                    range_max: mapping.range_max,
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

                    let param_def =
                        manifold_core::generator_definition_registry::try_get(
                            gp.generator_type(),
                        )
                        .and_then(|def| def.param_defs.get(mapping.param_index));
                    let (pmin, pmax) = param_def
                        .map(|pd| (pd.min, pd.max))
                        .unwrap_or((0.0, 1.0));
                    let (abl_min, abl_max) = self.ableton_param_range(
                        mapping.address.track_id,
                        mapping.address.device_id,
                        mapping.address.param_id,
                    );

                    new_write_targets
                        .entry(key)
                        .or_default()
                        .push(WriteTarget {
                            target: AbletonMappingTarget::GenParam {
                                layer_id: layer_id.clone(),
                                param_index: mapping.param_index,
                            },
                            param_index: mapping.param_index,
                            ableton_min: abl_min,
                            ableton_max: abl_max,
                            range_min: mapping.range_min,
                            range_max: mapping.range_max,
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

                new_write_targets
                    .entry(key)
                    .or_default()
                    .push(WriteTarget {
                        target: AbletonMappingTarget::MacroSlot { slot_index: i },
                        param_index: 0,
                        ableton_min: abl_min,
                        ableton_max: abl_max,
                        range_min: mapping.range_min,
                        range_max: mapping.range_max,
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
    }

    /// Build an `AbletonSetContext` from the current session for project storage.
    pub fn build_set_context(&self) -> AbletonSetContext {
        AbletonSetContext {
            set_file_path: self.session.set_file_path.clone(),
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
            log::warn!(
                "[AbletonBridge] Cannot enable transport sync — no send socket"
            );
            return;
        }
        self.transport_enabled = true;
        // Reset SMs for clean start
        self.transport_sm = TransportCommand::Idle;
        self.seek_sm = SeekCommand::Idle;
        self.last_known_manifold_playing = false;
        self.last_ableton_is_playing = false;
        self.ableton_position_beats = 0.0;
        self.last_position_poll_time = 0.0;
        self.send_osc("/live/song/start_listen/is_playing", &[]);
        self.send_osc("/live/song/start_listen/tempo", &[]);
        // Seed initial state
        self.send_osc("/live/song/get/is_playing", &[]);
        self.send_osc("/live/song/get/tempo", &[]);
        eprintln!("[ABL-TRANSPORT] Transport sync enabled, SMs → Idle");
        log::info!("[AbletonBridge] Transport sync enabled");
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
        }
        self.transport_enabled = false;
        self.ableton_is_playing = false;
        self.last_ableton_is_playing = false;
        self.ableton_tempo = 120.0;
        self.ableton_position_beats = 0.0;
        self.transport_last_received = 0.0;
        self.transport_sm = TransportCommand::Idle;
        self.seek_sm = SeekCommand::Idle;
        self.last_known_manifold_playing = false;
        self.last_position_poll_time = 0.0;
        *self.pending_transport.lock() = PendingTransportState::default();
        eprintln!("[ABL-TRANSPORT] Transport sync disabled, all SMs → Idle");
        log::info!("[AbletonBridge] Transport sync disabled");
    }

    pub fn is_transport_enabled(&self) -> bool {
        self.transport_enabled
    }

    pub fn ableton_tempo(&self) -> f32 {
        self.ableton_tempo
    }

    // ── State machine: drain + process ───────────────────────────

    /// Drain pending transport state from the receiver thread.
    /// Updates ableton_is_playing, ableton_tempo, ableton_position_beats.
    fn drain_pending(&mut self, realtime: f64) {
        let pending = {
            let mut pt = self.pending_transport.lock();
            PendingTransportState {
                is_playing: pt.is_playing.take(),
                tempo: pt.tempo.take(),
                current_song_time: pt.current_song_time.take(),
            }
        };

        if pending.is_playing.is_some()
            || pending.tempo.is_some()
            || pending.current_song_time.is_some()
        {
            self.transport_last_received = realtime;
        }

        if let Some(playing) = pending.is_playing {
            self.ableton_is_playing = playing;
        }
        if let Some(tempo) = pending.tempo {
            self.ableton_tempo = tempo;
        }
        if let Some(pos) = pending.current_song_time {
            self.ableton_position_beats = pos;
        }
    }

    /// Main per-frame transport state machine tick. Called once per frame
    /// from tick_sync_controllers (BEFORE engine tick).
    ///
    /// Handles bidirectional play/stop/seek between MANIFOLD and Ableton:
    /// - Detects MANIFOLD-initiated transport changes → sends commands to Ableton
    /// - Detects Ableton-initiated transport changes → returns actions for engine
    /// - Confirms pending commands via listener responses
    /// - Retries on timeout, gives up after SM_MAX_RETRIES
    /// - Polls position for Ableton-initiated seek detection (when CLK off)
    pub fn process_transport(
        &mut self,
        manifold_is_playing: bool,
        manifold_beat: f32,
        _bpm: f32,
        realtime: f64,
        midi_clk_active: bool,
        arbiter: &mut SyncArbiter,
    ) -> TransportActions {
        if !self.transport_enabled {
            return TransportActions::none();
        }

        // 1. Drain pending OSC responses from receiver thread
        self.drain_pending(realtime);

        let mut actions = TransportActions::none();

        // 2. Detect edges
        let ableton_edge = if self.ableton_is_playing
            != self.last_ableton_is_playing
        {
            Some(self.ableton_is_playing)
        } else {
            None
        };
        let manifold_edge = manifold_is_playing
            != self.last_known_manifold_playing;

        // 3. Process transport SM
        self.process_transport_sm(
            manifold_is_playing,
            manifold_beat,
            realtime,
            ableton_edge,
            manifold_edge,
            arbiter,
            &mut actions,
        );

        // 4. Process seek SM (confirmations, timeouts, retries)
        self.process_seek_sm(realtime, &mut actions);

        // 5. Position monitor (Ableton-initiated seek detection)
        if !midi_clk_active && matches!(self.seek_sm, SeekCommand::Idle) {
            self.poll_position_monitor(
                manifold_beat,
                realtime,
                &mut actions,
            );
        }

        // 6. Update tracking for next frame
        self.last_ableton_is_playing = self.ableton_is_playing;

        actions
    }

    /// Transport (play/stop) state machine transitions.
    ///
    /// Play sends start_playing + set/current_song_time in one burst.
    /// Stop sends stop_playing + set/current_song_time to preserve position.
    /// (Ableton's stop_playing resets playhead; start_playing plays from 0:00.)
    fn process_transport_sm(
        &mut self,
        manifold_is_playing: bool,
        manifold_beat: f32,
        realtime: f64,
        ableton_edge: Option<bool>,
        manifold_edge: bool,
        arbiter: &mut SyncArbiter,
        actions: &mut TransportActions,
    ) {
        match self.transport_sm {
            TransportCommand::Idle => {
                // Check MANIFOLD-initiated change FIRST
                if manifold_edge {
                    if manifold_is_playing {
                        // MANIFOLD started playing → play + seek together
                        self.send_play_with_position(manifold_beat);
                        arbiter.set_manifold_owns_at(Seconds(realtime));
                        self.transport_sm = TransportCommand::PlaySent {
                            sent_at: realtime,
                            retries: 0,
                            beat: manifold_beat,
                        };
                        self.last_known_manifold_playing = true;
                        eprintln!(
                            "[ABL-TRANSPORT] Idle → PlaySent \
                             (MANIFOLD initiated, beat={:.1})",
                            manifold_beat
                        );
                        eprintln!(
                            "[ABL-TRANSPORT] SEND start_playing + \
                             set/current_song_time({:.1}) (retry 0/{})",
                            manifold_beat, SM_MAX_RETRIES
                        );
                    } else {
                        // MANIFOLD stopped → stop + preserve position
                        self.send_stop_with_position(manifold_beat);
                        self.transport_sm = TransportCommand::StopSent {
                            sent_at: realtime,
                            retries: 0,
                            beat: manifold_beat,
                        };
                        self.last_known_manifold_playing = false;
                        eprintln!(
                            "[ABL-TRANSPORT] Idle → StopSent \
                             (MANIFOLD initiated, beat={:.1})",
                            manifold_beat
                        );
                        eprintln!(
                            "[ABL-TRANSPORT] SEND stop_playing + \
                             set/current_song_time({:.1}) (retry 0/{})",
                            manifold_beat, SM_MAX_RETRIES
                        );
                    }
                }

                // Check Ableton-initiated change
                if let Some(playing) = ableton_edge {
                    match self.transport_sm {
                        TransportCommand::Idle => {
                            if playing {
                                actions.play = true;
                                self.last_known_manifold_playing = true;
                                eprintln!(
                                    "[ABL-TRANSPORT] RELAY Ableton play \
                                     → engine.play()"
                                );
                            } else {
                                actions.pause = true;
                                self.last_known_manifold_playing = false;
                                eprintln!(
                                    "[ABL-TRANSPORT] RELAY Ableton stop \
                                     → engine.pause()"
                                );
                            }
                        }
                        TransportCommand::PlaySent { .. } => {
                            if playing {
                                self.confirm_play_sent(realtime);
                            } else {
                                eprintln!(
                                    "[ABL-TRANSPORT] Stale is_playing=false \
                                     while in PlaySent, ignoring"
                                );
                            }
                        }
                        TransportCommand::StopSent { .. } => {
                            if !playing {
                                self.confirm_stop_sent(realtime);
                            } else {
                                eprintln!(
                                    "[ABL-TRANSPORT] Stale is_playing=true \
                                     while in StopSent, ignoring"
                                );
                            }
                        }
                    }
                }
            }

            TransportCommand::PlaySent {
                sent_at, retries, beat,
            } => {
                // MANIFOLD interrupt: user hit stop
                if manifold_edge && !manifold_is_playing {
                    self.send_stop_with_position(manifold_beat);
                    self.transport_sm = TransportCommand::StopSent {
                        sent_at: realtime,
                        retries: 0,
                        beat: manifold_beat,
                    };
                    self.last_known_manifold_playing = false;
                    eprintln!(
                        "[ABL-TRANSPORT] PlaySent CANCELLED → StopSent \
                         (MANIFOLD stopped, beat={:.1})",
                        manifold_beat
                    );
                    return;
                }

                // Ableton confirmation
                if let Some(playing) = ableton_edge {
                    if playing {
                        self.confirm_play_sent(realtime);
                    } else {
                        eprintln!(
                            "[ABL-TRANSPORT] Stale is_playing=false \
                             while in PlaySent, ignoring"
                        );
                    }
                    return;
                }

                // Timeout → retry (resend play + position together)
                if realtime - sent_at >= SM_RETRY_TIMEOUT_SECS {
                    if retries < SM_MAX_RETRIES {
                        let r = retries + 1;
                        self.send_play_with_position(beat);
                        self.transport_sm = TransportCommand::PlaySent {
                            sent_at: realtime,
                            retries: r,
                            beat,
                        };
                        eprintln!(
                            "[ABL-TRANSPORT] TIMEOUT PlaySent, retry {}/{} \
                             — SEND start_playing + set/current_song_time({:.1})",
                            r, SM_MAX_RETRIES, beat
                        );
                    } else {
                        self.transport_sm = TransportCommand::Idle;
                        eprintln!(
                            "[ABL-TRANSPORT] FAILED PlaySent after {} \
                             retries, giving up (MANIFOLD keeps playing)",
                            SM_MAX_RETRIES
                        );
                    }
                }
            }

            TransportCommand::StopSent {
                sent_at, retries, beat,
            } => {
                // MANIFOLD interrupt: user hit play
                if manifold_edge && manifold_is_playing {
                    self.send_play_with_position(manifold_beat);
                    arbiter.set_manifold_owns_at(Seconds(realtime));
                    self.transport_sm = TransportCommand::PlaySent {
                        sent_at: realtime,
                        retries: 0,
                        beat: manifold_beat,
                    };
                    self.last_known_manifold_playing = true;
                    eprintln!(
                        "[ABL-TRANSPORT] StopSent CANCELLED → PlaySent \
                         (MANIFOLD plays, beat={:.1})",
                        manifold_beat
                    );
                    return;
                }

                // Ableton confirmation
                if let Some(playing) = ableton_edge {
                    if !playing {
                        self.confirm_stop_sent(realtime);
                    } else {
                        eprintln!(
                            "[ABL-TRANSPORT] Stale is_playing=true \
                             while in StopSent, ignoring"
                        );
                    }
                    return;
                }

                // Timeout → retry (resend stop + position together)
                if realtime - sent_at >= SM_RETRY_TIMEOUT_SECS {
                    if retries < SM_MAX_RETRIES {
                        let r = retries + 1;
                        self.send_stop_with_position(beat);
                        self.transport_sm = TransportCommand::StopSent {
                            sent_at: realtime,
                            retries: r,
                            beat,
                        };
                        eprintln!(
                            "[ABL-TRANSPORT] TIMEOUT StopSent, retry {}/{} \
                             — SEND stop_playing + set/current_song_time({:.1})",
                            r, SM_MAX_RETRIES, beat
                        );
                    } else {
                        self.transport_sm = TransportCommand::Idle;
                        eprintln!(
                            "[ABL-TRANSPORT] FAILED StopSent after {} \
                             retries, giving up",
                            SM_MAX_RETRIES
                        );
                    }
                }
            }
        }
    }

    /// Confirm PlaySent → Idle.
    fn confirm_play_sent(&mut self, realtime: f64) {
        if let TransportCommand::PlaySent { sent_at, .. } =
            self.transport_sm
        {
            let latency_ms = (realtime - sent_at) * 1000.0;
            eprintln!(
                "[ABL-TRANSPORT] CONFIRMED is_playing=true \
                 (latency={:.0}ms)",
                latency_ms
            );
        }
        self.transport_sm = TransportCommand::Idle;
    }

    /// Confirm StopSent → Idle.
    fn confirm_stop_sent(&mut self, realtime: f64) {
        if let TransportCommand::StopSent { sent_at, .. } =
            self.transport_sm
        {
            let latency_ms = (realtime - sent_at) * 1000.0;
            eprintln!(
                "[ABL-TRANSPORT] CONFIRMED is_playing=false \
                 (latency={:.0}ms)",
                latency_ms
            );
        }
        self.transport_sm = TransportCommand::Idle;
    }

    /// Seek state machine: check confirmation, timeout, retry.
    fn process_seek_sm(
        &mut self,
        realtime: f64,
        actions: &mut TransportActions,
    ) {
        let _ = actions; // seek SM only generates actions via position monitor
        match self.seek_sm {
            SeekCommand::Idle => {}
            SeekCommand::Sent {
                target_beat,
                sent_at,
                retries,
            } => {
                // Check if we got a position response this frame
                // (drain_pending already updated ableton_position_beats)
                if self.transport_last_received >= sent_at {
                    let delta =
                        (self.ableton_position_beats - target_beat).abs();
                    if delta < SEEK_CONFIRM_TOLERANCE {
                        let latency_ms =
                            (realtime - sent_at) * 1000.0;
                        self.seek_sm = SeekCommand::Idle;
                        eprintln!(
                            "[ABL-SEEK] CONFIRMED target={:.1} \
                             actual={:.1} (latency={:.0}ms)",
                            target_beat,
                            self.ableton_position_beats,
                            latency_ms
                        );
                        return;
                    }
                }

                // Check for timeout
                if realtime - sent_at >= SM_RETRY_TIMEOUT_SECS {
                    if retries < SM_MAX_RETRIES {
                        let new_retries = retries + 1;
                        self.send_osc(
                            "/live/song/set/current_song_time",
                            &[rosc::OscType::Float(target_beat)],
                        );
                        self.send_osc(
                            "/live/song/get/current_song_time",
                            &[],
                        );
                        self.seek_sm = SeekCommand::Sent {
                            target_beat,
                            sent_at: realtime,
                            retries: new_retries,
                        };
                        eprintln!(
                            "[ABL-SEEK] TIMEOUT after {:.0}ms, \
                             retry {}/{}",
                            SM_RETRY_TIMEOUT_SECS * 1000.0,
                            new_retries,
                            SM_MAX_RETRIES
                        );
                        eprintln!(
                            "[ABL-SEEK] SEND set/current_song_time({:.1}) \
                             + poll (retry {}/{})",
                            target_beat, new_retries, SM_MAX_RETRIES
                        );
                    } else {
                        self.seek_sm = SeekCommand::Idle;
                        eprintln!(
                            "[ABL-SEEK] FAILED after {} retries, giving up",
                            SM_MAX_RETRIES
                        );
                    }
                }
            }
        }
    }

    /// Position monitor: poll Ableton's position periodically and relay
    /// large deltas as Ableton-initiated seeks.
    /// Only active when MIDI CLK is off and Seek SM is Idle.
    fn poll_position_monitor(
        &mut self,
        manifold_beat: f32,
        realtime: f64,
        actions: &mut TransportActions,
    ) {
        // Rate limit polling
        if realtime - self.last_position_poll_time
            < POSITION_POLL_INTERVAL_SECS
        {
            return;
        }

        let poll_sent_time = self.last_position_poll_time;
        self.last_position_poll_time = realtime;

        // Only act on position responses that arrived AFTER our last poll
        // was sent (avoids stale data from before a MANIFOLD-initiated seek).
        // Also require at least 2 poll cycles before acting (avoids startup
        // spike where ableton_position_beats is from an initial seed).
        if poll_sent_time > 0.0
            && self.transport_last_received > poll_sent_time
            && self.ableton_position_beats > 0.0
        {
            let delta =
                (self.ableton_position_beats - manifold_beat).abs();
            if delta > POSITION_RELAY_THRESHOLD {
                actions.seek_beat = Some(self.ableton_position_beats);
                eprintln!(
                    "[ABL-SEEK] MONITOR delta={:.1} beats → relay \
                     engine.seek({:.1})",
                    delta, self.ableton_position_beats
                );
            }
        }

        // Send poll for next check
        self.send_osc("/live/song/get/current_song_time", &[]);
    }

    /// Notify the bridge that MANIFOLD initiated a seek (from content
    /// commands: SeekTo, SeekToBeat, ruler scrub, etc.).
    /// Enters Seek::Sent and sends position + confirmation poll.
    pub fn notify_manifold_seek(
        &mut self,
        target_beat: f32,
        realtime: f64,
    ) {
        if !self.transport_enabled {
            return;
        }

        match self.seek_sm {
            SeekCommand::Sent {
                target_beat: old_target,
                ..
            } => {
                eprintln!(
                    "[ABL-SEEK] Sent(target={:.1}) → Sent(target={:.1}) \
                     (new seek, reset retries)",
                    old_target, target_beat
                );
            }
            SeekCommand::Idle => {
                eprintln!(
                    "[ABL-SEEK] Idle → Sent (target={:.1})",
                    target_beat
                );
            }
        }

        self.send_seek(target_beat, realtime);
    }

    /// Send play + position in one burst. Ableton's start_playing resets
    /// the playhead, so we immediately follow with set/current_song_time.
    fn send_play_with_position(&self, beat: f32) {
        self.send_osc("/live/song/start_playing", &[]);
        self.send_osc(
            "/live/song/set/current_song_time",
            &[rosc::OscType::Float(beat)],
        );
    }

    /// Send stop + restore position. Ableton's stop_playing resets the
    /// playhead to the start, so we immediately follow with
    /// set/current_song_time to preserve MANIFOLD's position.
    fn send_stop_with_position(&self, beat: f32) {
        self.send_osc("/live/song/stop_playing", &[]);
        self.send_osc(
            "/live/song/set/current_song_time",
            &[rosc::OscType::Float(beat)],
        );
    }

    /// Common seek send + state update.
    fn send_seek(&mut self, target_beat: f32, realtime: f64) {
        self.send_osc(
            "/live/song/set/current_song_time",
            &[rosc::OscType::Float(target_beat)],
        );
        self.send_osc("/live/song/get/current_song_time", &[]);
        self.seek_sm = SeekCommand::Sent {
            target_beat,
            sent_at: realtime,
            retries: 0,
        };
        eprintln!(
            "[ABL-SEEK] SEND set/current_song_time({:.1}) + poll \
             (retry 0/{})",
            target_beat, SM_MAX_RETRIES
        );
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

/// Free function to send OSC — avoids borrow conflicts when called alongside
/// mutable borrows of other `AbletonBridge` fields.
fn send_osc_to(socket: &Option<UdpSocket>, address: &str, args: &[rosc::OscType]) {
    if let Some(sock) = socket {
        let msg = rosc::OscMessage {
            addr: address.to_string(),
            args: args.to_vec(),
        };
        let packet = rosc::OscPacket::Message(msg);
        match rosc::encoder::encode(&packet) {
            Ok(buf) => {
                let _ = sock.send(&buf);
            }
            Err(e) => {
                log::error!("[AbletonBridge] OSC encode error: {e:?}");
            }
        }
    }
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

    /// Helper: create a bridge with transport enabled for SM tests.
    /// No real socket, so send_osc calls are silently dropped.
    fn test_bridge() -> AbletonBridge {
        let mut b = AbletonBridge::new();
        b.transport_enabled = true;
        b
    }

    /// Helper: create a no-op arbiter for SM tests.
    fn test_arbiter() -> SyncArbiter {
        SyncArbiter::new()
    }

    #[test]
    fn transport_sm_manifold_play() {
        let mut bridge = test_bridge();
        let mut arb = test_arbiter();
        // MANIFOLD starts playing at beat 5
        let actions = bridge.process_transport(
            true, 5.0, 120.0, 1.0, false, &mut arb,
        );
        // No relay action (MANIFOLD already playing)
        assert!(!actions.play);
        assert!(!actions.pause);
        // SM should be in PlaySent
        assert!(matches!(
            bridge.transport_sm,
            TransportCommand::PlaySent { beat, .. } if beat > 0.0
        ));
    }

    #[test]
    fn transport_sm_play_confirmed() {
        let mut bridge = test_bridge();
        let mut arb = test_arbiter();
        // Enter PlaySent
        bridge.process_transport(true, 5.0, 120.0, 1.0, false, &mut arb);
        assert!(matches!(bridge.transport_sm, TransportCommand::PlaySent { .. }));

        // Inject Ableton confirmation
        bridge.pending_transport.lock().is_playing = Some(true);
        let actions = bridge.process_transport(
            true, 5.1, 120.0, 1.05, false, &mut arb,
        );
        // No relay (engine already playing), SM back to Idle
        assert!(!actions.play);
        assert!(matches!(bridge.transport_sm, TransportCommand::Idle));
    }

    #[test]
    fn transport_sm_ableton_play() {
        let mut bridge = test_bridge();
        let mut arb = test_arbiter();
        // Inject Ableton play (while MANIFOLD is stopped)
        bridge.pending_transport.lock().is_playing = Some(true);
        let actions = bridge.process_transport(
            false, 0.0, 120.0, 1.0, false, &mut arb,
        );
        // Should relay play to engine
        assert!(actions.play);
        assert!(!actions.pause);
        // SM stays Idle (no outbound command needed)
        assert!(matches!(bridge.transport_sm, TransportCommand::Idle));
    }

    #[test]
    fn transport_sm_play_timeout_retry() {
        let mut bridge = test_bridge();
        let mut arb = test_arbiter();
        // Enter PlaySent at t=1.0
        bridge.process_transport(true, 5.0, 120.0, 1.0, false, &mut arb);

        // Advance past timeout (50ms) with no Ableton response
        let actions = bridge.process_transport(
            true, 5.1, 120.0, 1.06, false, &mut arb,
        );
        assert!(!actions.play);
        // Should still be PlaySent with retries incremented
        if let TransportCommand::PlaySent { retries, .. } = bridge.transport_sm
        {
            assert_eq!(retries, 1);
        } else {
            panic!("Expected PlaySent after timeout");
        }
    }

    #[test]
    fn transport_sm_play_max_retries() {
        let mut bridge = test_bridge();
        let mut arb = test_arbiter();
        // Manually set PlaySent with max retries
        bridge.transport_sm = TransportCommand::PlaySent {
            sent_at: 0.0,
            retries: SM_MAX_RETRIES,
            beat: 5.0,
        };
        bridge.last_known_manifold_playing = true;
        // Advance past timeout
        let actions = bridge.process_transport(
            true, 5.0, 120.0, 0.1, false, &mut arb,
        );
        // Should give up → Idle, no engine change
        assert!(!actions.play);
        assert!(!actions.pause);
        assert!(matches!(bridge.transport_sm, TransportCommand::Idle));
    }

    #[test]
    fn transport_sm_cancel_play() {
        let mut bridge = test_bridge();
        let mut arb = test_arbiter();
        // Enter PlaySent
        bridge.process_transport(true, 5.0, 120.0, 1.0, false, &mut arb);
        assert!(matches!(bridge.transport_sm, TransportCommand::PlaySent { .. }));

        // MANIFOLD stops (user interrupt)
        let actions = bridge.process_transport(
            false, 5.0, 120.0, 1.01, false, &mut arb,
        );
        assert!(!actions.play);
        assert!(!actions.pause);
        // Should transition to StopSent
        assert!(matches!(bridge.transport_sm, TransportCommand::StopSent { .. }));
    }

    #[test]
    fn seek_sm_manifold_seek() {
        let mut bridge = test_bridge();
        bridge.notify_manifold_seek(32.0, 1.0);
        assert!(matches!(
            bridge.seek_sm,
            SeekCommand::Sent { target_beat, .. } if (target_beat - 32.0).abs() < 0.01
        ));
    }

    #[test]
    fn seek_sm_confirmed() {
        let mut bridge = test_bridge();
        let mut arb = test_arbiter();
        bridge.notify_manifold_seek(32.0, 1.0);

        // Inject position response near target
        bridge.pending_transport.lock().current_song_time = Some(32.1);
        bridge.last_known_manifold_playing = false;
        let actions = bridge.process_transport(
            false, 32.0, 120.0, 1.02, false, &mut arb,
        );
        assert!(actions.seek_beat.is_none());
        assert!(matches!(bridge.seek_sm, SeekCommand::Idle));
    }

    #[test]
    fn seek_sm_new_target_during_sent() {
        let mut bridge = test_bridge();
        bridge.notify_manifold_seek(20.0, 1.0);
        assert!(matches!(
            bridge.seek_sm,
            SeekCommand::Sent { target_beat, .. } if (target_beat - 20.0).abs() < 0.01
        ));

        // New seek while still Sent → update target, reset retries
        bridge.notify_manifold_seek(35.0, 1.01);
        if let SeekCommand::Sent {
            target_beat,
            retries,
            ..
        } = bridge.seek_sm
        {
            assert!((target_beat - 35.0).abs() < 0.01);
            assert_eq!(retries, 0);
        } else {
            panic!("Expected Seek::Sent");
        }
    }

    #[test]
    fn position_monitor_detects_delta() {
        let mut bridge = test_bridge();
        let mut arb = test_arbiter();
        // Simulate: a previous poll was sent at t=0.5
        bridge.last_position_poll_time = 0.5;
        // Position response arrived at t=0.6 (after the poll)
        bridge.ableton_position_beats = 47.0;
        bridge.transport_last_received = 0.6;
        // Process at t=1.0 (>250ms after last poll, so new poll fires)
        // MANIFOLD is at beat 30
        let actions = bridge.process_transport(
            false, 30.0, 120.0, 1.0, false, &mut arb,
        );
        // Delta = 17 > threshold → should relay seek
        assert_eq!(actions.seek_beat, Some(47.0));
    }

    #[test]
    fn write_target_value_mapping() {
        let wt = WriteTarget {
            target: AbletonMappingTarget::MasterEffect {
                effect_type: manifold_core::EffectTypeId::BLOOM,
                param_index: 0,
            },
            param_index: 0,
            // Rack macros send 0-127 from Ableton
            ableton_min: 0.0,
            ableton_max: 127.0,
            range_min: 0.25,
            range_max: 0.75,
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
}
