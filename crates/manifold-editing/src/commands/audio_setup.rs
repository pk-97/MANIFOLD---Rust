//! Undoable commands for the project's [`AudioSetup`] — the audio-modulation
//! input routing. Edits route through `EditingService` like every other
//! project mutation. See `docs/AUDIO_MODULATION_DESIGN.md`.
//!
//! Sends are addressed by [`AudioSendId`] (stable identity), not by index, so a
//! command stays correct even if the send list is reordered between capture and
//! apply. `AddAudioSendCommand` is the exception — it carries the whole send
//! (id minted at construction) so execute/undo are deterministic.

use crate::command::Command;
use manifold_core::audio_setup::{AudioDeviceRef, AudioSend, SendAnalysisConfig};
use manifold_core::id::{AudioSendId, LayerId};
use manifold_core::project::Project;

/// Set (or clear) the input device. `None` = system default input.
#[derive(Debug)]
pub struct SetAudioInputDeviceCommand {
    old: Option<AudioDeviceRef>,
    new: Option<AudioDeviceRef>,
}

impl SetAudioInputDeviceCommand {
    pub fn new(old: Option<AudioDeviceRef>, new: Option<AudioDeviceRef>) -> Self {
        Self { old, new }
    }
}

impl Command for SetAudioInputDeviceCommand {
    fn execute(&mut self, project: &mut Project) {
        project.audio_setup.device = self.new.clone();
    }

    fn undo(&mut self, project: &mut Project) {
        project.audio_setup.device = self.old.clone();
    }

    fn description(&self) -> &str {
        "Set Audio Input Device"
    }
}

/// Add a send. The send (with its minted id) is supplied by the caller.
#[derive(Debug)]
pub struct AddAudioSendCommand {
    send: AudioSend,
}

impl AddAudioSendCommand {
    pub fn new(send: AudioSend) -> Self {
        Self { send }
    }
}

impl Command for AddAudioSendCommand {
    fn execute(&mut self, project: &mut Project) {
        project.audio_setup.sends.push(self.send.clone());
    }

    fn undo(&mut self, project: &mut Project) {
        let id = self.send.id.clone();
        project.audio_setup.sends.retain(|s| s.id != id);
    }

    fn description(&self) -> &str {
        "Add Audio Send"
    }
}

/// Remove a send by id. Captures the send and its position on execute so undo
/// restores it at the same index (send order is the worker index).
///
/// Removing a send does not touch sliders that reference it — their modulation
/// goes inert (the id no longer resolves) and can be re-pointed, matching the
/// orphan policy for drivers/envelopes.
#[derive(Debug)]
pub struct RemoveAudioSendCommand {
    id: AudioSendId,
    removed: Option<(usize, AudioSend)>,
}

impl RemoveAudioSendCommand {
    pub fn new(id: AudioSendId) -> Self {
        Self { id, removed: None }
    }
}

impl Command for RemoveAudioSendCommand {
    fn execute(&mut self, project: &mut Project) {
        let sends = &mut project.audio_setup.sends;
        if let Some(pos) = sends.iter().position(|s| s.id == self.id) {
            let send = sends.remove(pos);
            self.removed = Some((pos, send));
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((pos, send)) = self.removed.take() {
            let sends = &mut project.audio_setup.sends;
            let at = pos.min(sends.len());
            sends.insert(at, send);
        }
    }

    fn description(&self) -> &str {
        "Remove Audio Send"
    }
}

/// Rename a send.
#[derive(Debug)]
pub struct RenameAudioSendCommand {
    id: AudioSendId,
    old_label: String,
    new_label: String,
}

impl RenameAudioSendCommand {
    pub fn new(id: AudioSendId, old_label: String, new_label: String) -> Self {
        Self { id, old_label, new_label }
    }
}

impl Command for RenameAudioSendCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(s) = project.audio_setup.find_send_mut(&self.id) {
            s.label = self.new_label.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(s) = project.audio_setup.find_send_mut(&self.id) {
            s.label = self.old_label.clone();
        }
    }

    fn description(&self) -> &str {
        "Rename Audio Send"
    }
}

/// Set a send's input channels (the channels downmixed to mono for analysis).
#[derive(Debug)]
pub struct SetAudioSendChannelsCommand {
    id: AudioSendId,
    old: Vec<u16>,
    new: Vec<u16>,
}

impl SetAudioSendChannelsCommand {
    pub fn new(id: AudioSendId, old: Vec<u16>, new: Vec<u16>) -> Self {
        Self { id, old, new }
    }
}

impl Command for SetAudioSendChannelsCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(s) = project.audio_setup.find_send_mut(&self.id) {
            s.channels = self.new.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(s) = project.audio_setup.find_send_mut(&self.id) {
            s.channels = self.old.clone();
        }
    }

    fn description(&self) -> &str {
        "Set Audio Send Channels"
    }
}

/// Set a send's input gain trim (decibels). Applied live by the analysis
/// worker without restarting capture — a calibration knob, not structural.
#[derive(Debug)]
pub struct SetAudioSendGainCommand {
    id: AudioSendId,
    old_db: f32,
    new_db: f32,
}

impl SetAudioSendGainCommand {
    pub fn new(id: AudioSendId, old_db: f32, new_db: f32) -> Self {
        Self { id, old_db, new_db }
    }
}

impl Command for SetAudioSendGainCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(s) = project.audio_setup.find_send_mut(&self.id) {
            s.gain_db = self.new_db;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(s) = project.audio_setup.find_send_mut(&self.id) {
            s.gain_db = self.old_db;
        }
    }

    fn description(&self) -> &str {
        "Set Audio Send Gain"
    }
}

/// Set a send's pre-analysis noise floor (dB). The analyzer gates bins below it
/// before scope display + feature/transient extraction — a per-send squelch.
/// Applied live by the runtime without restarting capture, like
/// [`SetAudioSendGainCommand`].
#[derive(Debug)]
pub struct SetAudioSendFloorCommand {
    id: AudioSendId,
    old_db: f32,
    new_db: f32,
}

impl SetAudioSendFloorCommand {
    pub fn new(id: AudioSendId, old_db: f32, new_db: f32) -> Self {
        Self { id, old_db, new_db }
    }
}

impl Command for SetAudioSendFloorCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(s) = project.audio_setup.find_send_mut(&self.id) {
            s.floor_db = self.new_db;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(s) = project.audio_setup.find_send_mut(&self.id) {
            s.floor_db = self.old_db;
        }
    }

    fn description(&self) -> &str {
        "Set Audio Send Floor"
    }
}

/// Set the global Low/Mid/High crossover frequencies (Hz) — the band splits the
/// analysis worker reads and the spectrogram draws as divider lines. One command
/// captures both so a drag on either line is a single undo step. Applied live by
/// the worker without restarting capture, like [`SetAudioSendGainCommand`].
#[derive(Debug)]
pub struct SetAudioCrossoversCommand {
    old: (f32, f32),
    new: (f32, f32),
}

impl SetAudioCrossoversCommand {
    /// `old`/`new` are `(low_hz, mid_hz)` pairs.
    pub fn new(old: (f32, f32), new: (f32, f32)) -> Self {
        Self { old, new }
    }
}

impl Command for SetAudioCrossoversCommand {
    fn execute(&mut self, project: &mut Project) {
        project.audio_setup.low_hz = self.new.0;
        project.audio_setup.mid_hz = self.new.1;
    }

    fn undo(&mut self, project: &mut Project) {
        project.audio_setup.low_hz = self.old.0;
        project.audio_setup.mid_hz = self.old.1;
    }

    fn description(&self) -> &str {
        "Set Audio Crossovers"
    }
}

/// Set a send's analysis config (which extractors run for it).
#[derive(Debug)]
pub struct SetAudioSendAnalysisCommand {
    id: AudioSendId,
    old: SendAnalysisConfig,
    new: SendAnalysisConfig,
}

impl SetAudioSendAnalysisCommand {
    pub fn new(id: AudioSendId, old: SendAnalysisConfig, new: SendAnalysisConfig) -> Self {
        Self { id, old, new }
    }
}

impl Command for SetAudioSendAnalysisCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(s) = project.audio_setup.find_send_mut(&self.id) {
            s.analysis = self.new.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(s) = project.audio_setup.find_send_mut(&self.id) {
            s.analysis = self.old.clone();
        }
    }

    fn description(&self) -> &str {
        "Set Audio Send Analysis"
    }
}

// The send-owned Triggers matrix's whole-vec editing command is deleted
// (P3, D2): `AudioSend.triggers` is deserialize-only legacy now (P2), never
// written. Clip triggers are authored on the layer only, through
// `Add/Remove/SetLayerClipTriggerCommand` (`commands/layer.rs`).

/// Route an audio **layer** to feed a send (or to feed none). A layer feeds at
/// most one send, so this moves the layer off whatever send it fed before —
/// additively: the target send keeps its capture flag and other layers, so a
/// default send becomes a capture+layer mix. Layer-centric because that's how the
/// layer header edits routing ("this layer → which send"). Undo restores the
/// layer to its prior send.
#[derive(Debug)]
pub struct SetLayerAudioSendCommand {
    layer: LayerId,
    new_send: Option<AudioSendId>,
    /// The send this layer fed before (captured on first execute), for undo.
    old_send: Option<AudioSendId>,
    captured: bool,
}

impl SetLayerAudioSendCommand {
    pub fn new(layer: LayerId, new_send: Option<AudioSendId>) -> Self {
        Self { layer, new_send, old_send: None, captured: false }
    }
}

impl Command for SetLayerAudioSendCommand {
    fn execute(&mut self, project: &mut Project) {
        let setup = &mut project.audio_setup;
        if !self.captured {
            self.old_send = setup.send_for_layer(&self.layer).map(|s| s.id.clone());
            self.captured = true;
        }
        match &self.new_send {
            // bind_send_to_layer already detaches the layer from any other send.
            Some(send) => {
                setup.bind_send_to_layer(send, self.layer.clone());
            }
            None => setup.unbind_layer(&self.layer),
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let setup = &mut project.audio_setup;
        match &self.old_send {
            Some(send) => {
                setup.bind_send_to_layer(send, self.layer.clone());
            }
            None => setup.unbind_layer(&self.layer),
        }
    }

    fn description(&self) -> &str {
        "Set Layer Audio Send"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_then_undo_removes_the_send() {
        let mut project = Project::default();
        let send = AudioSend::new("Kick");
        let id = send.id.clone();
        let mut cmd = AddAudioSendCommand::new(send);

        cmd.execute(&mut project);
        assert!(project.audio_setup.find_send(&id).is_some());
        cmd.undo(&mut project);
        assert!(project.audio_setup.find_send(&id).is_none());
    }

    #[test]
    fn remove_undo_restores_at_same_index() {
        let mut project = Project::default();
        let a = AudioSend::new("A");
        let b = AudioSend::new("B");
        let c = AudioSend::new("C");
        let b_id = b.id.clone();
        project.audio_setup.sends = vec![a, b, c];

        let mut cmd = RemoveAudioSendCommand::new(b_id.clone());
        cmd.execute(&mut project);
        assert_eq!(project.audio_setup.sends.len(), 2);
        assert_eq!(project.audio_setup.send_index(&b_id), None);

        cmd.undo(&mut project);
        assert_eq!(project.audio_setup.send_index(&b_id), Some(1));
    }

    #[test]
    fn gain_round_trips() {
        let mut project = Project::default();
        let send = AudioSend::new("Bass");
        let id = send.id.clone();
        project.audio_setup.sends.push(send);

        let mut cmd = SetAudioSendGainCommand::new(id.clone(), 0.0, 6.0);
        cmd.execute(&mut project);
        assert_eq!(project.audio_setup.find_send(&id).unwrap().gain_db, 6.0);
        cmd.undo(&mut project);
        assert_eq!(project.audio_setup.find_send(&id).unwrap().gain_db, 0.0);
    }

    #[test]
    fn crossovers_round_trip() {
        let mut project = Project::default();
        // Defaults.
        assert_eq!(project.audio_setup.low_hz, 250.0);
        assert_eq!(project.audio_setup.mid_hz, 2000.0);

        let mut cmd = SetAudioCrossoversCommand::new((250.0, 2000.0), (180.0, 3500.0));
        cmd.execute(&mut project);
        assert_eq!(project.audio_setup.low_hz, 180.0);
        assert_eq!(project.audio_setup.mid_hz, 3500.0);
        cmd.undo(&mut project);
        assert_eq!(project.audio_setup.low_hz, 250.0);
        assert_eq!(project.audio_setup.mid_hz, 2000.0);
    }

    #[test]
    fn layer_send_routing_moves_and_undoes() {
        use manifold_core::id::LayerId;
        let mut project = Project::default();
        let a = AudioSend::new("A");
        let b = AudioSend::new("B");
        let a_id = a.id.clone();
        let b_id = b.id.clone();
        let layer = LayerId::new("L1");
        project.audio_setup.sends = vec![a, b];

        // Route the layer to A — A gains the layer (its device channels, if any,
        // stay; the two are summed).
        let mut c1 = SetLayerAudioSendCommand::new(layer.clone(), Some(a_id.clone()));
        c1.execute(&mut project);
        assert!(project.audio_setup.find_send(&a_id).unwrap().feeds_from_layer(&layer));

        // Route it to B — A loses the layer, B gains it.
        let mut c2 = SetLayerAudioSendCommand::new(layer.clone(), Some(b_id.clone()));
        c2.execute(&mut project);
        assert!(project.audio_setup.find_send(&b_id).unwrap().feeds_from_layer(&layer));
        assert!(!project.audio_setup.find_send(&a_id).unwrap().is_layer_fed());

        // Undo c2 restores the layer to A.
        c2.undo(&mut project);
        assert!(project.audio_setup.find_send(&a_id).unwrap().feeds_from_layer(&layer));
        assert!(!project.audio_setup.find_send(&b_id).unwrap().is_layer_fed());

        // Route to None detaches entirely.
        let mut c3 = SetLayerAudioSendCommand::new(layer.clone(), None);
        c3.execute(&mut project);
        assert!(project.audio_setup.send_for_layer(&layer).is_none());
    }


    // `triggers_round_trip` (the deleted matrix-editing command's test) is
    // deleted with the command (P3, D2). Clip-trigger round-trip coverage
    // lives in `manifold-editing::commands::layer` (P2) and
    // `manifold-io`/`manifold-playback`'s migration + evaluator tests.

    #[test]
    fn rename_round_trips() {
        let mut project = Project::default();
        let send = AudioSend::new("Old");
        let id = send.id.clone();
        project.audio_setup.sends.push(send);

        let mut cmd = RenameAudioSendCommand::new(id.clone(), "Old".into(), "New".into());
        cmd.execute(&mut project);
        assert_eq!(project.audio_setup.find_send(&id).unwrap().label, "New");
        cmd.undo(&mut project);
        assert_eq!(project.audio_setup.find_send(&id).unwrap().label, "Old");
    }
}
