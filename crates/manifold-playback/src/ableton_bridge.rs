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
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected
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
            // Ensure we have macro entries (limited to RACK_MACRO_COUNT)
            for (i, name) in names.iter().take(RACK_MACRO_COUNT).enumerate() {
                if i < device.macros.len() {
                    device.macros[i].name = name.clone();
                } else {
                    device.macros.push(AbletonMacro {
                        param_id: i as i32,
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
            for (i, &min_val) in mins.iter().take(RACK_MACRO_COUNT).enumerate() {
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
            for (i, &max_val) in maxs.iter().take(RACK_MACRO_COUNT).enumerate() {
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
