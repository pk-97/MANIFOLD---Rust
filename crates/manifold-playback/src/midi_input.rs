//! MIDI device input controller.
//! Mechanical translation of Unity MidiInputController.cs (526 lines).
//!
//! Architecture divergence [D-26]: Unity has two input paths — Minis (Unity Input System)
//! and a native CoreMIDI plugin (MidiClock.bundle) when CLK is the clock authority and
//! the plugin supports note events. In Rust, `midir` replaces both. It is the native
//! CoreMIDI backend on macOS (ALSA on Linux, WinMM on Windows). One unified input path
//! with equivalent accuracy. See docs/KNOWN_DIVERGENCES.md.
//!
//! MidiNoteEventType and MidiNoteEvent are defined here, translated from
//! MidiClock.cs (MidiClock.MidiNoteEventType / MidiClock.MidiNoteEvent).

use std::sync::mpsc::{self, Receiver, Sender};

use manifold_core::midi::MidiMappingConfig;
use manifold_core::project::Project;
use manifold_core::types::ClockAuthority;

use crate::clip_launcher::ClipLauncher;
use crate::live_clip_manager::{LiveClipHost, LiveClipManager};

// ── Types from MidiClock.cs ───────────────────────────────────────────────────

/// Port of C# MidiClock.MidiNoteEventType enum.
/// Unknown = 0, NoteOn = 1, NoteOff = 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiNoteEventType {
    Unknown = 0,
    NoteOn = 1,
    NoteOff = 2,
}

/// Port of C# MidiClock.MidiNoteEvent struct.
/// Carries all fields from the native plugin queue.
#[derive(Debug, Clone, Copy)]
pub struct MidiNoteEvent {
    pub event_type: MidiNoteEventType,
    pub note: i32,
    pub velocity: f32,
    pub channel: i32,
    pub source_index: i32,
    pub absolute_tick: i32,
    pub sequence: u32,
}

// ── Constants ─────────────────────────────────────────────────────────────────

/// Port of C# MidiInputController.NativeClockDeviceIdBase = 100000.
const NATIVE_CLOCK_DEVICE_ID_BASE: i32 = 100000;

/// Port of C# MidiInputController.MaxNativeClockEventsPerFrame = 512.
const MAX_NATIVE_CLOCK_EVENTS_PER_FRAME: usize = 512;

// ── MidiDevice ────────────────────────────────────────────────────────────────

/// Represents a connected midir MIDI input port.
/// Holds the live connection (dropping it closes the port) and the port name.
struct MidiDevice {
    /// midir connection — kept alive by ownership.
    #[allow(dead_code)]
    connection: midir::MidiInputConnection<()>,
    /// Display name of the port.
    name: String,
    /// Port index (used to deduplicate).
    port_index: usize,
}

// ── MidiInputController ───────────────────────────────────────────────────────

/// Routes MIDI note events to ClipLauncher.
/// Port of Unity MidiInputController.cs.
///
/// Lifecycle:
///   `new()` → `start()` (OnEnable) → `update()` per tick → `stop()` (OnDisable)
pub struct MidiInputController {
    // ── Serialized config (matches Unity [SerializeField] fields) ──
    midi_channel: i32,
    show_debug_logs: bool,
    /// When CLK is the active clock authority, track native path for telemetry.
    /// Port of Unity: useNativeClockNoteEvents field.
    use_native_clock_note_events: bool,

    // ── Runtime state ──────────────────────────────────────────────
    midi_config: Option<MidiMappingConfig>,
    /// Connected midir devices (replaces Unity's List<MidiDevice> registeredDevices).
    registered_devices: Vec<MidiDevice>,
    device_filter: Option<String>,
    native_clock_path_active_last_frame: bool,
    #[allow(dead_code)]
    logged_minis_clock_fallback_warning: bool,
    last_dropped_native_note_events: i32,
    /// Pre-allocated drain buffer (replaces Unity's List<MidiNoteEvent>(512)).
    native_event_buffer: Vec<MidiNoteEvent>,

    // ── midir channel ──────────────────────────────────────────────
    /// Sender cloned into midir callbacks; update() drains the receiver.
    event_tx: Sender<MidiNoteEvent>,
    /// Receiver owned by the controller; drained each update().
    event_rx: Receiver<MidiNoteEvent>,

    // ── Public properties (port of C# public { get; private set; }) ─
    pub last_note_number: i32,
    pub last_velocity: f32,
    pub native_clock_path_active: bool,
    pub native_clock_events_processed_last_frame: i32,
    pub native_clock_events_processed_total: i32,
    pub native_clock_same_tick_reorders_last_frame: i32,
    pub native_clock_same_tick_reorders_total: i32,
    pub native_clock_pending_events: i32,
    pub native_clock_dropped_events: i32,
}

impl MidiInputController {
    /// Create a new controller. Equivalent to Unity `Awake()`.
    /// Call `start()` to open devices (OnEnable).
    pub fn new() -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        Self {
            midi_channel: -1,
            show_debug_logs: false,
            use_native_clock_note_events: true,
            midi_config: None,
            registered_devices: Vec::new(),
            device_filter: None,
            native_clock_path_active_last_frame: false,
            logged_minis_clock_fallback_warning: false,
            last_dropped_native_note_events: 0,
            native_event_buffer: Vec::with_capacity(MAX_NATIVE_CLOCK_EVENTS_PER_FRAME),
            event_tx,
            event_rx,
            last_note_number: -1,
            last_velocity: 0.0,
            native_clock_path_active: false,
            native_clock_events_processed_last_frame: 0,
            native_clock_events_processed_total: 0,
            native_clock_same_tick_reorders_last_frame: 0,
            native_clock_same_tick_reorders_total: 0,
            native_clock_pending_events: 0,
            native_clock_dropped_events: 0,
        }
    }

    // ── Public configuration ─────────────────────────────────────────

    pub fn set_midi_channel(&mut self, channel: i32) {
        self.midi_channel = channel;
    }

    pub fn set_show_debug_logs(&mut self, value: bool) {
        self.show_debug_logs = value;
    }

    pub fn set_use_native_clock_note_events(&mut self, value: bool) {
        self.use_native_clock_note_events = value;
    }

    /// Port of C# MidiInputController.DeviceFilter property getter.
    pub fn device_filter(&self) -> Option<&str> {
        self.device_filter.as_deref()
    }

    /// Port of C# MidiInputController.DeviceName property.
    pub fn device_name(&self) -> &str {
        if let Some(dev) = self.registered_devices.first() {
            &dev.name
        } else {
            "No device"
        }
    }

    /// Port of C# MidiInputController.HasDevice property.
    pub fn has_device(&self) -> bool {
        !self.registered_devices.is_empty()
    }

    /// Port of C# MidiInputController.SetMidiConfig.
    pub fn set_midi_config(&mut self, config: MidiMappingConfig) {
        if self.show_debug_logs {
            let note_count = config.mappings.len();
            log::debug!("[MidiInputController] Config set: {} notes mapped", note_count);
        }
        self.midi_config = Some(config);
    }

    /// Port of C# MidiInputController.SetDeviceFilter.
    /// Restricts input to devices whose port name contains the filter string (case-insensitive).
    /// Pass None or empty to listen to all devices.
    pub fn set_device_filter(&mut self, product_name: Option<&str>) {
        self.device_filter = match product_name {
            Some(s) if !s.is_empty() => Some(s.to_string()),
            _ => None,
        };

        // Port of C#: unregister devices that no longer match.
        let to_remove: Vec<usize> = self
            .registered_devices
            .iter()
            .enumerate()
            .filter(|(_, dev)| {
                !Self::matches_device_filter_name(&dev.name, self.device_filter.as_deref())
            })
            .map(|(i, _)| i)
            .collect();
        for i in to_remove.into_iter().rev() {
            let dev = self.registered_devices.remove(i);
            log::info!("[MidiInputController] MIDI device disconnected (filter): {}", dev.name);
        }

        // Port of C#: register devices that now match but weren't registered.
        self.scan_and_register_devices();

        log::info!(
            "[MidiInputController] Device filter: {} ({} device(s))",
            self.device_filter.as_deref().unwrap_or("All"),
            self.registered_devices.len()
        );
    }

    /// Port of C# MidiInputController.MatchesDeviceFilter (private).
    /// Returns true when the device name matches the current filter (case-insensitive contains).
    fn matches_device_filter_name(name: &str, filter: Option<&str>) -> bool {
        match filter {
            None => true,
            Some(f) => name.to_lowercase().contains(&f.to_lowercase()),
        }
    }

    // ── Lifecycle ────────────────────────────────────────────────────

    /// Open MIDI devices. Port of C# OnEnable.
    pub fn start(&mut self) {
        self.scan_and_register_devices();
    }

    /// Close all MIDI devices and reset state. Port of C# OnDisable.
    pub fn stop(&mut self) {
        self.unregister_all_devices();
        self.native_clock_path_active_last_frame = false;
        self.logged_minis_clock_fallback_warning = false;
        self.native_clock_path_active = false;
        self.native_clock_events_processed_last_frame = 0;
        self.native_clock_events_processed_total = 0;
        self.native_clock_same_tick_reorders_last_frame = 0;
        self.native_clock_same_tick_reorders_total = 0;
        self.native_clock_pending_events = 0;
        self.native_clock_dropped_events = 0;
    }

    // ── Per-tick update ──────────────────────────────────────────────

    /// Called each content tick. Drains MIDI events from the midir channel and
    /// routes them to ClipLauncher.
    /// Port of C# MidiInputController.Update().
    ///
    /// In Unity, Update() checks IsNativeClockNotePathActive() to decide whether to drain
    /// the native plugin queue or rely on Minis callbacks. In Rust, midir replaces both
    /// paths. All events arrive via the same mpsc channel. The native_clock_path_active
    /// flag is preserved for telemetry parity.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        clock_authority: ClockAuthority,
        project: &mut Project,
        clip_launcher: &mut ClipLauncher,
        live_clip_manager: &mut LiveClipManager,
        host: &mut dyn LiveClipHost,
        realtime_now: f64,
    ) {
        // Port of C# Update(): determine native path active.
        let native_path_active = self.is_native_clock_note_path_active(clock_authority);
        self.native_clock_path_active = native_path_active;
        self.native_clock_events_processed_last_frame = 0;
        self.native_clock_same_tick_reorders_last_frame = 0;

        if native_path_active && !self.native_clock_path_active_last_frame {
            // Transition: inactive → active.
            self.last_dropped_native_note_events = 0;
            self.native_clock_events_processed_total = 0;
            self.native_clock_same_tick_reorders_total = 0;
            self.logged_minis_clock_fallback_warning = false;

            if self.show_debug_logs {
                log::debug!("[MidiInputController] Native CLK note path: active");
            }
        } else if !native_path_active && self.native_clock_path_active_last_frame {
            if self.show_debug_logs {
                log::debug!("[MidiInputController] Native CLK note path: inactive");
            }
        }

        self.native_clock_path_active_last_frame = native_path_active;

        if !native_path_active {
            self.native_clock_pending_events = 0;
            self.native_clock_dropped_events = 0;
        }

        // Drain all events from the midir callback channel into native_event_buffer.
        // Port of C# DrainNativeClockNoteEvents / Minis callback accumulation.
        self.drain_midir_events_to_buffer();

        let processed = self.native_event_buffer.len();

        // Sort events deterministically (NoteOff before NoteOn on same tick).
        if processed > 1 {
            let reorders = Self::count_same_tick_reorders(&self.native_event_buffer);
            self.native_clock_same_tick_reorders_last_frame = reorders;
            self.native_clock_same_tick_reorders_total += reorders;
            self.native_event_buffer.sort_by(compare_native_clock_events);
        }

        // Dispatch each event to ClipLauncher.
        for i in 0..self.native_event_buffer.len() {
            let evt = self.native_event_buffer[i];
            let source_index = evt.source_index.max(0);
            let device_id = NATIVE_CLOCK_DEVICE_ID_BASE + source_index;

            // beat_stamp: Some(tick / 24.0) when a real absolute_tick is present.
            // Port of C# line 379: float beatStamp = evt.AbsoluteTick / 24f;
            // In Rust midir path: absolute_tick = -1 means no clock domain — map to None.
            let beat_stamp: Option<f32> = if evt.absolute_tick >= 0 {
                Some(evt.absolute_tick as f32 / 24.0)
            } else {
                None
            };

            match evt.event_type {
                MidiNoteEventType::NoteOn => {
                    self.process_note_on(
                        evt.note,
                        evt.velocity,
                        evt.channel,
                        device_id,
                        beat_stamp,
                        "midir",
                        evt.sequence,
                        evt.absolute_tick,
                        project,
                        clip_launcher,
                        live_clip_manager,
                        host,
                        realtime_now,
                    );
                }
                MidiNoteEventType::NoteOff => {
                    self.process_note_off(
                        evt.note,
                        evt.channel,
                        device_id,
                        beat_stamp,
                        "midir",
                        evt.sequence,
                        evt.absolute_tick,
                        project,
                        clip_launcher,
                        live_clip_manager,
                        host,
                        realtime_now,
                    );
                }
                MidiNoteEventType::Unknown => {}
            }
        }

        // Update telemetry counters.
        // Port of C# lines 388-399 (NativeClockEventsProcessedLastFrame, etc.).
        self.native_clock_events_processed_last_frame = processed as i32;
        self.native_clock_events_processed_total += processed as i32;
        // last_dropped_native_note_events tracks overflow warning baseline.
        // In the midir path we have no drop counter from a native plugin,
        // so we leave native_clock_dropped_events = 0 when not on native CLK path.

        if self.show_debug_logs
            && processed == MAX_NATIVE_CLOCK_EVENTS_PER_FRAME
            && self.native_clock_pending_events > 0
        {
            log::warn!(
                "[MidiInputController] MIDI note backlog: {} event(s) still queued.",
                self.native_clock_pending_events
            );
        }
    }

    // ── Device management ────────────────────────────────────────────

    /// Scan available midir ports and register any that match the filter.
    /// Port of the device enumeration loop in Unity OnEnable / SetDeviceFilter.
    fn scan_and_register_devices(&mut self) {
        let midi_in = match midir::MidiInput::new("manifold-scan") {
            Ok(m) => m,
            Err(e) => {
                log::warn!("[MidiInputController] Failed to create MidiInput for scan: {}", e);
                return;
            }
        };

        let ports = midi_in.ports();
        // Collect (port_index, name) pairs first to avoid borrow of midi_in inside loop.
        let port_infos: Vec<(usize, String)> = ports
            .iter()
            .enumerate()
            .filter_map(|(idx, port)| midi_in.port_name(port).ok().map(|n| (idx, n)))
            .collect();

        for (port_index, port_name) in port_infos {
            // Filter by device name.
            if !Self::matches_device_filter_name(&port_name, self.device_filter.as_deref()) {
                continue;
            }

            // Don't register duplicates.
            if self.registered_devices.iter().any(|d| d.port_index == port_index) {
                continue;
            }

            self.register_port(port_index, &port_name);
        }
    }

    /// Open a single midir port and register it.
    /// Port of C# MidiInputController.RegisterDevice.
    fn register_port(&mut self, port_index: usize, port_name: &str) {
        let midi_in = match midir::MidiInput::new(&format!("manifold-{}", port_index)) {
            Ok(m) => m,
            Err(e) => {
                log::warn!(
                    "[MidiInputController] Failed to create MidiInput for port {}: {}",
                    port_index, e
                );
                return;
            }
        };

        let ports = midi_in.ports();
        let port = match ports.get(port_index) {
            Some(p) => p,
            None => {
                log::warn!(
                    "[MidiInputController] Port index {} no longer available",
                    port_index
                );
                return;
            }
        };

        let event_tx = self.event_tx.clone();
        let source_index = port_index as i32;

        // Per-connection sequence counter. Monotonically increasing, wraps on overflow.
        // Replaces Unity native plugin's uint sequence field.
        let mut seq: u32 = 0;

        let connection = match midi_in.connect(
            port,
            &format!("manifold-conn-{}", port_index),
            move |_timestamp_us: u64, message: &[u8], _: &mut ()| {
                if message.len() < 3 {
                    return;
                }
                let status = message[0] & 0xF0;
                let channel = (message[0] & 0x0F) as i32;
                let note = message[1] as i32;
                seq = seq.wrapping_add(1);

                let evt = match status {
                    0x90 => {
                        // NoteOn — velocity 0 is a NoteOff by MIDI convention.
                        let velocity = message[2] as f32 / 127.0;
                        if velocity > 0.0 {
                            MidiNoteEvent {
                                event_type: MidiNoteEventType::NoteOn,
                                note,
                                velocity,
                                channel,
                                source_index,
                                absolute_tick: -1, // no clock domain tick from midir
                                sequence: seq,
                            }
                        } else {
                            MidiNoteEvent {
                                event_type: MidiNoteEventType::NoteOff,
                                note,
                                velocity: 0.0,
                                channel,
                                source_index,
                                absolute_tick: -1,
                                sequence: seq,
                            }
                        }
                    }
                    0x80 => MidiNoteEvent {
                        event_type: MidiNoteEventType::NoteOff,
                        note,
                        velocity: 0.0,
                        channel,
                        source_index,
                        absolute_tick: -1,
                        sequence: seq,
                    },
                    _ => return, // ignore non-note MIDI messages
                };

                // Best-effort: if the main thread is behind, drop the event rather than block.
                let _ = event_tx.send(evt);
            },
            (),
        ) {
            Ok(c) => c,
            Err(e) => {
                log::warn!(
                    "[MidiInputController] Failed to connect to port '{}': {}",
                    port_name, e
                );
                return;
            }
        };

        log::info!(
            "[MidiInputController] MIDI device connected: {} (port {})",
            port_name, port_index
        );

        self.registered_devices.push(MidiDevice {
            connection,
            name: port_name.to_string(),
            port_index,
        });
    }

    /// Port of C# MidiInputController.UnregisterAllDevices.
    fn unregister_all_devices(&mut self) {
        for dev in self.registered_devices.drain(..) {
            log::info!("[MidiInputController] MIDI device disconnected: {}", dev.name);
            // midir MidiInputConnection is closed on drop.
        }
    }

    // ── Event drain ──────────────────────────────────────────────────

    /// Drain pending events from the mpsc channel into native_event_buffer.
    /// Capped at MAX_NATIVE_CLOCK_EVENTS_PER_FRAME to match Unity.
    fn drain_midir_events_to_buffer(&mut self) {
        self.native_event_buffer.clear();
        while self.native_event_buffer.len() < MAX_NATIVE_CLOCK_EVENTS_PER_FRAME {
            match self.event_rx.try_recv() {
                Ok(evt) => self.native_event_buffer.push(evt),
                Err(_) => break,
            }
        }
    }

    // ── Native clock path (telemetry flag) ───────────────────────────

    /// Port of C# MidiInputController.IsNativeClockNotePathActive.
    /// In Rust: true when CLK is clock authority and use_native_clock_note_events is set.
    /// Unity checks SupportsNoteEvents on the native plugin — in Rust midir always supports events.
    fn is_native_clock_note_path_active(&self, clock_authority: ClockAuthority) -> bool {
        if !self.use_native_clock_note_events {
            return false;
        }
        clock_authority == ClockAuthority::MidiClock
    }

    // ── Note routing ─────────────────────────────────────────────────

    /// Port of C# MidiInputController.ProcessNoteOn.
    /// Layer-based lookup first, then MidiMappingConfig fallback.
    #[allow(clippy::too_many_arguments)]
    fn process_note_on(
        &mut self,
        note_number: i32,
        velocity: f32,
        channel: i32,
        device_id: i32,
        beat_stamp: Option<f32>,
        source: &str,
        event_sequence: u32,
        event_absolute_tick: i32,
        project: &mut Project,
        clip_launcher: &mut ClipLauncher,
        live_clip_manager: &mut LiveClipManager,
        host: &mut dyn LiveClipHost,
        realtime_now: f64,
    ) {
        // Port of C# line 459.
        if self.midi_channel >= 0 && channel != self.midi_channel {
            return;
        }

        self.last_note_number = note_number;
        self.last_velocity = velocity;

        // Port of C# line 467: Try layer-based lookup first.
        if clip_launcher.handle_note_on_from_layer(
            project,
            live_clip_manager,
            host,
            note_number,
            velocity,
            channel,
            device_id,
            beat_stamp,
            event_sequence,
            event_absolute_tick,
            realtime_now,
        ) {
            if self.show_debug_logs {
                let beat_info = if let Some(b) = beat_stamp {
                    format!(" beat={:.3}", b)
                } else {
                    String::new()
                };
                let seq_info = if event_sequence > 0 {
                    format!(" seq={}", event_sequence)
                } else {
                    String::new()
                };
                log::debug!(
                    "[MidiInputController] [{}] NoteOn: {} ch={} vel={:.2}{}{} → handled by layer",
                    source, note_number, channel, velocity, beat_info, seq_info
                );
            }
            return;
        }

        // Port of C# line 479: Fall back to MidiMappingConfig lookup.
        let mapping = match &self.midi_config {
            None => return,
            Some(cfg) => match cfg.get_mapping_for_note(note_number) {
                None => return,
                Some(m) => m.clone(),
            },
        };

        if self.show_debug_logs {
            let beat_info = if let Some(b) = beat_stamp {
                format!(" beat={:.3}", b)
            } else {
                String::new()
            };
            let seq_info = if event_sequence > 0 {
                format!(" seq={}", event_sequence)
            } else {
                String::new()
            };
            log::debug!(
                "[MidiInputController] [{}] NoteOn: {} ch={} vel={:.2}{}{} → layer {} ({:?})",
                source,
                note_number,
                channel,
                velocity,
                beat_info,
                seq_info,
                mapping.target_layer_index,
                mapping.duration_mode
            );
        }

        clip_launcher.handle_note_on(
            project,
            live_clip_manager,
            host,
            note_number,
            velocity,
            channel,
            device_id,
            &mapping,
            beat_stamp,
            event_sequence,
            event_absolute_tick,
            realtime_now,
        );
    }

    /// Port of C# MidiInputController.ProcessNoteOff.
    #[allow(clippy::too_many_arguments)]
    fn process_note_off(
        &mut self,
        note_number: i32,
        channel: i32,
        device_id: i32,
        beat_stamp: Option<f32>,
        source: &str,
        event_sequence: u32,
        event_absolute_tick: i32,
        project: &mut Project,
        clip_launcher: &mut ClipLauncher,
        live_clip_manager: &mut LiveClipManager,
        host: &mut dyn LiveClipHost,
        realtime_now: f64,
    ) {
        // Port of C# line 497.
        if self.midi_channel >= 0 && channel != self.midi_channel {
            return;
        }

        if self.show_debug_logs {
            let beat_info = if let Some(b) = beat_stamp {
                format!(" beat={:.3}", b)
            } else {
                String::new()
            };
            let seq_info = if event_sequence > 0 {
                format!(" seq={}", event_sequence)
            } else {
                String::new()
            };
            log::debug!(
                "[MidiInputController] [{}] NoteOff: {} ch={}{}{}",
                source, note_number, channel, beat_info, seq_info
            );
        }

        clip_launcher.handle_note_off(
            project,
            live_clip_manager,
            host,
            note_number,
            channel,
            device_id,
            beat_stamp,
            event_sequence,
            event_absolute_tick,
            realtime_now,
        );
    }

    // ── Sorting / reorder counting ────────────────────────────────────

    /// Port of C# MidiInputController.HasBeatStamp (static).
    /// True when beat_stamp is a valid finite value.
    /// In Rust: Some(v) where v.is_finite() = valid, None = no beat stamp.
    pub fn has_beat_stamp(beat_stamp: Option<f32>) -> bool {
        match beat_stamp {
            None => false,
            Some(v) => v.is_finite(),
        }
    }

    /// Port of C# MidiInputController.CountSameTickReorders (static).
    /// Counts NoteOff events that appear AFTER a NoteOn at the same absolute_tick
    /// in the unsorted buffer, indicating that a reorder will be applied.
    fn count_same_tick_reorders(events: &[MidiNoteEvent]) -> i32 {
        let mut reorders = 0i32;
        let mut current_tick = i32::MIN;
        let mut saw_note_on_at_tick = false;

        for evt in events {
            if evt.absolute_tick != current_tick {
                current_tick = evt.absolute_tick;
                saw_note_on_at_tick = false;
            }

            if evt.event_type == MidiNoteEventType::NoteOn {
                saw_note_on_at_tick = true;
            } else if evt.event_type == MidiNoteEventType::NoteOff && saw_note_on_at_tick {
                reorders += 1;
            }
        }

        reorders
    }
}

/// Port of C# MidiInputController.CompareNativeClockEvents (static).
/// Deterministic sort: 1) absolute_tick, 2) NoteOff before NoteOn on same tick, 3) sequence.
fn compare_native_clock_events(a: &MidiNoteEvent, b: &MidiNoteEvent) -> std::cmp::Ordering {
    let tick_cmp = a.absolute_tick.cmp(&b.absolute_tick);
    if tick_cmp != std::cmp::Ordering::Equal {
        return tick_cmp;
    }

    let a_type_rank = match a.event_type {
        MidiNoteEventType::NoteOff => 0,
        MidiNoteEventType::NoteOn => 1,
        MidiNoteEventType::Unknown => 2,
    };
    let b_type_rank = match b.event_type {
        MidiNoteEventType::NoteOff => 0,
        MidiNoteEventType::NoteOn => 1,
        MidiNoteEventType::Unknown => 2,
    };
    let type_cmp = a_type_rank.cmp(&b_type_rank);
    if type_cmp != std::cmp::Ordering::Equal {
        return type_cmp;
    }

    a.sequence.cmp(&b.sequence)
}

impl Default for MidiInputController {
    fn default() -> Self {
        Self::new()
    }
}
