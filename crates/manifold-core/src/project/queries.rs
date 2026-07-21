//! Read-only lookups over the project: effects, preset instances, audio-mod/send/trigger consumers.

use super::*;

impl Project {
    /// Whether any layer has an enabled [`crate::audio_trigger::LayerClipTrigger`]
    /// — the P2 replacement for `AudioSend::has_active_triggers()`, which now
    /// only ever reads drained (always-empty) legacy storage.
    pub fn has_active_clip_triggers(&self) -> bool {
        self.timeline
            .layers
            .iter()
            .any(|l| l.clip_triggers.iter().any(|c| c.enabled))
    }

    /// Walk every effect list in the project (master, every layer's
    /// effects, every clip's effects) for an instance whose stable id
    /// matches `effect_id`. Returns the first match or `None`. Linear
    /// in total effect count; used by editor-canvas snapshotting and
    /// graph-mutation commands — not on the per-frame hot path.
    pub fn find_effect_by_id(
        &self,
        effect_id: &crate::id::EffectId,
    ) -> Option<&crate::effects::PresetInstance> {
        for fx in &self.settings.master_effects {
            if &fx.id == effect_id {
                return Some(fx);
            }
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                for fx in effects {
                    if &fx.id == effect_id {
                        return Some(fx);
                    }
                }
            }
            for clip in &layer.clips {
                for fx in &clip.effects {
                    if &fx.id == effect_id {
                        return Some(fx);
                    }
                }
            }
        }
        None
    }

    /// The [`PresetInstance`](crate::effects::PresetInstance) a
    /// [`GraphTarget`] resolves to — const twin of
    /// [`Self::with_preset_graph_mut`]. Effect by stable
    /// [`EffectId`](crate::id::EffectId); generator by its host layer's
    /// `gen_params`. `None` if the target doesn't resolve. The single const
    /// locate behind read-side per-target accessors (e.g. resolving a preset's
    /// graph def for fork / export).
    pub fn preset_instance(
        &self,
        target: &crate::GraphTarget,
    ) -> Option<&crate::effects::PresetInstance> {
        match target {
            crate::GraphTarget::Effect(eid) => self.find_effect_by_id(eid),
            crate::GraphTarget::Generator(lid) => self
                .timeline
                .layers
                .iter()
                .find(|l| &l.layer_id == lid)
                .and_then(|l| l.gen_params()),
        }
    }

    /// Mutable variant of [`Self::preset_instance`]. Resolves the effect or
    /// generator instance behind a [`GraphTarget`] for in-place edits (e.g.
    /// re-seeding `param_values` after a fork/import retarget).
    pub fn preset_instance_mut(
        &mut self,
        target: &crate::GraphTarget,
    ) -> Option<&mut crate::effects::PresetInstance> {
        match target {
            crate::GraphTarget::Effect(eid) => self.find_effect_by_id_mut(eid),
            crate::GraphTarget::Generator(lid) => self
                .timeline
                .layers
                .iter_mut()
                .find(|l| &l.layer_id == lid)
                .and_then(|l| l.gen_params_mut()),
        }
    }

    /// Mutable variant of [`Self::find_effect_by_id`]. Used by
    /// graph-mutation commands to apply edits to the matching
    /// instance in place.
    pub fn find_effect_by_id_mut(
        &mut self,
        effect_id: &crate::id::EffectId,
    ) -> Option<&mut crate::effects::PresetInstance> {
        for fx in &mut self.settings.master_effects {
            if &fx.id == effect_id {
                return Some(fx);
            }
        }
        for layer in &mut self.timeline.layers {
            if let Some(effects) = layer.effects.as_mut() {
                for fx in effects {
                    if &fx.id == effect_id {
                        return Some(fx);
                    }
                }
            }
            for clip in &mut layer.clips {
                for fx in &mut clip.effects {
                    if &fx.id == effect_id {
                        return Some(fx);
                    }
                }
            }
        }
        None
    }

    /// The owning layer for an effect/clip-effect instance, or `None` for a
    /// master-chain effect (or an unknown id). Used where an `EffectId` needs
    /// its container — e.g. labelling an EffectId-addressed macro mapping.
    pub fn layer_id_for_effect(&self, effect_id: &crate::id::EffectId) -> Option<crate::id::LayerId> {
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref()
                && effects.iter().any(|fx| &fx.id == effect_id)
            {
                return Some(layer.layer_id.clone());
            }
            for clip in &layer.clips {
                if clip.effects.iter().any(|fx| &fx.id == effect_id) {
                    return Some(layer.layer_id.clone());
                }
            }
        }
        None
    }

    /// Whether any enabled audio modulation exists anywhere in the project —
    /// the gate the content thread uses to decide whether to run audio capture
    /// at all. Walks the same instance set the modulation pipeline evaluates:
    /// master effects, every layer's effects, and every layer's generator
    /// instance (NOT clip effects, which the pipeline neither resets nor
    /// modulates). Cheap — most instances carry no audio mods, short-circuiting
    /// on the `Option`.
    /// Send ids with at least one ENABLED audio mod reading `Pitch` or
    /// `Presence` — the D7 activation set (docs/AUDIO_OBJECT_TRACKING_DESIGN.md
    /// P4). The audio-mod runtime recomputes this only on a data-version
    /// change and switches each send analyzer's ridge tracker on/off with it,
    /// so projects that never bind pitch pay nothing (the tracker path is
    /// byte-identical when off — tested in manifold-audio).
    pub fn sends_with_pitch_mods(&self) -> ahash::AHashSet<crate::AudioSendId> {
        let mut out = ahash::AHashSet::new();
        let mut collect = |fx: &crate::effects::PresetInstance| {
            if let Some(mods) = fx.audio_mods.as_ref() {
                for m in mods.iter().filter(|m| m.enabled) {
                    if matches!(
                        m.source.feature.kind,
                        crate::audio_mod::AudioFeatureKind::Pitch | crate::audio_mod::AudioFeatureKind::Presence
                    ) {
                        out.insert(m.source.send_id.clone());
                    }
                }
            }
        };
        for fx in &self.settings.master_effects {
            collect(fx);
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                for fx in effects {
                    collect(fx);
                }
            }
            if let Some(gp) = layer.gen_params() {
                collect(gp);
            }
        }
        out
    }

    pub fn has_active_audio_mods(&self) -> bool {
        fn inst_has(fx: &crate::effects::PresetInstance) -> bool {
            // §9 U4: a fire-mode (trigger-gate) mod is a normal `audio_mods`
            // entry now — no separate `audio_trigger` config to special-case,
            // so this plain check already covers it.
            fx.audio_mods
                .as_ref()
                .is_some_and(|v| v.iter().any(|a| a.enabled))
        }
        if self.settings.master_effects.iter().any(inst_has) {
            return true;
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref()
                && effects.iter().any(inst_has)
            {
                return true;
            }
            if layer.gen_params().is_some_and(inst_has) {
                return true;
            }
        }
        false
    }

    /// Send ids the analysis runtime should actually spend cycles on: every
    /// send with at least one ENABLED audio mod reading it, plus every send
    /// with at least one enabled `LayerClipTrigger` sourcing it. Walks the
    /// same instance set [`Self::has_active_audio_mods`] does (master
    /// effects, layer effects, layer generator params — NOT clip effects,
    /// mirroring that function), plus every layer's `clip_triggers` (P2 —
    /// the §3.4 walker arm; a send-owned `AudioSend::triggers` is drained
    /// legacy storage now and is never read here again).
    /// `AudioModRuntime` recomputes this only on a data-version change and
    /// skips every send outside the set (unless it's the scope-tapped send) —
    /// see `docs/AUDIO_SENDS_UX_DESIGN.md` D4/§3.2,
    /// `docs/AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §3.4.
    pub fn analysis_consumed_sends(&self) -> ahash::AHashSet<crate::AudioSendId> {
        // A plain fn (not a closure capturing `out`) so it can be called
        // alongside the direct `out.insert` the clip-trigger arm below needs —
        // a closure borrowing `out` mutably would keep it borrowed for the
        // whole loop and conflict with that direct access.
        fn collect(
            out: &mut ahash::AHashSet<crate::AudioSendId>,
            fx: &crate::effects::PresetInstance,
        ) {
            // §9 U4: a fire-mode mod is just an enabled `audio_mods` entry,
            // already covered below — no separate arm needed.
            if let Some(mods) = fx.audio_mods.as_ref() {
                for m in mods.iter().filter(|m| m.enabled) {
                    out.insert(m.source.send_id.clone());
                }
            }
        }
        let mut out = ahash::AHashSet::new();
        for fx in &self.settings.master_effects {
            collect(&mut out, fx);
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                for fx in effects {
                    collect(&mut out, fx);
                }
            }
            if let Some(gp) = layer.gen_params() {
                collect(&mut out, gp);
            }
            for ct in layer.clip_triggers.iter().filter(|c| c.enabled) {
                out.insert(ct.source.send_id.clone());
            }
        }
        out
    }

    /// Number of parameters whose modulation references `send_id` (enabled or
    /// not), across master effects, layer effects, and generator params. Used to
    /// warn before deleting a send that sliders still depend on.
    pub fn audio_send_usage_count(&self, send_id: &crate::id::AudioSendId) -> usize {
        fn inst_count(fx: &crate::effects::PresetInstance, send_id: &crate::id::AudioSendId) -> usize {
            // §9 U4: a fire-mode mod is a normal `audio_mods` entry — already
            // counted below, no separate arm needed.
            fx.audio_mods
                .as_ref()
                .map(|v| v.iter().filter(|a| &a.source.send_id == send_id).count())
                .unwrap_or(0)
        }
        let mut count = self
            .settings
            .master_effects
            .iter()
            .map(|fx| inst_count(fx, send_id))
            .sum::<usize>();
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                count += effects.iter().map(|fx| inst_count(fx, send_id)).sum::<usize>();
            }
            if let Some(gp) = layer.gen_params() {
                count += inst_count(gp, send_id);
            }
        }
        count
    }

    /// Every ENABLED audio mod reading `send_id`, resolved to a legible
    /// `(owning layer, "LayerName \u{2022} EffectName \u{2022} ParamName")` pair — the
    /// Audio Setup panel's Consumers section (`docs/AUDIO_SENDS_UX_DESIGN.md`
    /// D1/D3). `layer_id` is `None` for a master-effects mod (nothing to jump
    /// to; the label reads "Master" instead). Walks the same instance set
    /// [`Self::audio_send_usage_count`] does.
    pub fn audio_mod_consumers(&self, send_id: &crate::id::AudioSendId) -> Vec<(Option<crate::id::LayerId>, String)> {
        fn collect(
            layer_id: Option<crate::id::LayerId>,
            layer_name: &str,
            fx: &crate::effects::PresetInstance,
            send_id: &crate::id::AudioSendId,
            out: &mut Vec<(Option<crate::id::LayerId>, String)>,
        ) {
            // §9 U4: a fire-mode mod is a normal `audio_mods` entry — already
            // listed below by its own param name (no more bespoke "Trigger"
            // label; the param the gate card lives on names itself).
            if let Some(mods) = fx.audio_mods.as_ref() {
                for m in mods.iter().filter(|m| m.enabled && &m.source.send_id == send_id) {
                    let effect_name = crate::preset_type_registry::display_name(fx.effect_type());
                    let param_name = fx
                        .params
                        .get(&m.param_id)
                        .map(|p| p.spec.name.clone())
                        .unwrap_or_else(|| m.param_id.to_string());
                    out.push((
                        layer_id.clone(),
                        format!("{layer_name} \u{2022} {effect_name} \u{2022} {param_name}"),
                    ));
                }
            }
        }
        let mut out = Vec::new();
        for fx in &self.settings.master_effects {
            collect(None, "Master", fx, send_id, &mut out);
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                for fx in effects {
                    collect(Some(layer.layer_id.clone()), &layer.name, fx, send_id, &mut out);
                }
            }
            if let Some(gp) = layer.gen_params() {
                collect(Some(layer.layer_id.clone()), &layer.name, gp, send_id, &mut out);
            }
        }
        out
    }

    /// Consumers for the Audio Setup panel's Consumers section that are
    /// layer-owned `LayerClipTrigger` configs (P3, D2) rather than
    /// `PresetInstance` audio mods — the walk `audio_mod_consumers` above
    /// can't reach, since a clip trigger has no `param_id`/effect to name.
    /// Mirrors that method's shape: `(owning layer, display label)`, enabled
    /// configs sourcing `send_id` only. Label format is "Clip trigger •
    /// Layer • Band" (§7.2 item 7, P8, 2026-07-11 — matches the mod rows'
    /// "Layer • Effect • Param" bullet convention instead of the deleted
    /// Triggers matrix's arrow style, "Low → LayerName").
    pub fn clip_trigger_consumers(
        &self,
        send_id: &crate::id::AudioSendId,
    ) -> Vec<(Option<crate::id::LayerId>, String)> {
        let mut out = Vec::new();
        for layer in &self.timeline.layers {
            for cfg in &layer.clip_triggers {
                if !cfg.enabled || &cfg.source.send_id != send_id {
                    continue;
                }
                let feature = cfg.source.feature;
                let feature_label = match feature.kind {
                    crate::audio_mod::AudioFeatureKind::Transients => {
                        feature.band.label().to_string()
                    }
                    crate::audio_mod::AudioFeatureKind::Kick => "Kick".to_string(),
                    kind => format!("{} {}", kind.label(), feature.band.label()),
                };
                out.push((
                    Some(layer.layer_id.clone()),
                    format!("Clip trigger \u{2022} {} \u{2022} {feature_label}", layer.name),
                ));
            }
        }
        out
    }

    /// Run `f` against the [`crate::effects::PresetInstance`] that a
    /// [`crate::graph_target::GraphTarget`] resolves to, returning its
    /// result (`None` if the target doesn't resolve). The one entry point
    /// editing commands use to operate on an effect instance or a layer's
    /// generator without forking — both are a `PresetInstance` now that the
    /// generator's graph lives on `gen_params` (graph-home unification), so
    /// there is no `GraphHost`/`GeneratorHost` abstraction. A generator target
    /// initializes the layer's `gen_params` if absent (graph editing must work
    /// before param state exists), inheriting the layer's generator type.
    pub fn with_preset_graph_mut<R>(
        &mut self,
        target: &crate::graph_target::GraphTarget,
        f: impl FnOnce(&mut crate::effects::PresetInstance) -> R,
    ) -> Option<R> {
        match target {
            crate::graph_target::GraphTarget::Effect(eid) => {
                let fx = self.find_effect_by_id_mut(eid)?;
                Some(f(fx))
            }
            crate::graph_target::GraphTarget::Generator(lid) => {
                let (_, layer) = self.timeline.find_layer_by_id_mut(lid.as_str())?;
                Some(f(layer.gen_params_or_init()))
            }
        }
    }

    /// The `&mut PresetInstance` an Ableton mapping target addresses —
    /// located the way the Ableton bridge addresses hosts: by `effect_type`
    /// within master / a layer, or a layer's generator. `None` for
    /// `MacroSlot` (a macro slot is not a preset instance) or an unresolved
    /// host. This is the single master/layer/generator locate-fork: every
    /// per-target Ableton accessor (the mappings vec, live value writes)
    /// routes through here so the dispatch is written exactly once.
    pub fn find_preset_instance_mut(
        &mut self,
        target: &crate::ableton_mapping::AbletonMappingTarget,
    ) -> Option<&mut crate::effects::PresetInstance> {
        use crate::ableton_mapping::AbletonMappingTarget as T;
        match target {
            T::MasterEffect { effect_type, .. } => self
                .settings
                .master_effects
                .iter_mut()
                .find(|f| f.effect_type() == effect_type),
            T::LayerEffect {
                layer_id,
                effect_type,
                ..
            } => self
                .timeline
                .find_layer_by_id_mut(layer_id.as_str())
                .and_then(|(_, layer)| layer.effects.as_mut())
                .and_then(|effects| effects.iter_mut().find(|f| f.effect_type() == effect_type)),
            T::GenParam { layer_id, .. } => self
                .timeline
                .find_layer_by_id_mut(layer_id.as_str())
                .and_then(|(_, layer)| layer.gen_params_mut()),
            T::MacroSlot { .. } => None,
        }
    }

    /// Const twin of [`Self::find_preset_instance_mut`].
    pub fn find_preset_instance(
        &self,
        target: &crate::ableton_mapping::AbletonMappingTarget,
    ) -> Option<&crate::effects::PresetInstance> {
        use crate::ableton_mapping::AbletonMappingTarget as T;
        match target {
            T::MasterEffect { effect_type, .. } => self
                .settings
                .master_effects
                .iter()
                .find(|f| f.effect_type() == effect_type),
            T::LayerEffect {
                layer_id,
                effect_type,
                ..
            } => self
                .timeline
                .find_layer_by_id(layer_id.as_str())
                .and_then(|(_, layer)| layer.effects.as_ref())
                .and_then(|effects| effects.iter().find(|f| f.effect_type() == effect_type)),
            T::GenParam { layer_id, .. } => self
                .timeline
                .find_layer_by_id(layer_id.as_str())
                .and_then(|(_, layer)| layer.gen_params()),
            T::MacroSlot { .. } => None,
        }
    }

    /// The `&mut Option<Vec<AbletonParamMapping>>` an Ableton mapping
    /// target's per-param mappings live in. Thin projection of
    /// [`Self::find_preset_instance_mut`]; `None` for `MacroSlot` (single
    /// mapping, not a per-param vec — its call sites keep their own arm) or
    /// an unresolved host.
    pub fn ableton_param_mappings_mut(
        &mut self,
        target: &crate::ableton_mapping::AbletonMappingTarget,
    ) -> Option<&mut Option<Vec<crate::ableton_mapping::AbletonParamMapping>>> {
        self.find_preset_instance_mut(target)
            .map(|fx| &mut fx.ableton_mappings)
    }

    /// Const twin of [`Self::ableton_param_mappings_mut`].
    pub fn ableton_param_mappings(
        &self,
        target: &crate::ableton_mapping::AbletonMappingTarget,
    ) -> Option<&Option<Vec<crate::ableton_mapping::AbletonParamMapping>>> {
        self.find_preset_instance(target)
            .map(|fx| &fx.ableton_mappings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_support::*;
    use crate::PresetTypeId;
    use crate::effects::{ParamId, PresetInstance};

    fn amplitude_mod(send_id: crate::AudioSendId) -> crate::audio_mod::ParameterAudioMod {
        crate::audio_mod::ParameterAudioMod::new(
            ParamId::from("amount"),
            send_id,
            crate::audio_mod::AudioFeature::new(
                crate::audio_mod::AudioFeatureKind::Amplitude,
                crate::audio_mod::AudioBand::Full,
            ),
        )
    }

    #[test]
    fn analysis_consumed_sends_includes_only_the_send_with_an_enabled_mod() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        let send_b = send_with_id("B", "send-b");
        p.audio_setup.sends.push(send_a.clone());
        p.audio_setup.sends.push(send_b.clone());

        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.audio_mods_mut().push(amplitude_mod(send_a.id.clone()));
        p.settings.master_effects.push(fx);

        let consumed = p.analysis_consumed_sends();
        assert_eq!(consumed.len(), 1);
        assert!(consumed.contains(&send_a.id));
        assert!(!consumed.contains(&send_b.id));
    }

    #[test]
    fn analysis_consumed_sends_is_empty_when_the_only_mod_is_disabled() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        p.audio_setup.sends.push(send_a.clone());

        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        let m = fx.audio_mods_mut();
        m.push(amplitude_mod(send_a.id.clone()));
        m[0].enabled = false;
        p.settings.master_effects.push(fx);

        assert!(p.analysis_consumed_sends().is_empty());
    }

    #[test]
    fn analysis_consumed_sends_includes_send_with_enabled_layer_clip_trigger_and_no_mod() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        p.audio_setup.sends.push(send_a.clone());
        p.timeline.layers.push(layer_with_clip_trigger(
            send_a.id.clone(),
            crate::audio_mod::AudioBand::Low,
            true,
        ));

        let consumed = p.analysis_consumed_sends();
        assert_eq!(consumed.len(), 1);
        assert!(consumed.contains(&send_a.id));
    }

    #[test]
    fn analysis_consumed_sends_excludes_send_with_disabled_layer_clip_trigger() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        p.audio_setup.sends.push(send_a.clone());
        // `enabled: false` — layer_with_clip_trigger's third arg.
        p.timeline.layers.push(layer_with_clip_trigger(
            send_a.id.clone(),
            crate::audio_mod::AudioBand::Low,
            false,
        ));

        assert!(p.analysis_consumed_sends().is_empty());
    }

    #[test]
    fn analysis_consumed_sends_ignores_drained_legacy_send_triggers() {
        // §3.4: `send.triggers` is deserialize-only legacy storage now —
        // even if something hand-populates it (bypassing the load
        // migration), `analysis_consumed_sends` must never read it again.
        let mut p = Project::default();
        let mut send_a = send_with_id("A", "send-a");
        let mut route = crate::audio_trigger::TriggerRoute::new(crate::audio_mod::AudioBand::Low);
        route.enabled = true;
        send_a.triggers.push(route);
        p.audio_setup.sends.push(send_a);

        assert!(p.analysis_consumed_sends().is_empty());
    }

    #[test]
    fn has_active_clip_triggers_true_only_when_some_layer_has_an_enabled_config() {
        let mut p = Project::default();
        assert!(!p.has_active_clip_triggers());

        p.timeline.layers.push(layer_with_clip_trigger(
            crate::AudioSendId::new("send-a"),
            crate::audio_mod::AudioBand::Low,
            false,
        ));
        assert!(!p.has_active_clip_triggers(), "disabled config doesn't count");

        p.timeline.layers[0].clip_triggers[0].enabled = true;
        assert!(p.has_active_clip_triggers());
    }

    /// A `clip_trigger`-shaped bundled param, `is_trigger_gate` set — the
    /// only thing project.rs's own `slot`-less test module needs to build a
    /// trigger-gate card by hand (mirrors `effects::tests::gate_slot`, kept
    /// local since that helper is private to `effects.rs`'s own test
    /// module).
    fn gate_param(id: &str) -> crate::params::Param {
        let mut p = slot(id, 0.0, true);
        p.spec.name = "Clip Trigger".to_string();
        p.spec.is_toggle = true;
        p.spec.is_trigger_gate = true;
        p
    }

    fn armed_trigger_gate_mod(send_id: crate::AudioSendId) -> crate::audio_mod::ParameterAudioMod {
        let mut m = crate::audio_mod::ParameterAudioMod::new(
            "clip_trigger".into(),
            send_id,
            crate::audio_mod::AudioFeature::new(
                crate::audio_mod::AudioFeatureKind::Transients,
                crate::audio_mod::AudioBand::Full,
            ),
        );
        m.trigger_mode = Some(crate::audio_trigger::TriggerFireMode::Transient);
        m
    }

    #[test]
    fn armed_trigger_gate_mod_turns_the_analysis_gate_on_and_claims_its_send() {
        // Regression (2026-07-07, class-collapsed 2026-07-07 per §9 U1/U4): a
        // project whose ONLY audio consumer is an armed fire-mode mod on a
        // trigger-gate param never started capture (has_active_audio_mods
        // false) and, even with capture running, its send was skipped by the
        // D4 gate (analysis_consumed_sends empty) — so armed audio triggers
        // silently never fired. §9 deletes the second per-instance config
        // type that caused it; this test is the proof the plain `audio_mods`
        // walk covers a fire-mode mod with zero special-case code.
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        p.audio_setup.sends.push(send_a.clone());

        let mut layer = crate::layer::Layer::new("PLASMA".into(), crate::types::LayerType::Generator, 0);
        layer.layer_id = crate::LayerId::new("plasma-layer");
        let gp = layer.gen_params_or_init();
        gp.params.push(gate_param("clip_trigger"));
        gp.audio_mods_mut().push(armed_trigger_gate_mod(send_a.id.clone()));
        p.timeline.layers.push(layer);

        assert!(p.has_active_audio_mods(), "armed trigger-gate mod must start capture");
        let consumed = p.analysis_consumed_sends();
        assert!(consumed.contains(&send_a.id), "armed trigger's send must be analyzed");
        assert_eq!(p.audio_send_usage_count(&send_a.id), 1);
        let consumers = p.audio_mod_consumers(&send_a.id);
        assert_eq!(consumers.len(), 1);
        assert!(
            consumers[0].1.contains("Clip Trigger"),
            "consumers list names the param the gate card lives on, not a bespoke 'Trigger' label; got {}",
            consumers[0].1
        );
    }

    #[test]
    fn disarmed_trigger_gate_mod_does_not_gate_analysis_but_still_counts_as_send_usage() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        p.audio_setup.sends.push(send_a.clone());

        let mut layer = crate::layer::Layer::new("PLASMA".into(), crate::types::LayerType::Generator, 0);
        layer.layer_id = crate::LayerId::new("plasma-layer");
        let gp = layer.gen_params_or_init();
        gp.params.push(gate_param("clip_trigger"));
        let mut m = armed_trigger_gate_mod(send_a.id.clone());
        m.enabled = false;
        gp.audio_mods_mut().push(m);
        p.timeline.layers.push(layer);

        assert!(!p.has_active_audio_mods(), "disarmed mod must not run capture");
        assert!(p.analysis_consumed_sends().is_empty());
        // Usage matches the plain audio-mod semantics: the mod still
        // references the send whether enabled or not, so deleting it should
        // warn.
        assert_eq!(p.audio_send_usage_count(&send_a.id), 1);
        assert!(p.audio_mod_consumers(&send_a.id).is_empty(), "consumers lists armed only");
    }

    #[test]
    fn audio_mod_consumers_resolves_layer_effect_and_param_names() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        p.audio_setup.sends.push(send_a.clone());

        let mut layer = crate::layer::Layer::new("BLOOM LAYER".into(), crate::types::LayerType::Video, 0);
        layer.layer_id = crate::LayerId::new("bloom-layer");
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.audio_mods_mut().push(amplitude_mod(send_a.id.clone()));
        layer.effects = Some(vec![fx]);
        p.timeline.layers.push(layer);

        let consumers = p.audio_mod_consumers(&send_a.id);
        assert_eq!(consumers.len(), 1);
        assert_eq!(consumers[0].0, Some(crate::LayerId::new("bloom-layer")));
        assert!(
            consumers[0].1.starts_with("BLOOM LAYER \u{2022} Bloom \u{2022} "),
            "label should read 'LayerName \u{2022} EffectName \u{2022} ParamName', got {}",
            consumers[0].1
        );
    }

    #[test]
    fn audio_mod_consumers_excludes_disabled_mods_and_other_sends() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        let send_b = send_with_id("B", "send-b");
        p.audio_setup.sends.push(send_a.clone());
        p.audio_setup.sends.push(send_b.clone());

        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        let mods = fx.audio_mods_mut();
        mods.push(amplitude_mod(send_a.id.clone()));
        mods[0].enabled = false;
        mods.push(amplitude_mod(send_b.id.clone()));
        p.settings.master_effects.push(fx);

        assert!(p.audio_mod_consumers(&send_a.id).is_empty(), "disabled mod excluded");
        let b_consumers = p.audio_mod_consumers(&send_b.id);
        assert_eq!(b_consumers.len(), 1);
        assert_eq!(b_consumers[0].0, None, "master-effects mod has no owning layer");
        assert!(b_consumers[0].1.starts_with("Master \u{2022} "));
    }

    #[test]
    fn clip_trigger_consumers_resolves_layer_and_band_label() {
        use crate::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource};
        use crate::audio_trigger::LayerClipTrigger;

        let mut p = Project::default();
        let send_a = send_with_id("Kick", "send-a");
        p.audio_setup.sends.push(send_a.clone());

        let mut layer = crate::layer::Layer::new("STROBE".into(), crate::types::LayerType::Video, 0);
        layer.layer_id = crate::LayerId::new("strobe-layer");
        let mut cfg = LayerClipTrigger::new(AudioModSource {
            send_id: send_a.id.clone(),
            feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
        });
        cfg.enabled = true;
        layer.clip_triggers.push(cfg);
        p.timeline.layers.push(layer);

        let consumers = p.clip_trigger_consumers(&send_a.id);
        assert_eq!(consumers.len(), 1);
        assert_eq!(consumers[0].0, Some(crate::LayerId::new("strobe-layer")));
        assert_eq!(
            consumers[0].1,
            "Clip trigger \u{2022} STROBE \u{2022} Low",
            "Transients formats as the bare band label"
        );
    }

    #[test]
    fn clip_trigger_consumers_excludes_disabled_configs_and_other_sends() {
        use crate::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource};
        use crate::audio_trigger::LayerClipTrigger;

        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        let send_b = send_with_id("B", "send-b");
        p.audio_setup.sends.push(send_a.clone());
        p.audio_setup.sends.push(send_b.clone());

        let mut layer = crate::layer::Layer::new("L".into(), crate::types::LayerType::Video, 0);
        layer.layer_id = crate::LayerId::new("l1");
        // Disabled — excluded even though it sources send_a.
        let mut disabled = LayerClipTrigger::new(AudioModSource {
            send_id: send_a.id.clone(),
            feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        });
        disabled.enabled = false;
        layer.clip_triggers.push(disabled);
        // Enabled but sources send_b — excluded from send_a's consumers.
        let mut other_send = LayerClipTrigger::new(AudioModSource {
            send_id: send_b.id.clone(),
            feature: AudioFeature::new(AudioFeatureKind::Centroid, AudioBand::Full),
        });
        other_send.enabled = true;
        layer.clip_triggers.push(other_send);
        p.timeline.layers.push(layer);

        assert!(p.clip_trigger_consumers(&send_a.id).is_empty(), "disabled config excluded");
        let b_consumers = p.clip_trigger_consumers(&send_b.id);
        assert_eq!(b_consumers.len(), 1);
        assert_eq!(
            b_consumers[0].1,
            "Clip trigger \u{2022} L \u{2022} Centroid Full",
            "non-Transients spells out the detector"
        );
    }
}
