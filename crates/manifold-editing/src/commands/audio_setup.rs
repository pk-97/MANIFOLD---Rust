//! Undoable commands for the project's [`AudioSetup`] — the audio-modulation
//! input routing. Edits route through `EditingService` like every other
//! project mutation. See `docs/AUDIO_MODULATION_DESIGN.md`.
//!
//! Sends are addressed by [`AudioSendId`] (stable identity), not by index, so a
//! command stays correct even if the send list is reordered between capture and
//! apply. `AddAudioSendCommand` is the exception — it carries the whole send
//! (id minted at construction) so execute/undo are deterministic.

use crate::command::Command;
use manifold_core::audio_setup::{AudioDeviceRef, AudioSend, AudioSendSource, SendAnalysisConfig};
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

/// Route a send's signal source — capture downmix (the default) or a timeline
/// audio layer. Binding to a layer clears any other send that was pointing at
/// the same layer (one layer → one send); both the new binding and the cleared
/// one(s) are captured so undo restores the exact prior routing. This is the
/// single mutation path for the layer↔send binding the layer header edits.
#[derive(Debug)]
pub struct SetAudioSendSourceCommand {
    id: AudioSendId,
    new: AudioSendSource,
    /// The target send's source before this command (captured on execute).
    old_target: Option<AudioSendSource>,
    /// Other sends whose layer binding was cleared, with their prior source.
    cleared: Vec<(AudioSendId, AudioSendSource)>,
}

impl SetAudioSendSourceCommand {
    pub fn new(id: AudioSendId, new: AudioSendSource) -> Self {
        Self { id, new, old_target: None, cleared: Vec::new() }
    }

    /// Convenience: bind `send` to be fed by `layer`.
    pub fn to_layer(send: AudioSendId, layer: LayerId) -> Self {
        Self::new(send, AudioSendSource::Layer(layer))
    }

    /// Convenience: revert `send` to a capture source.
    pub fn to_capture(send: AudioSendId) -> Self {
        Self::new(send, AudioSendSource::Capture)
    }
}

impl Command for SetAudioSendSourceCommand {
    fn execute(&mut self, project: &mut Project) {
        let setup = &mut project.audio_setup;
        self.old_target = setup.find_send(&self.id).map(|s| s.source.clone());
        self.cleared.clear();
        match &self.new {
            AudioSendSource::Layer(layer) => {
                // Capture the sends that bind_send_to_layer will clear, for undo.
                for s in &setup.sends {
                    if s.id != self.id && s.layer_source() == Some(layer) {
                        self.cleared.push((s.id.clone(), s.source.clone()));
                    }
                }
                setup.bind_send_to_layer(&self.id, layer.clone());
            }
            AudioSendSource::Capture => {
                if let Some(s) = setup.find_send_mut(&self.id) {
                    s.source = AudioSendSource::Capture;
                }
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let setup = &mut project.audio_setup;
        for (id, src) in self.cleared.drain(..) {
            if let Some(s) = setup.find_send_mut(&id) {
                s.source = src;
            }
        }
        if let Some(old) = self.old_target.take()
            && let Some(s) = setup.find_send_mut(&self.id)
        {
            s.source = old;
        }
    }

    fn description(&self) -> &str {
        "Set Audio Send Source"
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
    fn send_source_binds_layer_and_clears_other_then_undoes() {
        use manifold_core::id::LayerId;
        let mut project = Project::default();
        let a = AudioSend::new("A");
        let b = AudioSend::new("B");
        let a_id = a.id.clone();
        let b_id = b.id.clone();
        let layer = LayerId::new("L1");
        project.audio_setup.sends = vec![a, b];

        // Bind A to the layer.
        let mut c1 = SetAudioSendSourceCommand::to_layer(a_id.clone(), layer.clone());
        c1.execute(&mut project);
        assert_eq!(project.audio_setup.find_send(&a_id).unwrap().layer_source(), Some(&layer));

        // Bind B to the SAME layer — A must be cleared back to capture.
        let mut c2 = SetAudioSendSourceCommand::to_layer(b_id.clone(), layer.clone());
        c2.execute(&mut project);
        assert_eq!(project.audio_setup.find_send(&b_id).unwrap().layer_source(), Some(&layer));
        assert!(!project.audio_setup.find_send(&a_id).unwrap().is_layer_fed());

        // Undo c2 restores A's binding and clears B.
        c2.undo(&mut project);
        assert_eq!(project.audio_setup.find_send(&a_id).unwrap().layer_source(), Some(&layer));
        assert!(!project.audio_setup.find_send(&b_id).unwrap().is_layer_fed());
    }

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
