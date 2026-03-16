use crate::command::Command;
use crate::commands::effect_target::{DriverTarget, with_effects_mut};
use manifold_core::project::Project;
use manifold_core::effects::ParameterDriver;
use manifold_core::types::{BeatDivision, DriverWaveform};

/// Resolve a DriverTarget to mutable access to the driver list.
fn with_drivers_mut<F, R>(project: &mut Project, target: &DriverTarget, f: F) -> Option<R>
where
    F: FnOnce(&mut Vec<ParameterDriver>) -> R,
{
    match target {
        DriverTarget::Effect { effect_target, effect_index } => {
            let eidx = *effect_index;
            with_effects_mut(project, effect_target, |effects, _groups| {
                if let Some(effect) = effects.get_mut(eidx) {
                    let drivers = effect.drivers_mut();
                    f(drivers)
                } else {
                    f(&mut Vec::new()) // fallback — shouldn't happen
                }
            })
        }
        DriverTarget::GeneratorParam { layer_index } => {
            let layer = project.timeline.layers.get_mut(*layer_index)?;
            let gp = layer.gen_params.get_or_insert_with(Default::default);
            let drivers = gp.drivers.get_or_insert_with(Vec::new);
            Some(f(drivers))
        }
    }
}

/// Add a driver.
#[derive(Debug)]
pub struct AddDriverCommand {
    target: DriverTarget,
    driver: ParameterDriver,
}

impl AddDriverCommand {
    pub fn new(target: DriverTarget, driver: ParameterDriver) -> Self {
        Self { target, driver }
    }
}

impl Command for AddDriverCommand {
    fn execute(&mut self, project: &mut Project) {
        let driver = self.driver.clone();
        with_drivers_mut(project, &self.target, |drivers| {
            drivers.push(driver);
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let param_index = self.driver.param_index;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(pos) = drivers.iter().position(|d| d.param_index == param_index) {
                drivers.remove(pos);
            }
        });
    }

    fn description(&self) -> &str { "Add Driver" }
}

/// Toggle driver enabled state.
#[derive(Debug)]
pub struct ToggleDriverEnabledCommand {
    target: DriverTarget,
    driver_index: usize,
    old_enabled: bool,
    new_enabled: bool,
}

impl ToggleDriverEnabledCommand {
    pub fn new(target: DriverTarget, driver_index: usize, old_enabled: bool, new_enabled: bool) -> Self {
        Self { target, driver_index, old_enabled, new_enabled }
    }
}

impl Command for ToggleDriverEnabledCommand {
    fn execute(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let val = self.new_enabled;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.enabled = val;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let val = self.old_enabled;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.enabled = val;
            }
        });
    }

    fn description(&self) -> &str { "Toggle Driver" }
}

/// Change driver beat division.
#[derive(Debug)]
pub struct ChangeDriverBeatDivCommand {
    target: DriverTarget,
    driver_index: usize,
    old_div: BeatDivision,
    new_div: BeatDivision,
}

impl ChangeDriverBeatDivCommand {
    pub fn new(target: DriverTarget, driver_index: usize, old_div: BeatDivision, new_div: BeatDivision) -> Self {
        Self { target, driver_index, old_div, new_div }
    }
}

impl Command for ChangeDriverBeatDivCommand {
    fn execute(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let val = self.new_div;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.beat_division = val;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let val = self.old_div;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.beat_division = val;
            }
        });
    }

    fn description(&self) -> &str { "Change Driver Beat Division" }
}

/// Change driver waveform.
#[derive(Debug)]
pub struct ChangeDriverWaveformCommand {
    target: DriverTarget,
    driver_index: usize,
    old_waveform: DriverWaveform,
    new_waveform: DriverWaveform,
}

impl ChangeDriverWaveformCommand {
    pub fn new(target: DriverTarget, driver_index: usize, old_waveform: DriverWaveform, new_waveform: DriverWaveform) -> Self {
        Self { target, driver_index, old_waveform, new_waveform }
    }
}

impl Command for ChangeDriverWaveformCommand {
    fn execute(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let val = self.new_waveform;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.waveform = val;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let val = self.old_waveform;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.waveform = val;
            }
        });
    }

    fn description(&self) -> &str { "Change Driver Waveform" }
}

/// Toggle driver reversed.
#[derive(Debug)]
pub struct ToggleDriverReversedCommand {
    target: DriverTarget,
    driver_index: usize,
    old_reversed: bool,
    new_reversed: bool,
}

impl ToggleDriverReversedCommand {
    pub fn new(target: DriverTarget, driver_index: usize, old_reversed: bool, new_reversed: bool) -> Self {
        Self { target, driver_index, old_reversed, new_reversed }
    }
}

impl Command for ToggleDriverReversedCommand {
    fn execute(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let val = self.new_reversed;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.reversed = val;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let val = self.old_reversed;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.reversed = val;
            }
        });
    }

    fn description(&self) -> &str { "Toggle Driver Reversed" }
}

/// Change driver trim range.
#[derive(Debug)]
pub struct ChangeTrimCommand {
    target: DriverTarget,
    driver_index: usize,
    old_min: f32,
    old_max: f32,
    new_min: f32,
    new_max: f32,
}

impl ChangeTrimCommand {
    pub fn new(target: DriverTarget, driver_index: usize, old_min: f32, old_max: f32, new_min: f32, new_max: f32) -> Self {
        Self { target, driver_index, old_min, old_max, new_min, new_max }
    }
}

impl Command for ChangeTrimCommand {
    fn execute(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let (min, max) = (self.new_min, self.new_max);
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.trim_min = min;
                d.trim_max = max;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let (min, max) = (self.old_min, self.old_max);
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.trim_min = min;
                d.trim_max = max;
            }
        });
    }

    fn description(&self) -> &str { "Change Trim" }
}
