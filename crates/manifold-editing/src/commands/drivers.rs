use crate::command::Command;
use crate::commands::effect_target::DriverTarget;
use manifold_core::effects::ParameterDriver;
use manifold_core::project::Project;
use manifold_core::types::{BeatDivision, DriverWaveform};

/// Resolve a DriverTarget to mutable access to the driver list.
fn with_drivers_mut<F, R>(project: &mut Project, target: &DriverTarget, f: F) -> Option<R>
where
    F: FnOnce(&mut Vec<ParameterDriver>) -> R,
{
    match target {
        DriverTarget::Effect { effect_id } => {
            let effect = project.find_effect_by_id_mut(effect_id)?;
            Some(f(effect.drivers_mut()))
        }
        DriverTarget::GeneratorParam { layer_id } => {
            let (_, layer) = project.timeline.find_layer_by_id_mut(layer_id)?;
            let gp = layer.gen_params_or_init();
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
        let param_id = self.driver.param_id.clone();
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(pos) = drivers.iter().position(|d| d.param_id == param_id) {
                drivers.remove(pos);
            }
        });
    }

    fn description(&self) -> &str {
        "Add Driver"
    }
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
    pub fn new(
        target: DriverTarget,
        driver_index: usize,
        old_enabled: bool,
        new_enabled: bool,
    ) -> Self {
        Self {
            target,
            driver_index,
            old_enabled,
            new_enabled,
        }
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

    fn description(&self) -> &str {
        "Toggle Driver"
    }
}

/// Change driver beat division. Selecting a division (grid / dotted / triplet)
/// also returns the driver to **sync mode** by clearing any free period, so the
/// grid and the type-in can't both claim the rate. `old_free` is captured so
/// undo restores a free-mode driver exactly.
#[derive(Debug)]
pub struct ChangeDriverBeatDivCommand {
    target: DriverTarget,
    driver_index: usize,
    old_div: BeatDivision,
    new_div: BeatDivision,
    old_free: Option<f32>,
}

impl ChangeDriverBeatDivCommand {
    pub fn new(
        target: DriverTarget,
        driver_index: usize,
        old_div: BeatDivision,
        new_div: BeatDivision,
        old_free: Option<f32>,
    ) -> Self {
        Self {
            target,
            driver_index,
            old_div,
            new_div,
            old_free,
        }
    }
}

impl Command for ChangeDriverBeatDivCommand {
    fn execute(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let val = self.new_div;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.beat_division = val;
                d.free_period_beats = None; // grid pick => sync mode
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let val = self.old_div;
        let free = self.old_free;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.beat_division = val;
                d.free_period_beats = free;
            }
        });
    }

    fn description(&self) -> &str {
        "Change Driver Beat Division"
    }
}

/// Set (or clear) the driver's free-running period in beats. `Some(p)` puts the
/// driver in **free mode** (cycles every `p` beats, ignoring the grid); `None`
/// returns it to sync mode. The type-in field writes this.
#[derive(Debug)]
pub struct SetDriverFreePeriodCommand {
    target: DriverTarget,
    driver_index: usize,
    old_free: Option<f32>,
    new_free: Option<f32>,
}

impl SetDriverFreePeriodCommand {
    pub fn new(
        target: DriverTarget,
        driver_index: usize,
        old_free: Option<f32>,
        new_free: Option<f32>,
    ) -> Self {
        Self {
            target,
            driver_index,
            old_free,
            new_free,
        }
    }
}

impl Command for SetDriverFreePeriodCommand {
    fn execute(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let val = self.new_free;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.free_period_beats = val;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let idx = self.driver_index;
        let val = self.old_free;
        with_drivers_mut(project, &self.target, |drivers| {
            if let Some(d) = drivers.get_mut(idx) {
                d.free_period_beats = val;
            }
        });
    }

    fn description(&self) -> &str {
        "Set Driver Free Period"
    }
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
    pub fn new(
        target: DriverTarget,
        driver_index: usize,
        old_waveform: DriverWaveform,
        new_waveform: DriverWaveform,
    ) -> Self {
        Self {
            target,
            driver_index,
            old_waveform,
            new_waveform,
        }
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

    fn description(&self) -> &str {
        "Change Driver Waveform"
    }
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
    pub fn new(
        target: DriverTarget,
        driver_index: usize,
        old_reversed: bool,
        new_reversed: bool,
    ) -> Self {
        Self {
            target,
            driver_index,
            old_reversed,
            new_reversed,
        }
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

    fn description(&self) -> &str {
        "Toggle Driver Reversed"
    }
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
    pub fn new(
        target: DriverTarget,
        driver_index: usize,
        old_min: f32,
        old_max: f32,
        new_min: f32,
        new_max: f32,
    ) -> Self {
        Self {
            target,
            driver_index,
            old_min,
            old_max,
            new_min,
            new_max,
        }
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

    fn description(&self) -> &str {
        "Change Trim"
    }
}
