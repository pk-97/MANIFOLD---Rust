//! Embedded-preset registry: minting, tracking, forking, and manifest reconciliation.

use super::*;

impl Project {
    /// The project-embedded preset with this id, if any.
    pub fn embedded_preset(&self, id: &PresetTypeId) -> Option<&EmbeddedPreset> {
        self.embedded_presets.iter().find(|p| p.id() == Some(id))
    }

    /// Mint an embedded-preset id derived from `base` (a `base#N` suffix probe)
    /// that collides with no existing embedded preset.
    pub fn mint_embedded_preset_id(&self, base: &str) -> PresetTypeId {
        let mut n = 1;
        loop {
            let candidate = format!("{base}#{n}");
            let taken = self
                .embedded_presets
                .iter()
                .any(|p| p.id().map(|i| i.as_str()) == Some(candidate.as_str()));
            if !taken {
                return PresetTypeId::from_string(candidate);
            }
            n += 1;
        }
    }

    /// Mint a human-readable, collision-free id for an explicit fork (Make
    /// Unique / Import — `ForkPresetCommand`) — a `base " {n}"` probe (e.g.
    /// `"Bloom 2"`) instead of `mint_embedded_preset_id`'s `base#{n}`. Starts
    /// at 2 so the first fork of a preset reads as its second instance, not
    /// literally "1" (D2: the design supersedes attempt #8's `#N` variant
    /// ids). The minted string is written to BOTH the embedded preset's id
    /// and its `display_name` — the id itself is now display-based, so the
    /// card can render it directly with no id-format parsing. Legacy `#N`
    /// ids already in a project keep resolving unchanged; this only changes
    /// what NEW forks mint.
    pub fn mint_forked_preset_id(&self, base: &str) -> PresetTypeId {
        let mut n = 2;
        loop {
            let candidate = format!("{base} {n}");
            let taken = self
                .embedded_presets
                .iter()
                .any(|p| p.id().map(|i| i.as_str()) == Some(candidate.as_str()));
            if !taken {
                return PresetTypeId::from_string(candidate);
            }
            n += 1;
        }
    }

    /// The current preset id of the instance addressed by `target`, if found.
    pub fn instance_preset_id(&self, target: &crate::GraphTarget) -> Option<PresetTypeId> {
        match target {
            crate::GraphTarget::Effect(effect_id) => {
                self.find_effect_by_id(effect_id).map(|fx| fx.effect_type().clone())
            }
            crate::GraphTarget::Generator(layer_id) => self
                .timeline
                .layers
                .iter()
                .find(|l| &l.layer_id == layer_id)
                .and_then(|l| l.gen_params())
                .map(|gp| gp.generator_type().clone()),
        }
    }

    /// Insert (or replace by id) a project-embedded preset.
    pub fn upsert_embedded_preset(&mut self, preset: EmbeddedPreset) {
        let id = preset.id().cloned();
        if let Some(id) = id {
            self.embedded_presets.retain(|p| p.id() != Some(&id));
        }
        self.embedded_presets.push(preset);
    }

    /// Remove a project-embedded preset by id. Returns it if present.
    pub fn remove_embedded_preset(&mut self, id: &PresetTypeId) -> Option<EmbeddedPreset> {
        let pos = self.embedded_presets.iter().position(|p| p.id() == Some(id))?;
        Some(self.embedded_presets.remove(pos))
    }

    /// Every `(id, kind)` referenced by a TRACKING instance (`graph: None`)
    /// anywhere in the project — master effects, every layer's effects,
    /// every clip's effects, and every layer's generator. A diverged
    /// instance (`graph: Some`) already carries its own private copy; its
    /// library id (still named by `effect_type`, D8) is not collected here
    /// because no self-containment snapshot is needed for it.
    ///
    /// Used at save time (PRESET_LIBRARY_DESIGN D5) to know which library
    /// ids need their current def cached into `embedded_presets` as
    /// `origin: Snapshot`. Renderer-free (reads only instance state), so it
    /// lives in core; the actual catalog lookup + upsert happens app-side
    /// (see `manifold-app::project_io::snapshot_and_prune_embedded_presets`),
    /// which has both this project AND the renderer's live catalog.
    pub fn tracking_preset_ids(&self) -> Vec<(PresetTypeId, PresetKind)> {
        fn collect(
            fx: &crate::effects::PresetInstance,
            kind: PresetKind,
            out: &mut Vec<(PresetTypeId, PresetKind)>,
        ) {
            if fx.graph.is_none() {
                out.push((fx.effect_type().clone(), kind));
            }
        }
        let mut out = Vec::new();
        for fx in &self.settings.master_effects {
            collect(fx, PresetKind::Effect, &mut out);
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                for fx in effects {
                    collect(fx, PresetKind::Effect, &mut out);
                }
            }
            for clip in &layer.clips {
                for fx in &clip.effects {
                    collect(fx, PresetKind::Effect, &mut out);
                }
            }
            if let Some(gp) = layer.gen_params() {
                collect(gp, PresetKind::Generator, &mut out);
            }
        }
        out
    }

    /// Mutable-walk sibling of [`Self::tracking_preset_ids`]: visits every
    /// `PresetInstance` home in the project — master effects, every layer's
    /// effects, every clip's effects, every layer's generator — INCLUDING
    /// diverged instances (`graph: Some`), unlike the read-only walk above.
    /// A diverged instance still deserializes its own `params` wire map and
    /// still needs it reconciled, so this walker doesn't filter by
    /// `graph.is_none()` the way `tracking_preset_ids` does.
    pub(super) fn for_each_preset_instance_mut(
        &mut self,
        mut f: impl FnMut(&mut crate::effects::PresetInstance),
    ) {
        for fx in &mut self.settings.master_effects {
            f(fx);
        }
        for layer in &mut self.timeline.layers {
            if let Some(effects) = layer.effects.as_mut() {
                for fx in effects {
                    f(fx);
                }
            }
            for clip in &mut layer.clips {
                for fx in &mut clip.effects {
                    f(fx);
                }
            }
            if let Some(gp) = layer.gen_params_mut() {
                f(gp);
            }
        }
    }

    /// Rebuild every instance's `ParamManifest` from its stashed wire entries
    /// against the CURRENT registry (PARAM_STORAGE_BOUNDARIES_DESIGN.md D1) —
    /// call after the project's embedded presets are installed. Idempotent:
    /// instances with no stash (freshly constructed, or already resolved by
    /// an earlier call) are untouched. Walks exactly the homes
    /// `tracking_preset_ids` walks (its mut sibling, above).
    ///
    /// Returns how many instances still have an unresolved preset template
    /// after this pass (`PresetInstance::template_unresolved`) — BUG-079:
    /// the loader folds this into `Project::load_report` so a missing
    /// preset def surfaces on-screen instead of only in an `eprintln`.
    pub fn reconcile_param_manifests(&mut self) -> usize {
        let mut unresolved = 0;
        self.for_each_preset_instance_mut(|fx| {
            fx.reconcile_manifest();
            if fx.template_unresolved() {
                unresolved += 1;
            }
        });
        unresolved
    }

    /// Remove every `Snapshot`-origin embedded preset whose id is not in
    /// `referenced` (D5) — the stale-snapshot prune that keeps the overlay
    /// from accumulating ids no tracking instance uses anymore (e.g. after
    /// an instance is retargeted or deleted). `Saved` entries are never
    /// touched here — they are a deliberate project-scoped fork, not
    /// save-time plumbing, and survive independent of what's referenced.
    pub fn prune_stale_snapshots(&mut self, referenced: &std::collections::HashSet<PresetTypeId>) {
        self.embedded_presets.retain(|p| {
            p.origin == EmbeddedOrigin::Saved || p.id().is_some_and(|id| referenced.contains(id))
        });
    }

    /// Retarget the instance addressed by `target` at a different preset id,
    /// keeping its param values. Returns `false` if the target wasn't found.
    pub fn set_instance_preset_id(
        &mut self,
        target: &crate::GraphTarget,
        id: PresetTypeId,
    ) -> bool {
        match target {
            crate::GraphTarget::Effect(effect_id) => {
                if let Some(fx) = self.find_effect_by_id_mut(effect_id) {
                    fx.set_preset_id(id);
                    return true;
                }
                false
            }
            crate::GraphTarget::Generator(layer_id) => {
                for layer in &mut self.timeline.layers {
                    if &layer.layer_id == layer_id {
                        if let Some(gp) = layer.gen_params_mut() {
                            gp.set_preset_id(id);
                            return true;
                        }
                        return false;
                    }
                }
                false
            }
        }
    }

    /// Fork: register `source_def` as a new project-embedded preset (id minted
    /// uniquely from its current id) and retarget the instance at `target` to
    /// it. Returns the new preset id, or `None` if the target wasn't found.
    /// The instance keeps its param values — a fork is a copy of the same
    /// preset under a new id, so the values stay valid.
    pub fn fork_preset(
        &mut self,
        target: &crate::GraphTarget,
        kind: PresetKind,
        mut source_def: EffectGraphDef,
    ) -> Option<PresetTypeId> {
        let base = source_def
            .preset_metadata
            .as_ref()
            .map(|m| m.id.as_str().to_string())
            .unwrap_or_else(|| "preset".to_string());
        let new_id = self.mint_embedded_preset_id(&base);
        if let Some(m) = source_def.preset_metadata.as_mut() {
            m.id = new_id.clone();
        }
        if !self.set_instance_preset_id(target, new_id.clone()) {
            return None;
        }
        self.embedded_presets.push(EmbeddedPreset {
            kind,
            def: source_def,
            origin: EmbeddedOrigin::Saved,
        });
        Some(new_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_support::*;
    use crate::PresetTypeId;
    use crate::effects::PresetInstance;

    #[test]
    fn fork_preset_mints_id_and_retargets_instance() {
        let mut p = Project::default();
        let fx = PresetInstance::new(PresetTypeId::BLOOM);
        let fx_id = fx.id.clone();
        p.settings.master_effects.push(fx);

        let target = crate::GraphTarget::Effect(fx_id.clone());
        let new_id = p
            .fork_preset(&target, PresetKind::Effect, graph_def_with_id("Bloom", "Bloom"))
            .expect("fork retargets an existing instance");

        // Minted a distinct project-scoped id.
        assert_eq!(new_id.as_str(), "Bloom#1");
        // The instance now points at the fork; the embedded preset exists.
        assert_eq!(p.find_effect_by_id(&fx_id).unwrap().effect_type(), &new_id);
        assert!(p.embedded_preset(&new_id).is_some());
        assert_eq!(p.embedded_preset(&new_id).unwrap().def.preset_metadata.as_ref().unwrap().id, new_id);

        // A second fork of the same base mints a fresh id.
        let fx2 = PresetInstance::new(PresetTypeId::BLOOM);
        let fx2_id = fx2.id.clone();
        p.settings.master_effects.push(fx2);
        let new_id2 = p
            .fork_preset(
                &crate::GraphTarget::Effect(fx2_id),
                PresetKind::Effect,
                graph_def_with_id("Bloom", "Bloom"),
            )
            .unwrap();
        assert_eq!(new_id2.as_str(), "Bloom#2");
        assert_eq!(p.embedded_presets.len(), 2);
    }

    #[test]
    fn mint_forked_preset_id_starts_at_2_and_probes_past_collisions() {
        let mut p = Project::default();
        // No embedded presets yet: first fork of "Bloom" reads as "Bloom 2",
        // not "Bloom 1" (D2 — a fork is presented as the preset's second
        // instance).
        assert_eq!(p.mint_forked_preset_id("Bloom").as_str(), "Bloom 2");

        p.embedded_presets.push(EmbeddedPreset {
            kind: PresetKind::Effect,
            def: graph_def_with_id("Bloom 2", "Bloom 2"),
            origin: EmbeddedOrigin::Saved,
        });
        // "Bloom 2" is taken — probes to "Bloom 3".
        assert_eq!(p.mint_forked_preset_id("Bloom").as_str(), "Bloom 3");
    }

    #[test]
    fn embedded_presets_round_trip_and_skip_when_empty() {
        // Empty by default → no `embeddedPresets` field on the wire (existing
        // fixtures stay byte-identical).
        let p = Project::default();
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("embeddedPresets"), "empty must be skipped: {json}");

        // A forked preset round-trips inside the project JSON.
        let mut p = Project::default();
        let mut def = crate::effect_graph_def::EffectGraphDef {
            version: crate::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: Some("Oily Fluid (Layer 2 variant)".to_string()),
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        };
        def.preset_metadata = Some(crate::effect_graph_def::PresetMetadata {
            id: PresetTypeId::from_string("OilyFluid#layer2".to_string()),
            display_name: "Oily Fluid (Layer 2 variant)".to_string(),
            category: String::new(),
            osc_prefix: String::new(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: Vec::new(),
            bindings: Vec::new(),
            skip_mode: Default::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        });
        p.embedded_presets.push(EmbeddedPreset {
            kind: PresetKind::Generator,
            def,
            origin: EmbeddedOrigin::Saved,
        });

        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("embeddedPresets"), "non-empty must serialize: {json}");
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.embedded_presets.len(), 1);
        assert_eq!(back.embedded_presets[0].kind, PresetKind::Generator);
        assert_eq!(back.embedded_presets[0].origin, EmbeddedOrigin::Saved);
        assert_eq!(
            back.embedded_presets[0].id().map(|i| i.as_str()),
            Some("OilyFluid#layer2")
        );
    }

    /// D5: `origin` round-trips for BOTH variants, and a legacy embedded
    /// preset with no `origin` field on the wire (pre-P2 project files)
    /// loads as `Saved` — the deliberate, on-top-of-disk default that
    /// matches what those files' entries always meant before `Snapshot`
    /// existed.
    #[test]
    fn embedded_preset_origin_round_trips_both_variants_and_defaults_legacy_to_saved() {
        let mut p = Project::default();
        p.embedded_presets.push(EmbeddedPreset {
            kind: PresetKind::Effect,
            def: graph_def_with_id("Bloom 2", "Bloom 2"),
            origin: EmbeddedOrigin::Saved,
        });
        p.embedded_presets.push(EmbeddedPreset {
            kind: PresetKind::Effect,
            def: graph_def_with_id("Bloom", "Bloom"),
            origin: EmbeddedOrigin::Snapshot,
        });

        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"origin\":\"Saved\""), "Saved must serialize: {json}");
        assert!(json.contains("\"origin\":\"Snapshot\""), "Snapshot must serialize: {json}");

        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.embedded_preset(&PresetTypeId::from_string("Bloom 2".to_string()))
                .unwrap()
                .origin,
            EmbeddedOrigin::Saved
        );
        assert_eq!(
            back.embedded_preset(&PresetTypeId::from_string("Bloom".to_string()))
                .unwrap()
                .origin,
            EmbeddedOrigin::Snapshot
        );

        // Legacy shape: an embedded preset JSON object with no `origin` key
        // at all (as every pre-P2 project file has) must default to Saved.
        // `origin` is the last field in `EmbeddedPreset`, so it serializes
        // with a LEADING comma and no trailing one.
        let legacy_json = json.replacen(",\"origin\":\"Snapshot\"", "", 1);
        assert_eq!(
            legacy_json.matches("\"origin\"").count(),
            1,
            "sanity: exactly the Snapshot entry's origin key must be gone \
             (the untouched Saved entry still carries its own): {legacy_json}"
        );
        let legacy_back: Project = serde_json::from_str(&legacy_json).unwrap();
        assert_eq!(
            legacy_back
                .embedded_preset(&PresetTypeId::from_string("Bloom".to_string()))
                .unwrap()
                .origin,
            EmbeddedOrigin::Saved,
            "no `origin` field on the wire must default to Saved"
        );
    }
}
