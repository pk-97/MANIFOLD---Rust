//! Audio Setup — the project-level audio input configuration.
//!
//! The one place audio is routed into Manifold and split into **named sends**.
//! A slider's audio modulation references a send by [`AudioSendId`], never a raw
//! channel, so relabeling or re-patching a send updates every slider that uses
//! it in one place. The capture/analysis subsystem reads this to configure its
//! worker. Parallel to `midi_config` — input routing at the project root.
//!
//! See `docs/AUDIO_MODULATION_DESIGN.md` §3.2.

use serde::{Deserialize, Serialize};

use crate::id::{AudioSendId, LayerId};
use crate::math::short_id;

/// Where a send's signal comes from.
///
/// The default, [`Capture`](Self::Capture), is the historical meaning: the send
/// is a downmix of the chosen capture device's [`channels`](AudioSend::channels),
/// analyzed live by the worker. [`Layer`](Self::Layer) feeds the send from a
/// timeline audio layer instead — its features come from an offline curve sampled
/// at the playhead, not the live ring (see `docs/AUDIO_LAYER_DESIGN.md` §3). This
/// is the single source of truth for the layer↔send binding; the layer header's
/// send dropdown edits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AudioSendSource {
    /// Downmix of the capture device's channels (the original live path).
    #[default]
    Capture,
    /// Fed by a timeline audio layer, by its stable [`LayerId`].
    Layer(LayerId),
}

fn is_capture_source(s: &AudioSendSource) -> bool {
    matches!(s, AudioSendSource::Capture)
}

/// Per-send analysis configuration: which extractors run for this send.
///
/// Band energy is always computed (the cheap baseline feature). The flags here
/// gate the costlier extractors so they're **opt-in per send** — this is what
/// bounds worker cost, rather than paying for every analysis on every send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendAnalysisConfig {
    /// Onset / transient detection (v1). Cheap; on by default.
    #[serde(default = "default_true")]
    pub onset: bool,
    /// Synchrosqueeze pitch tracking → pitch / pitch-delta (v2). The expensive
    /// ridge-tracker path, off by default and enabled only on sends that need
    /// it (a clean monophonic source like an isolated bassline).
    #[serde(default)]
    pub pitch: bool,
}

fn default_true() -> bool {
    true
}

/// Default low/mid crossover (Hz). Matches the historical worker `LOW_HZ` const,
/// so projects saved before crossovers were editable analyse identically.
pub const DEFAULT_LOW_HZ: f32 = 250.0;
/// Default mid/high crossover (Hz). Matches the historical worker `MID_HZ`.
pub const DEFAULT_MID_HZ: f32 = 2000.0;
/// Smallest spacing between crossovers and the band edges, as a frequency ratio.
/// Keeps Low/Mid/High each at least this wide so a band never collapses to zero.
const CROSSOVER_MIN_RATIO: f32 = 1.1;
/// Absolute floor/ceiling for a crossover (Hz). The ceiling is conservative —
/// the analysed range tops out near Nyquist, but a sane upper bound avoids a
/// degenerate High band.
const CROSSOVER_MIN_HZ: f32 = 20.0;
const CROSSOVER_MAX_HZ: f32 = 18_000.0;

fn default_low_hz() -> f32 {
    DEFAULT_LOW_HZ
}

fn default_mid_hz() -> f32 {
    DEFAULT_MID_HZ
}

fn is_default_low_hz(v: &f32) -> bool {
    *v == DEFAULT_LOW_HZ
}

fn is_default_mid_hz(v: &f32) -> bool {
    *v == DEFAULT_MID_HZ
}

impl Default for SendAnalysisConfig {
    fn default() -> Self {
        Self { onset: true, pitch: false }
    }
}

/// A named audio send: a labeled tap on the input device. Routing (channels)
/// and analysis config live here; a slider's modulation only stores the
/// [`AudioSendId`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSend {
    /// Stable identity — what sliders reference. Never changes once minted.
    pub id: AudioSendId,
    /// User-facing name ("Kick", "Bass", "Vocals").
    pub label: String,
    /// Device input channels (0-based) downmixed to mono for analysis. Empty
    /// means the send produces silence until the user routes it.
    #[serde(default)]
    pub channels: Vec<u16>,
    /// Input gain trim in **decibels**, applied to the downmixed signal before
    /// analysis. Defaults to **0 dB (unity)** — the user is expected to route
    /// audio in at a sensible level, so gain is opt-in calibration, not a
    /// required step. Applied live by the worker (no capture restart on change).
    #[serde(default)]
    pub gain_db: f32,
    /// Which extractors run for this send.
    #[serde(default)]
    pub analysis: SendAnalysisConfig,
    /// Where the send's signal comes from — a capture downmix (default) or a
    /// timeline audio layer. Skipped on serialize when `Capture`, so pre-audio-
    /// layer project fixtures round-trip byte-identically.
    #[serde(default, skip_serializing_if = "is_capture_source")]
    pub source: AudioSendSource,
}

impl AudioSend {
    /// Create a new send with a freshly minted id and the given label.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            id: AudioSendId::new(short_id()),
            label: label.into(),
            channels: Vec::new(),
            gain_db: 0.0,
            analysis: SendAnalysisConfig::default(),
            source: AudioSendSource::Capture,
        }
    }

    /// The layer feeding this send, if it is a layer source.
    pub fn layer_source(&self) -> Option<&LayerId> {
        match &self.source {
            AudioSendSource::Layer(id) => Some(id),
            AudioSendSource::Capture => None,
        }
    }

    /// Whether this send is fed by a timeline audio layer (not capture).
    pub fn is_layer_fed(&self) -> bool {
        matches!(self.source, AudioSendSource::Layer(_))
    }

    /// Linear gain multiplier from the stored dB trim. 0 dB → 1.0 (unity).
    pub fn gain_linear(&self) -> f32 {
        10f32.powf(self.gain_db / 20.0)
    }
}

/// What an [`AudioDeviceRef`] points at — the kind of capture source.
///
/// The default, [`InputDevice`](Self::InputDevice), is the historical meaning: a
/// hardware, aggregate, or virtual **input** device addressed by its CoreAudio
/// UID. The tap kinds capture *rendered output* instead of a hardware input:
/// [`SystemAudio`](Self::SystemAudio) taps the whole system mix;
/// [`App`](Self::App) taps a single application's audio. The variant decides how
/// the runtime resolves the ref to a live capture backend, so it is the
/// authority — not the `uid`/`name` strings, which mean different things per
/// kind (see [`AudioDeviceRef`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AudioSourceKind {
    /// A hardware / aggregate / virtual input device, keyed by `uid`.
    #[default]
    InputDevice,
    /// The system-wide audio output mix, tapped (no per-process filter).
    SystemAudio,
    /// One application's audio output, tapped. `uid` holds the app's stable
    /// bundle id; `name` is the app's display name.
    App,
}

fn is_input_device_kind(k: &AudioSourceKind) -> bool {
    matches!(k, AudioSourceKind::InputDevice)
}

/// A reference to a chosen capture source that survives reconnection and rename.
///
/// For an [`InputDevice`](AudioSourceKind::InputDevice) (the default), identity
/// is the platform **UID** (CoreAudio's stable device id); `name` is for display
/// and as a fallback match when the UID can't be resolved — a project saved
/// before UID identity, or a device whose UID changed. For a tap source, `kind`
/// selects the capture path and the strings adapt: [`SystemAudio`] needs neither
/// (`name` is just the display label "System Audio"); [`App`] stores the app's
/// stable **bundle id** in `uid` and its display name in `name`, re-resolved to
/// a live process at capture time so an app that quits and relaunches re-binds.
///
/// The app resolves this through `manifold_audio::directory` at capture time, so
/// a renamed-but-same device still opens and a same-name-different device is not
/// silently bound. See `docs/AUDIO_INFRASTRUCTURE.md` §5.
///
/// [`SystemAudio`]: AudioSourceKind::SystemAudio
/// [`App`]: AudioSourceKind::App
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceRef {
    /// Stable identity: a device UID (`InputDevice`) or an app bundle id (`App`).
    /// Empty for `SystemAudio` and for a legacy name-only device reference.
    #[serde(default)]
    pub uid: String,
    /// Display name + fallback match key.
    pub name: String,
    /// Which kind of source this ref points at. Defaults to `InputDevice` and is
    /// skipped on serialize when so, keeping pre-tap project fixtures byte-identical.
    #[serde(default, skip_serializing_if = "is_input_device_kind")]
    pub kind: AudioSourceKind,
}

impl AudioDeviceRef {
    /// An input-device reference by UID + display name.
    pub fn new(uid: impl Into<String>, name: impl Into<String>) -> Self {
        Self { uid: uid.into(), name: name.into(), kind: AudioSourceKind::InputDevice }
    }

    /// The system-audio tap source (whole-system output mix).
    pub fn system_audio() -> Self {
        Self { uid: String::new(), name: "System Audio".to_string(), kind: AudioSourceKind::SystemAudio }
    }

    /// An application-audio tap source, keyed by stable bundle id + display name.
    pub fn app(bundle_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self { uid: bundle_id.into(), name: name.into(), kind: AudioSourceKind::App }
    }

    /// The UID for resolution, or `None` if this is a legacy name-only ref or a
    /// kind that carries no UID ([`SystemAudio`](AudioSourceKind::SystemAudio)).
    pub fn uid_opt(&self) -> Option<&str> {
        (!self.uid.is_empty()).then_some(self.uid.as_str())
    }

    /// Whether this ref points at a tap source (system or app) rather than a
    /// hardware input device. Tap sources expose a synthetic stereo channel
    /// layout instead of a hardware device's channels.
    pub fn is_tap(&self) -> bool {
        !matches!(self.kind, AudioSourceKind::InputDevice)
    }
}

/// Project-level audio input configuration. See module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSetup {
    /// Chosen input device. `None` = system default input. Remappable on load:
    /// if the saved device is absent at startup, the sends survive intact and
    /// capture stays dark until the user re-points it (the MIDI-port pattern),
    /// rather than silently binding to the wrong hardware.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<AudioDeviceRef>,
    /// The named sends, in declaration order. **Send order is significant**: it
    /// is the index the analysis worker keys feature frames by (see
    /// [`Self::send_index`]).
    #[serde(default)]
    pub sends: Vec<AudioSend>,
    /// Low/mid crossover frequency (Hz): the split between the Low and Mid
    /// analysis bands, and the lower divider line on the spectrogram. Global to
    /// all sends — Low/Mid/High mean one consistent thing everywhere. Applied
    /// live by the worker (no capture restart), like [`AudioSend::gain_db`].
    #[serde(default = "default_low_hz", skip_serializing_if = "is_default_low_hz")]
    pub low_hz: f32,
    /// Mid/high crossover frequency (Hz): the split between the Mid and High
    /// bands, and the upper divider line. See [`Self::low_hz`].
    #[serde(default = "default_mid_hz", skip_serializing_if = "is_default_mid_hz")]
    pub mid_hz: f32,
    /// Legacy pre-UID field: projects saved before UID identity stored only a
    /// device name under `deviceName`. Read on load and folded into [`device`]
    /// by [`Self::migrate_legacy_device`]; never serialized back.
    #[serde(default, rename = "deviceName", skip_serializing)]
    legacy_device_name: Option<String>,
}

impl Default for AudioSetup {
    fn default() -> Self {
        Self {
            device: None,
            sends: Vec::new(),
            low_hz: DEFAULT_LOW_HZ,
            mid_hz: DEFAULT_MID_HZ,
            legacy_device_name: None,
        }
    }
}

impl AudioSetup {
    /// True when nothing is configured — lets the project skip serializing the
    /// field so existing fixtures round-trip byte-identically. Default
    /// crossovers count as "nothing configured" (they serialize to nothing).
    pub fn is_empty(&self) -> bool {
        self.device.is_none()
            && self.legacy_device_name.is_none()
            && self.sends.is_empty()
            && is_default_low_hz(&self.low_hz)
            && is_default_mid_hz(&self.mid_hz)
    }

    /// Clamp a proposed `(low, mid)` crossover pair into the valid range:
    /// each within [`CROSSOVER_MIN_HZ`]..[`CROSSOVER_MAX_HZ`] and separated
    /// (from each other and the band edges) by at least [`CROSSOVER_MIN_RATIO`].
    /// Returns the sanitised pair; the dragged value is honoured first and the
    /// other is pushed only as far as needed to keep the spacing.
    pub fn clamp_crossovers(low: f32, mid: f32, dragging_low: bool) -> (f32, f32) {
        let lo_floor = CROSSOVER_MIN_HZ;
        let hi_ceil = CROSSOVER_MAX_HZ;
        if dragging_low {
            let low = low.clamp(lo_floor, hi_ceil / (CROSSOVER_MIN_RATIO * CROSSOVER_MIN_RATIO));
            let mid = mid.max(low * CROSSOVER_MIN_RATIO).min(hi_ceil);
            (low, mid)
        } else {
            let mid = mid.clamp(lo_floor * (CROSSOVER_MIN_RATIO * CROSSOVER_MIN_RATIO), hi_ceil);
            let low = low.min(mid / CROSSOVER_MIN_RATIO).max(lo_floor);
            (low, mid)
        }
    }

    /// Fold a legacy `deviceName` into a UID-less [`AudioDeviceRef`]. Idempotent;
    /// called once from `Project::on_after_deserialize`. The UID stays empty so
    /// resolution falls back to a name match until the user re-points the device
    /// (which mints a real UID) or it resolves and is re-saved.
    pub fn migrate_legacy_device(&mut self) {
        if self.device.is_none()
            && let Some(name) = self.legacy_device_name.take()
        {
            self.device = Some(AudioDeviceRef {
                uid: String::new(),
                name,
                kind: AudioSourceKind::InputDevice,
            });
        }
        self.legacy_device_name = None;
    }

    /// Display name of the chosen device, if any.
    pub fn device_display_name(&self) -> Option<&str> {
        self.device.as_ref().map(|d| d.name.as_str())
    }

    /// Find a send by id.
    pub fn find_send(&self, id: &AudioSendId) -> Option<&AudioSend> {
        self.sends.iter().find(|s| &s.id == id)
    }

    /// The send currently fed by `layer`, if any. One layer feeds at most one
    /// send — the reverse of [`AudioSendSource::Layer`].
    pub fn send_for_layer(&self, layer: &LayerId) -> Option<&AudioSend> {
        self.sends
            .iter()
            .find(|s| s.layer_source() == Some(layer))
    }

    /// Route `send` to be fed by `layer`, clearing any other send that was
    /// pointing at the same layer (one layer → one send). Returns `true` if a
    /// matching send existed and was (re)routed.
    pub fn bind_send_to_layer(&mut self, send: &AudioSendId, layer: LayerId) -> bool {
        for s in &mut self.sends {
            if s.layer_source() == Some(&layer) && &s.id != send {
                s.source = AudioSendSource::Capture;
            }
        }
        if let Some(s) = self.sends.iter_mut().find(|s| &s.id == send) {
            s.source = AudioSendSource::Layer(layer);
            true
        } else {
            false
        }
    }

    /// Detach any layer→send binding for `layer` (e.g. when the layer is deleted),
    /// reverting the affected send to a capture source.
    pub fn unbind_layer(&mut self, layer: &LayerId) {
        for s in &mut self.sends {
            if s.layer_source() == Some(layer) {
                s.source = AudioSendSource::Capture;
            }
        }
    }

    /// Find a send by id (mutable).
    pub fn find_send_mut(&mut self, id: &AudioSendId) -> Option<&mut AudioSend> {
        self.sends.iter_mut().find(|s| &s.id == id)
    }

    /// Position of a send by id. **This is the worker send index** the analysis
    /// crate keys feature frames by — send declaration order defines the
    /// `SendSpec` order handed to the worker, so resolving a slider's
    /// `AudioSendId` to a `FeatureFrame` lookup goes through here. `None` if the
    /// send was deleted (the referencing modulation is then inert).
    pub fn send_index(&self, id: &AudioSendId) -> Option<usize> {
        self.sends.iter().position(|s| &s.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_send_has_unique_stable_id() {
        let a = AudioSend::new("Kick");
        let b = AudioSend::new("Bass");
        assert_ne!(a.id, b.id);
        assert_eq!(a.label, "Kick");
    }

    #[test]
    fn send_index_tracks_declaration_order() {
        let mut setup = AudioSetup::default();
        let kick = AudioSend::new("Kick");
        let bass = AudioSend::new("Bass");
        let kick_id = kick.id.clone();
        let bass_id = bass.id.clone();
        setup.sends.push(kick);
        setup.sends.push(bass);

        assert_eq!(setup.send_index(&kick_id), Some(0));
        assert_eq!(setup.send_index(&bass_id), Some(1));

        // Removing the first send re-indexes the rest — callers resolve by id
        // each tick, so the slider following `bass_id` keeps working.
        setup.sends.remove(0);
        assert_eq!(setup.send_index(&kick_id), None);
        assert_eq!(setup.send_index(&bass_id), Some(0));
    }

    #[test]
    fn empty_setup_skips_serialization() {
        let setup = AudioSetup::default();
        assert!(setup.is_empty());
        let json = serde_json::to_string(&setup).unwrap();
        // device skipped (None), legacy field never serialized, sends empty.
        assert_eq!(json, r#"{"sends":[]}"#);
    }

    #[test]
    fn round_trips_through_json() {
        let mut setup = AudioSetup {
            device: Some(AudioDeviceRef::new("BlackHole16ch_UID", "BlackHole 16ch")),
            sends: vec![AudioSend::new("Bass")],
            ..Default::default()
        };
        setup.sends[0].channels = vec![2];
        setup.sends[0].analysis.pitch = true;

        let json = serde_json::to_string(&setup).unwrap();
        let back: AudioSetup = serde_json::from_str(&json).unwrap();
        assert_eq!(setup, back);
    }

    #[test]
    fn legacy_device_name_migrates_to_uidless_ref() {
        // A project saved before UID identity carries only `deviceName`.
        let json = r#"{"deviceName":"BlackHole 16ch","sends":[]}"#;
        let mut setup: AudioSetup = serde_json::from_str(json).unwrap();
        assert!(setup.device.is_none(), "not migrated until the hook runs");

        setup.migrate_legacy_device();
        let dev = setup.device.as_ref().expect("migrated device");
        assert_eq!(dev.name, "BlackHole 16ch");
        assert!(dev.uid.is_empty(), "legacy ref has no UID; resolves by name");
        assert_eq!(dev.uid_opt(), None);

        // Migration is idempotent and never re-serializes the legacy key.
        setup.migrate_legacy_device();
        let reser = serde_json::to_string(&setup).unwrap();
        assert!(!reser.contains("deviceName"));
    }

    #[test]
    fn default_crossovers_match_historical_consts() {
        let setup = AudioSetup::default();
        assert_eq!(setup.low_hz, 250.0);
        assert_eq!(setup.mid_hz, 2000.0);
        // Default crossovers must not break the empty round-trip.
        assert!(setup.is_empty());
        assert_eq!(serde_json::to_string(&setup).unwrap(), r#"{"sends":[]}"#);
    }

    #[test]
    fn old_project_without_crossovers_loads_defaults() {
        let json = r#"{"sends":[]}"#;
        let setup: AudioSetup = serde_json::from_str(json).unwrap();
        assert_eq!(setup.low_hz, 250.0);
        assert_eq!(setup.mid_hz, 2000.0);
    }

    #[test]
    fn non_default_crossovers_round_trip_and_are_not_empty() {
        let setup = AudioSetup {
            low_hz: 180.0,
            mid_hz: 3000.0,
            ..Default::default()
        };
        assert!(!setup.is_empty(), "edited crossovers must serialize");
        let json = serde_json::to_string(&setup).unwrap();
        assert!(json.contains("lowHz"));
        let back: AudioSetup = serde_json::from_str(&json).unwrap();
        assert_eq!(back.low_hz, 180.0);
        assert_eq!(back.mid_hz, 3000.0);
    }

    #[test]
    fn clamp_crossovers_keeps_low_below_mid() {
        // Dragging low up past mid pushes mid up to preserve spacing.
        let (low, mid) = AudioSetup::clamp_crossovers(5000.0, 2000.0, true);
        assert!(low < mid, "low {low} must stay below mid {mid}");
        assert!(mid >= low * 1.1);

        // Dragging mid down past low pushes low down.
        let (low, mid) = AudioSetup::clamp_crossovers(250.0, 100.0, false);
        assert!(low < mid, "low {low} must stay below mid {mid}");

        // Out-of-range values are floored/ceiled.
        let (low, _mid) = AudioSetup::clamp_crossovers(1.0, 2000.0, true);
        assert!(low >= 20.0);
        let (_low, mid) = AudioSetup::clamp_crossovers(250.0, 50_000.0, false);
        assert!(mid <= 18_000.0);
    }

    #[test]
    fn input_device_ref_round_trips_without_kind_key() {
        // A plain input-device ref must serialize byte-identically to the
        // pre-tap format — no `kind` key — so old fixtures round-trip.
        let dev = AudioDeviceRef::new("uid-1", "BlackHole 16ch");
        assert_eq!(dev.kind, AudioSourceKind::InputDevice);
        let json = serde_json::to_string(&dev).unwrap();
        assert!(!json.contains("kind"), "InputDevice kind must be skipped: {json}");
        let back: AudioDeviceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(dev, back);
    }

    #[test]
    fn legacy_device_ref_without_kind_loads_as_input_device() {
        let json = r#"{"uid":"uid-1","name":"BlackHole 16ch"}"#;
        let dev: AudioDeviceRef = serde_json::from_str(json).unwrap();
        assert_eq!(dev.kind, AudioSourceKind::InputDevice);
        assert!(!dev.is_tap());
    }

    #[test]
    fn tap_sources_round_trip_with_kind() {
        let sys = AudioDeviceRef::system_audio();
        assert_eq!(sys.kind, AudioSourceKind::SystemAudio);
        assert!(sys.is_tap());
        assert_eq!(sys.uid_opt(), None, "system audio carries no uid");
        let back: AudioDeviceRef = serde_json::from_str(&serde_json::to_string(&sys).unwrap()).unwrap();
        assert_eq!(sys, back);

        let app = AudioDeviceRef::app("com.ableton.live", "Ableton Live");
        assert_eq!(app.kind, AudioSourceKind::App);
        assert!(app.is_tap());
        assert_eq!(app.uid_opt(), Some("com.ableton.live"));
        let json = serde_json::to_string(&app).unwrap();
        assert!(json.contains("\"kind\":\"app\""), "kind serializes camelCase: {json}");
        let back: AudioDeviceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(app, back);
    }

    #[test]
    fn capture_source_is_skipped_layer_source_round_trips() {
        use crate::id::LayerId;
        // A default (capture) send must serialize without a `source` key, so
        // pre-audio-layer fixtures round-trip byte-identically.
        let send = AudioSend::new("Kick");
        assert_eq!(send.source, AudioSendSource::Capture);
        let json = serde_json::to_string(&send).unwrap();
        assert!(!json.contains("source"), "capture source must be skipped: {json}");

        // A layer-fed send round-trips with its LayerId.
        let mut layer_send = AudioSend::new("Bass");
        layer_send.source = AudioSendSource::Layer(LayerId::new("L7"));
        let json = serde_json::to_string(&layer_send).unwrap();
        assert!(json.contains("source"));
        let back: AudioSend = serde_json::from_str(&json).unwrap();
        assert_eq!(back.layer_source(), Some(&LayerId::new("L7")));
        assert!(back.is_layer_fed());
    }

    #[test]
    fn bind_send_to_layer_is_one_to_one() {
        use crate::id::LayerId;
        let mut setup = AudioSetup {
            sends: vec![AudioSend::new("A"), AudioSend::new("B")],
            ..Default::default()
        };
        let a = setup.sends[0].id.clone();
        let b = setup.sends[1].id.clone();
        let layer = LayerId::new("L1");

        setup.bind_send_to_layer(&a, layer.clone());
        assert_eq!(setup.send_for_layer(&layer).map(|s| &s.id), Some(&a));

        // Binding B to the same layer moves the binding (A reverts to capture).
        setup.bind_send_to_layer(&b, layer.clone());
        assert_eq!(setup.send_for_layer(&layer).map(|s| &s.id), Some(&b));
        assert!(!setup.find_send(&a).unwrap().is_layer_fed());

        // Unbinding the layer reverts B too.
        setup.unbind_layer(&layer);
        assert!(setup.send_for_layer(&layer).is_none());
    }

    #[test]
    fn new_uid_ref_takes_precedence_over_legacy() {
        let mut setup = AudioSetup {
            device: Some(AudioDeviceRef::new("uid-1", "Modern")),
            sends: vec![],
            legacy_device_name: Some("Legacy".into()),
            ..Default::default()
        };
        setup.migrate_legacy_device();
        assert_eq!(setup.device.as_ref().unwrap().name, "Modern");
    }
}
