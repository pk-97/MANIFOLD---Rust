//! Undoable commands for the project's [`AudioSetup`] — the audio-modulation
//! input routing. Edits route through `EditingService` like every other
//! project mutation. See `docs/AUDIO_MODULATION_DESIGN.md`.
//!
//! Sends are addressed by [`AudioSendId`] (stable identity), not by index, so a
//! command stays correct even if the send list is reordered between capture and
//! apply. `AddAudioSendCommand` is the exception — it carries the whole send
//! (id minted at construction) so execute/undo are deterministic.

use crate::command::Command;
use manifold_core::audio_setup::{AudioSend, SendAnalysisConfig};
use manifold_core::id::AudioSendId;
use manifold_core::project::Project;

/// Set (or clear) the input device name.
#[derive(Debug)]
pub struct SetAudioInputDeviceCommand {
    old: Option<String>,
    new: Option<String>,
}

impl SetAudioInputDeviceCommand {
    pub fn new(old: Option<String>, new: Option<String>) -> Self {
        Self { old, new }
    }
}

impl Command for SetAudioInputDeviceCommand {
    fn execute(&mut self, project: &mut Project) {
        project.audio_setup.device_name = self.new.clone();
    }

    fn undo(&mut self, project: &mut Project) {
        project.audio_setup.device_name = self.old.clone();
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

/// Set a send's gain trim (dB).
#[derive(Debug)]
pub struct SetAudioSendGainCommand {
    id: AudioSendId,
    old: f32,
    new: f32,
}

impl SetAudioSendGainCommand {
    pub fn new(id: AudioSendId, old: f32, new: f32) -> Self {
        Self { id, old, new }
    }
}

impl Command for SetAudioSendGainCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(s) = project.audio_setup.find_send_mut(&self.id) {
            s.gain_db = self.new;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(s) = project.audio_setup.find_send_mut(&self.id) {
            s.gain_db = self.old;
        }
    }

    fn description(&self) -> &str {
        "Set Audio Send Gain"
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
