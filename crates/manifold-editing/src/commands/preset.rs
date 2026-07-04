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
use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset, Project};

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
                origin: EmbeddedOrigin::Saved,
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

/// Save to Project (PRESET_LIBRARY_DESIGN D4, P3): register `source_def` as a
/// new project-embedded preset (`origin: Saved`), WITHOUT retargeting any
/// instance. This is a "library door" — publishing the current effective
/// look as a new named entry the project's picker can hand out to any card —
/// not a divergence action. That distinguishes it from [`ForkPresetCommand`]
/// (Make Unique / Import), which both mints an entry AND retargets the
/// instance that triggered it; Save to Project only does the former.
#[derive(Debug)]
pub struct SaveToProjectCommand {
    kind: PresetKind,
    source_def: EffectGraphDef,
    /// The created embedded preset (with its minted id), captured on first
    /// execute so redo re-inserts the SAME preset deterministically (mirrors
    /// `ForkPresetCommand::forked`).
    saved: Option<EmbeddedPreset>,
}

impl SaveToProjectCommand {
    /// `source_def`'s `preset_metadata.display_name` (falling back to its
    /// `id`) is the mint base — the caller (the Save to Project UI action)
    /// stamps the user-typed name onto `source_def` before constructing this
    /// command, so the minted id/display_name reflect what the user typed.
    pub fn new(kind: PresetKind, source_def: EffectGraphDef) -> Self {
        Self { kind, source_def, saved: None }
    }

    /// The minted id, available after `execute`.
    pub fn saved_id(&self) -> Option<&PresetTypeId> {
        self.saved.as_ref().and_then(|p| p.id())
    }
}

impl Command for SaveToProjectCommand {
    fn execute(&mut self, project: &mut Project) {
        if self.saved.is_none() {
            let base = self
                .source_def
                .preset_metadata
                .as_ref()
                .map(|m| m.display_name.clone())
                .filter(|s| !s.is_empty())
                .or_else(|| self.source_def.preset_metadata.as_ref().map(|m| m.id.as_str().to_string()))
                .unwrap_or_else(|| "Preset".to_string());
            let new_id = mint_project_preset_name(project, &base);
            let mut def = self.source_def.clone();
            if let Some(m) = def.preset_metadata.as_mut() {
                m.id = new_id.clone();
                m.display_name = new_id.as_str().to_string();
            }
            self.saved = Some(EmbeddedPreset {
                kind: self.kind,
                def,
                origin: EmbeddedOrigin::Saved,
            });
        }
        let sp = self.saved.clone().expect("saved set above");
        project.upsert_embedded_preset(sp);
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(id) = self.saved.as_ref().and_then(|p| p.id().cloned()) {
            project.remove_embedded_preset(&id);
        }
    }

    fn description(&self) -> &str {
        "Save Preset to Project"
    }
}

/// Revert a diverged instance back to tracking its library entry
/// (PRESET_LIBRARY_DESIGN D3/§4, P4): clears the per-instance graph override
/// (`inst.graph = None`), undoable. The card/editor's "MODIFIED" badge is
/// exactly `graph.is_some()` (D3/§6.6 — no hashing), so this is the single
/// action that turns the badge back off.
///
/// Fails loud: if the instance's library id no longer resolves in the
/// catalog, `execute` is a no-op (the graph is left untouched) rather than
/// clearing it — reverting to nothing (a card with no params, D9's
/// "not-representable state") is worse than staying diverged. The
/// resolution check itself happens OUTSIDE this crate: `manifold-editing`
/// cannot depend on `manifold-renderer` (the JSON catalog's home) without
/// creating a dependency cycle — `manifold-playback` already depends on
/// `manifold-editing`, and `manifold-renderer` depends on
/// `manifold-playback`, so `manifold-editing -> manifold-renderer` would
/// close the loop. The caller (the UI/app layer, which has renderer access)
/// resolves "does this id still exist" once, at the moment the user clicks
/// Revert, and bakes the answer into `resolves_in_catalog` — exactly the
/// same pre-resolve-at-the-call-site pattern [`ForkPresetCommand`] already
/// uses for `source_def`, so the same fact replays identically on both the
/// UI-local project copy and the content thread's authoritative one.
#[derive(Debug)]
pub struct RevertToLibraryCommand {
    target: GraphTarget,
    /// Whether `target`'s library id resolved in the catalog at the moment
    /// this command was constructed. `false` makes `execute` a no-op + log.
    resolves_in_catalog: bool,
    /// Captured on execute (`inst.graph.take()`), restored on undo. Doubles
    /// as the "was there anything to revert" state via `Option::take` —
    /// redo re-executes against the value undo just restored, so no
    /// separate first-execute flag is needed.
    old_graph: Option<EffectGraphDef>,
}

impl RevertToLibraryCommand {
    pub fn new(target: GraphTarget, resolves_in_catalog: bool) -> Self {
        Self { target, resolves_in_catalog, old_graph: None }
    }
}

impl Command for RevertToLibraryCommand {
    fn execute(&mut self, project: &mut Project) {
        if !self.resolves_in_catalog {
            eprintln!(
                "[manifold-editing] RevertToLibrary: {} no longer resolves in the catalog; refusing to revert (staying diverged is safer than reverting to nothing)",
                self.target.label()
            );
            return;
        }
        let taken = project.with_preset_graph_mut(&self.target, |inst| {
            let prev = inst.graph_def_mut().take();
            inst.bump_graph_structure_version();
            prev
        });
        if let Some(prev) = taken {
            self.old_graph = prev;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        // `execute` was a no-op fail-loud refusal — the graph was never
        // touched, so undo must be a no-op too (an unconditional restore
        // here would WRITE `None` over an untouched `Some(diverged_def)`).
        if !self.resolves_in_catalog {
            return;
        }
        let restore = self.old_graph.take();
        project.with_preset_graph_mut(&self.target, |inst| {
            *inst.graph_def_mut() = restore;
            inst.bump_graph_structure_version();
        });
    }

    fn description(&self) -> &str {
        "Revert to Library"
    }
}

/// Mint a collision-free embedded-preset id from a user-TYPED name for Save
/// to Project: `base` itself if free, else `"{base} 2"`, `"{base} 3"`, ... —
/// deliberately NOT `Project::mint_forked_preset_id` (which always appends a
/// numeric suffix, even to a name with no collision at all — correct for
/// Make Unique/Import, whose base is always an EXISTING preset's own id, but
/// wrong here: a freshly typed name with no collision should be saved
/// verbatim, not surprise-suffixed "2"). Scoped to `project.embedded_presets`
/// only, matching `mint_forked_preset_id`/`mint_embedded_preset_id`'s
/// existing collision domain (not the global stock/user catalog — that
/// wider check is `UserLibrary::save`'s job for the Library door; extending
/// it here would be a scope change to already-shipped fork minting, not a
/// P3 concern).
fn mint_project_preset_name(project: &Project, base: &str) -> PresetTypeId {
    let taken = |candidate: &str| {
        project
            .embedded_presets
            .iter()
            .any(|p| p.id().map(|i| i.as_str()) == Some(candidate))
    };
    if !taken(base) {
        return PresetTypeId::from_string(base.to_string());
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base} {n}");
        if !taken(&candidate) {
            return PresetTypeId::from_string(candidate);
        }
        n += 1;
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

    // ── SaveToProjectCommand (PRESET_LIBRARY_DESIGN P3) ─────────────────

    #[test]
    fn save_to_project_upserts_saved_origin_and_round_trips() {
        let mut project = Project::default();
        let mut def = def_with_param("Bloom", "intensity", 0.0, 2.0);
        // Simulate the UI action stamping the user-typed name onto the
        // effective def before constructing the command.
        def.preset_metadata.as_mut().unwrap().display_name = "Sunset Glow".to_string();

        let mut cmd = SaveToProjectCommand::new(PresetKind::Effect, def);
        cmd.execute(&mut project);

        let id = cmd.saved_id().cloned().expect("saved id");
        assert_eq!(id.as_str(), "Sunset Glow", "a fresh name with no collision saves verbatim");

        let saved = project.embedded_preset(&id).expect("preset upserted into the project");
        assert_eq!(saved.kind, PresetKind::Effect);
        assert_eq!(saved.origin, EmbeddedOrigin::Saved);
        assert_eq!(saved.def.preset_metadata.as_ref().unwrap().display_name, "Sunset Glow");

        // Round-trip: re-import the (de)serialized def and confirm nothing
        // is lost (the calibration — the widened `intensity` range — is the
        // whole point of a saved look).
        let json = serde_json::to_string(&saved.def).expect("serialize");
        let back: EffectGraphDef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.preset_metadata.unwrap().params[0].max, 2.0);
    }

    #[test]
    fn save_to_project_does_not_retarget_any_instance() {
        let mut project = Project::default();
        let fx = manifold_core::effects::PresetInstance::new(PresetTypeId::BLOOM);
        let fx_id = fx.id.clone();
        project.settings.master_effects.push(fx);
        let target = GraphTarget::Effect(fx_id.clone());

        let def = def_with_param("Bloom", "intensity", 0.0, 2.0);
        let mut cmd = SaveToProjectCommand::new(PresetKind::Effect, def);
        cmd.execute(&mut project);

        // The instance that triggered the save keeps its ORIGINAL preset id
        // — Save to Project publishes a copy, it doesn't divert the card
        // that made it (unlike Make Unique / Import).
        assert_eq!(
            project.instance_preset_id(&target),
            Some(PresetTypeId::BLOOM),
            "Save to Project must not retarget the source instance"
        );
    }

    #[test]
    fn save_to_project_disambiguates_name_collision_and_undo_removes_it() {
        let mut project = Project::default();
        // Pre-existing embedded preset already named "Look".
        project.upsert_embedded_preset(EmbeddedPreset {
            kind: PresetKind::Generator,
            def: def_with_param("Look", "speed", 0.0, 1.0),
            origin: EmbeddedOrigin::Saved,
        });

        let mut def = def_with_param("OilyFluid", "speed", 0.0, 1.0);
        def.preset_metadata.as_mut().unwrap().display_name = "Look".to_string();
        let mut cmd = SaveToProjectCommand::new(PresetKind::Generator, def);
        cmd.execute(&mut project);

        let id = cmd.saved_id().cloned().expect("saved id");
        assert_eq!(id.as_str(), "Look 2", "a name collision must disambiguate to 'Name 2'");
        assert!(project.embedded_preset(&PresetTypeId::from_string("Look".to_string())).is_some());
        assert!(project.embedded_preset(&id).is_some());

        cmd.undo(&mut project);
        assert!(
            project.embedded_preset(&id).is_none(),
            "undo must remove only the newly-saved entry"
        );
        assert!(
            project.embedded_preset(&PresetTypeId::from_string("Look".to_string())).is_some(),
            "undo must not touch the pre-existing entry it disambiguated against"
        );
    }

    // ── RevertToLibraryCommand (PRESET_LIBRARY_DESIGN P4) ───────────────

    #[test]
    fn revert_to_library_clears_graph_and_undo_restores_it() {
        let mut project = Project::default();
        let mut fx = manifold_core::effects::PresetInstance::new(PresetTypeId::BLOOM);
        let diverged = def_with_param("Bloom", "intensity", 0.0, 5.0);
        fx.graph = Some(diverged.clone());
        let fx_id = fx.id.clone();
        project.settings.master_effects.push(fx);
        let target = GraphTarget::Effect(fx_id);

        let mut cmd = RevertToLibraryCommand::new(target.clone(), true);
        cmd.execute(&mut project);
        assert!(
            project.preset_instance(&target).unwrap().graph.is_none(),
            "execute must clear the per-instance graph override (tracking again)"
        );

        cmd.undo(&mut project);
        assert_eq!(
            project.preset_instance(&target).unwrap().graph.as_ref(),
            Some(&diverged),
            "undo must restore the exact diverged graph"
        );
    }

    #[test]
    fn revert_to_library_is_a_no_op_when_the_id_no_longer_resolves() {
        let mut project = Project::default();
        let mut fx = manifold_core::effects::PresetInstance::new(PresetTypeId::BLOOM);
        let diverged = def_with_param("Bloom", "intensity", 0.0, 5.0);
        fx.graph = Some(diverged.clone());
        let fx_id = fx.id.clone();
        project.settings.master_effects.push(fx);
        let target = GraphTarget::Effect(fx_id);

        // `resolves_in_catalog: false` simulates a deleted/renamed library
        // file — execute must refuse rather than orphan the card (reverting
        // to nothing is worse than staying diverged).
        let mut cmd = RevertToLibraryCommand::new(target.clone(), false);
        cmd.execute(&mut project);
        assert_eq!(
            project.preset_instance(&target).unwrap().graph.as_ref(),
            Some(&diverged),
            "a refused execute must leave the diverged graph untouched"
        );

        // undo after a refused execute must also be a no-op — nothing was
        // taken, so nothing may be written back (would otherwise clear a
        // graph `execute` never touched).
        cmd.undo(&mut project);
        assert_eq!(
            project.preset_instance(&target).unwrap().graph.as_ref(),
            Some(&diverged),
            "undo of a refused execute must not clear the untouched graph"
        );
    }
}
