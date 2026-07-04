//! Project-preset commands (Phase 4/5 fork ergonomics).
//!
//! A "fork" creates a project-embedded preset (a self-contained copy of a
//! preset's `EffectGraphDef`, minted under a fresh id) and retargets an instance
//! to it, so a per-instance recalibration becomes a named, shareable variant
//! rather than a hidden override. These wrap the `Project` primitives
//! (`fork_preset`, `embedded_preset`) in undoable [`Command`]s so the fork and
//! any subsequent preset-param edit ride the normal undo stack.

use manifold_core::GraphTarget;
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::preset_def::PresetKind;
use manifold_core::project::{EmbeddedPreset, Project};

use crate::command::Command;

/// Fork the preset behind the instance at `target`: register `source_def` as a
/// new project-embedded preset (id minted from its current id) and retarget the
/// instance to it, keeping its param values.
///
/// The caller (renderer-aware) supplies `source_def` — the preset's current
/// definition (catalog default for a pristine instance, or its diverged graph).
#[derive(Debug)]
pub struct ForkPresetCommand {
    target: GraphTarget,
    kind: PresetKind,
    source_def: EffectGraphDef,
    /// Re-seed the instance's `param_values` from `source_def`'s defaults after
    /// retargeting. False for Make Unique (the instance already runs this
    /// preset, so its values line up — keep them). True for Import, whose def
    /// has a *different* param structure: the old positional values no longer
    /// match the new bindings, so they must be replaced by the imported
    /// defaults (which also applies the imported preset's saved values).
    reseed_values: bool,
    /// Captured on first execute so undo restores the pre-fork preset id.
    old_type: Option<PresetTypeId>,
    /// Pre-fork `param_values`, captured on first execute when `reseed_values`
    /// is set so undo restores them (Make Unique never touches them).
    old_param_values: Option<Vec<manifold_core::effects::ParamSlot>>,
    /// The created embedded preset (with its minted id), captured on first
    /// execute so redo re-inserts the SAME preset deterministically.
    forked: Option<EmbeddedPreset>,
}

impl ForkPresetCommand {
    /// Make Unique: fork in place and keep the instance's current values.
    pub fn new(target: GraphTarget, kind: PresetKind, source_def: EffectGraphDef) -> Self {
        Self {
            target,
            kind,
            source_def,
            reseed_values: false,
            old_type: None,
            old_param_values: None,
            forked: None,
        }
    }

    /// Import: fork from a loaded `.manifoldpreset` def and re-seed the
    /// instance's `param_values` from it, replacing the prior (differently
    /// structured) values with the imported preset's saved ones.
    pub fn importing(target: GraphTarget, kind: PresetKind, source_def: EffectGraphDef) -> Self {
        Self {
            target,
            kind,
            source_def,
            reseed_values: true,
            old_type: None,
            old_param_values: None,
            forked: None,
        }
    }

    /// The minted fork id, available after `execute`.
    pub fn forked_id(&self) -> Option<&PresetTypeId> {
        self.forked.as_ref().and_then(|p| p.id())
    }
}

impl Command for ForkPresetCommand {
    fn execute(&mut self, project: &mut Project) {
        if self.forked.is_none() {
            self.old_type = project.instance_preset_id(&self.target);
            let base = self
                .source_def
                .preset_metadata
                .as_ref()
                .map(|m| m.id.as_str().to_string())
                .unwrap_or_else(|| "preset".to_string());
            let new_id = project.mint_forked_preset_id(&base);
            let mut def = self.source_def.clone();
            if let Some(m) = def.preset_metadata.as_mut() {
                m.id = new_id.clone();
                m.display_name = new_id.as_str().to_string();
            }
            self.forked = Some(EmbeddedPreset {
                kind: self.kind,
                def,
            });
        }
        let fp = self.forked.clone().expect("forked set above");
        let new_id = fp.id().cloned();
        project.upsert_embedded_preset(fp.clone());
        if let Some(id) = new_id {
            project.set_instance_preset_id(&self.target, id);
        }
        // Import re-seeds values from the (differently structured) imported def.
        // Capture the pre-fork values once for undo, then apply the imported
        // defaults. Make Unique skips this — its values already line up.
        if self.reseed_values
            && let Some(inst) = project.preset_instance_mut(&self.target)
        {
            if self.old_param_values.is_none() {
                self.old_param_values = Some(inst.param_values.clone());
            }
            inst.reseed_param_values_from_def(&fp.def);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(fp) = &self.forked
            && let Some(id) = fp.id().cloned()
        {
            project.remove_embedded_preset(&id);
        }
        if let Some(old) = self.old_type.clone() {
            project.set_instance_preset_id(&self.target, old);
        }
        if let Some(vals) = self.old_param_values.clone()
            && let Some(inst) = project.preset_instance_mut(&self.target)
        {
            inst.param_values = vals;
        }
    }

    fn description(&self) -> &str {
        "Fork Preset"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::{EffectGraphDef, ParamSpecDef, PresetMetadata};
    use manifold_core::macro_bank::MacroCurve;

    fn def_with_param(id: &str, param: &str, min: f32, max: f32) -> EffectGraphDef {
        EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: Some(id.to_string()),
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::from_string(id.to_string()),
                display_name: id.to_string(),
                category: String::new(),
                osc_prefix: String::new(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![ParamSpecDef {
                    id: param.to_string(),
                    name: param.to_string(),
                    min,
                    max,
                    default_value: min,
                    whole_numbers: false,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: Vec::new(),
                    format_string: None,
                    osc_suffix: String::new(),
                    curve: MacroCurve::Linear,
                    invert: false,
                }],
                bindings: Vec::new(),
                skip_mode: Default::default(),
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
            }),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    #[test]
    fn fork_command_forks_retargets_and_undoes() {
        let mut project = Project::default();
        let fx = manifold_core::effects::PresetInstance::new(PresetTypeId::OILY_FLUID);
        let fx_id = fx.id.clone();
        project.settings.master_effects.push(fx);
        let target = GraphTarget::Effect(fx_id.clone());

        let mut cmd = ForkPresetCommand::new(
            target.clone(),
            PresetKind::Generator,
            def_with_param("OilyFluid", "speed", 0.1, 4.0),
        );
        cmd.execute(&mut project);

        let new_id = cmd.forked_id().cloned().expect("forked id");
        assert_eq!(new_id.as_str(), "OilyFluid 2");
        assert_eq!(project.instance_preset_id(&target).as_ref(), Some(&new_id));
        let forked = project.embedded_preset(&new_id).expect("forked preset registered");
        assert_eq!(
            forked.def.preset_metadata.as_ref().unwrap().display_name,
            "OilyFluid 2",
            "minted name must be written to display_name AND id (D2)",
        );

        cmd.undo(&mut project);
        assert_eq!(
            project.instance_preset_id(&target),
            Some(PresetTypeId::OILY_FLUID)
        );
        assert!(project.embedded_preset(&new_id).is_none());

        // Redo reuses the same minted id.
        cmd.execute(&mut project);
        assert_eq!(project.instance_preset_id(&target).as_ref(), Some(&new_id));
    }

    #[test]
    fn import_reseeds_param_values_from_def_and_undo_restores() {
        use manifold_core::effects::ParamSlot;

        let mut project = Project::default();
        let mut fx = manifold_core::effects::PresetInstance::new(PresetTypeId::OILY_FLUID);
        // Prior card state: one value the user had dialed in. The imported
        // preset has a *different* saved default, so a correct import must
        // replace this, not keep it (the source of the black-render bug).
        fx.param_values = vec![ParamSlot::exposed(0.9)];
        let fx_id = fx.id.clone();
        project.settings.master_effects.push(fx);
        let target = GraphTarget::Effect(fx_id.clone());

        // Imported def carries a saved default of 2.0 for "speed".
        let mut cmd = ForkPresetCommand::importing(
            target.clone(),
            PresetKind::Generator,
            def_with_param("OilyFluid", "speed", 2.0, 8.0),
        );
        cmd.execute(&mut project);

        let inst = project.preset_instance(&target).expect("instance");
        assert_eq!(
            inst.param_values,
            vec![ParamSlot::exposed(2.0)],
            "import must re-seed param_values from the imported def's defaults",
        );

        cmd.undo(&mut project);
        let inst = project.preset_instance(&target).expect("instance");
        assert_eq!(
            inst.param_values,
            vec![ParamSlot::exposed(0.9)],
            "undo must restore the pre-import param_values",
        );
    }
}
