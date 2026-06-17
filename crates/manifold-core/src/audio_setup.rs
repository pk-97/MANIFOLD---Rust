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

use crate::id::AudioSendId;
use crate::math::short_id;

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
    /// Which extractors run for this send.
    #[serde(default)]
    pub analysis: SendAnalysisConfig,
}

impl AudioSend {
    /// Create a new send with a freshly minted id and the given label.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            id: AudioSendId::new(short_id()),
            label: label.into(),
            channels: Vec::new(),
            analysis: SendAnalysisConfig::default(),
        }
    }
}

/// A reference to a chosen input device that survives reconnection and rename.
///
/// Identity is the platform **UID** (CoreAudio's stable device id); `name` is
/// for display and as a fallback match when the UID can't be resolved — a
/// project saved before UID identity, or a device whose UID changed. The app
/// resolves this to a live device through `manifold_audio::directory` at capture
/// time, so a renamed-but-same device still opens and a same-name-different
/// device is not silently bound. See `docs/AUDIO_INFRASTRUCTURE.md` §5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceRef {
    /// Stable platform UID. Empty only for a legacy name-only reference.
    #[serde(default)]
    pub uid: String,
    /// Display name + fallback match key.
    pub name: String,
}

impl AudioDeviceRef {
    pub fn new(uid: impl Into<String>, name: impl Into<String>) -> Self {
        Self { uid: uid.into(), name: name.into() }
    }

    /// The UID for resolution, or `None` if this is a legacy name-only ref.
    pub fn uid_opt(&self) -> Option<&str> {
        (!self.uid.is_empty()).then_some(self.uid.as_str())
    }
}

/// Project-level audio input configuration. See module docs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// Legacy pre-UID field: projects saved before UID identity stored only a
    /// device name under `deviceName`. Read on load and folded into [`device`]
    /// by [`Self::migrate_legacy_device`]; never serialized back.
    #[serde(default, rename = "deviceName", skip_serializing)]
    legacy_device_name: Option<String>,
}

impl AudioSetup {
    /// True when nothing is configured — lets the project skip serializing the
    /// field so existing fixtures round-trip byte-identically.
    pub fn is_empty(&self) -> bool {
        self.device.is_none() && self.legacy_device_name.is_none() && self.sends.is_empty()
    }

    /// Fold a legacy `deviceName` into a UID-less [`AudioDeviceRef`]. Idempotent;
    /// called once from `Project::on_after_deserialize`. The UID stays empty so
    /// resolution falls back to a name match until the user re-points the device
    /// (which mints a real UID) or it resolves and is re-saved.
    pub fn migrate_legacy_device(&mut self) {
        if self.device.is_none()
            && let Some(name) = self.legacy_device_name.take()
        {
            self.device = Some(AudioDeviceRef { uid: String::new(), name });
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
            legacy_device_name: None,
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
    fn new_uid_ref_takes_precedence_over_legacy() {
        let mut setup = AudioSetup {
            device: Some(AudioDeviceRef::new("uid-1", "Modern")),
            sends: vec![],
            legacy_device_name: Some("Legacy".into()),
        };
        setup.migrate_legacy_device();
        assert_eq!(setup.device.as_ref().unwrap().name, "Modern");
    }
}
