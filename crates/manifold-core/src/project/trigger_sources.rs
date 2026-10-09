use crate::GraphTarget;
use crate::LayerId;
use crate::layer::Layer;
use crate::params::ClipTriggerSource;
use crate::types::LayerType;

/// Authored timing dependency for a source, including its available session
/// patterns. Launch cursors, meters, media settings and display metadata are
/// deliberately absent. Streams directly into SHA-256 without allocating.
pub fn clip_trigger_pattern_digest(layer: &Layer, session: &crate::session::SessionGrid) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    fn text(hash: &mut Sha256, value: &str) {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    fn clips(hash: &mut Sha256, clips: &[crate::clip::TimelineClip]) {
        hash.update((clips.len() as u64).to_le_bytes());
        for clip in clips {
            text(hash, clip.id.as_str());
            hash.update(clip.start_beat.0.to_le_bytes());
            hash.update(clip.duration_beats.0.to_le_bytes());
            hash.update([u8::from(clip.is_muted)]);
        }
    }

    let mut hash = Sha256::new();
    hash.update(b"manifold.clip-control-pattern\0v1");
    text(&mut hash, layer.layer_id.as_str());
    // Parent/group mute is not a control gate. Trigger mute is.
    hash.update([u8::from(layer.is_trigger() && layer.is_muted)]);
    clips(&mut hash, &layer.clips);
    for slot in session.slots.iter().filter(|slot| slot.layer_id == layer.layer_id) {
        hash.update(b"slot");
        text(&mut hash, slot.scene_id.as_str());
        hash.update(slot.sequence.length_beats.0.to_le_bytes());
        clips(&mut hash, &slot.sequence.clips);
    }
    hash.finalize().into()
}

#[derive(Clone, Copy)]
pub struct ClipTriggerLayer<'a> {
    pub layer_type: LayerType,
    pub parent: Option<&'a LayerId>,
}

impl<'a> From<&'a Layer> for ClipTriggerLayer<'a> {
    fn from(layer: &'a Layer) -> Self {
        Self {
            layer_type: layer.layer_type,
            parent: layer.parent_layer_id.as_ref(),
        }
    }
}

/// Return whether `source` is a trigger lane owned by `owner` or one of the
/// consecutive group ancestors above it.
pub fn clip_trigger_source_is_eligible<'a>(
    owner: &LayerId,
    source: &LayerId,
    layer_count: usize,
    lookup: impl Fn(&LayerId) -> Option<ClipTriggerLayer<'a>>,
) -> bool {
    let Some(owner_layer) = lookup(owner) else {
        return false;
    };
    if owner_layer.layer_type == LayerType::Trigger {
        return false;
    }
    let Some(source_layer) = lookup(source) else {
        return false;
    };
    if source_layer.layer_type != LayerType::Trigger {
        return false;
    }
    let Some(source_parent) = source_layer.parent else {
        return false;
    };
    if source_parent == owner {
        return true;
    }

    let mut current = owner;
    for _ in 0..layer_count {
        let Some(current_layer) = lookup(current) else {
            return false;
        };
        let Some(parent) = current_layer.parent else {
            return false;
        };
        let Some(parent_layer) = lookup(parent) else {
            return false;
        };
        if parent_layer.layer_type != LayerType::Group {
            return false;
        }
        if parent == source_parent {
            return true;
        }
        current = parent;
    }
    false
}

impl crate::project::Project {
    /// Resolve the non-master, non-trigger layer that owns a graph target.
    pub fn clip_trigger_target_layer(&self, target: &GraphTarget) -> Option<&Layer> {
        let host = target.host_target()?;
        match host {
            GraphTarget::Effect(effect_id) => self.timeline.layers.iter().find(|layer| {
                !layer.is_trigger()
                    && layer
                        .effects
                        .as_deref()
                        .is_some_and(|effects| effects.iter().any(|effect| &effect.id == effect_id))
            }),
            GraphTarget::Generator(layer_id) => self.timeline.layers.iter().find(|layer| {
                &layer.layer_id == layer_id
                    && !layer.is_trigger()
                    && layer.hosts_generator()
                    && layer.gen_params().is_some()
            }),
            GraphTarget::SceneModifier { .. } => None,
        }
    }

    /// Return trigger lanes directly owned by the target layer or one of its
    /// group ancestors, in timeline order.
    pub fn clip_trigger_source_options(&self, target: &GraphTarget) -> Vec<LayerId> {
        let Some(target_layer) = self.clip_trigger_target_layer(target) else {
            return Vec::new();
        };
        let owner = &target_layer.layer_id;
        let layer_count = self.timeline.layers.len();
        self.timeline
            .layers
            .iter()
            .filter(|candidate| {
                let source = &candidate.layer_id;
                clip_trigger_source_is_eligible(owner, source, layer_count, |id| {
                    self.timeline
                        .layer_index_for_id(id)
                        .and_then(|index| self.timeline.layers.get(index))
                        .map(ClipTriggerLayer::from)
                })
            })
            .map(|candidate| candidate.layer_id.clone())
            .collect()
    }

    /// Whether a parameter can use the requested clip-trigger source.
    pub fn can_assign_clip_trigger_source(
        &self,
        target: &GraphTarget,
        param_id: &str,
        source: &ClipTriggerSource,
    ) -> bool {
        let Some(owner_layer) = self.clip_trigger_target_layer(target) else {
            return false;
        };
        let Some(owner) = self.graph_target_owner(target) else {
            return false;
        };
        let Some(param) = owner.params.get(param_id) else {
            return false;
        };
        if param.spec.is_trigger_gate {
            return false;
        }
        match source {
            ClipTriggerSource::OwnLayer | ClipTriggerSource::Disabled => true,
            ClipTriggerSource::Lane { layer_id } => {
                let layer_count = self.timeline.layers.len();
                clip_trigger_source_is_eligible(
                    &owner_layer.layer_id,
                    layer_id,
                    layer_count,
                    |id| {
                        self.timeline
                            .layer_index_for_id(id)
                            .and_then(|index| self.timeline.layers.get(index))
                            .map(ClipTriggerLayer::from)
                    },
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::PresetInstance;
    use crate::effect_graph_def::ParamSpecDef;
    use crate::id::EffectId;
    use crate::layer::Layer;
    use crate::{Beats, PresetTypeId};

    fn param(id: &str) -> crate::params::Param {
        let spec = ParamSpecDef {
            id: id.to_owned(),
            name: id.to_owned(),
            ..Default::default()
        };
        crate::params::Param::bundled(spec)
    }

    fn effect(id: &str) -> PresetInstance {
        let mut instance = PresetInstance::new(PresetTypeId::BLOOM);
        instance.id = EffectId::new(id);
        instance.params.push(param("clipFire"));
        instance
    }

    fn effect_layer(id: &str, kind: LayerType, parent: Option<&str>) -> Layer {
        let mut layer = Layer::new(id.to_owned(), kind, 0);
        layer.layer_id = LayerId::new(id);
        layer.parent_layer_id = parent.map(LayerId::new);
        layer.effects = Some(vec![effect(&format!("{id}-effect"))]);
        layer
    }

    fn generator_layer(id: &str, kind: LayerType, parent: Option<&str>) -> Layer {
        let mut layer = Layer::new_generator(id.to_owned(), PresetTypeId::PLASMA, 0);
        layer.layer_id = LayerId::new(id);
        layer.layer_type = kind;
        layer.parent_layer_id = parent.map(LayerId::new);
        layer.gen_params_mut().unwrap().params.push(param("clipFire"));
        layer
    }

    fn trigger(id: &str, parent: &str) -> Layer {
        let mut layer = Layer::new_trigger(id.to_owned(), LayerId::new(parent), 0);
        layer.layer_id = LayerId::new(id);
        layer
    }

    #[test]
    fn eligibility_is_bounded_and_accepts_only_owner_or_group_ancestors() {
        let layers = [
            effect_layer("group", LayerType::Group, None),
            effect_layer("owner", LayerType::Video, Some("group")),
            trigger("owner-lane", "owner"),
            trigger("group-lane", "group"),
            effect_layer("sibling", LayerType::Video, Some("group")),
            trigger("sibling-lane", "sibling"),
            trigger("root-lane", "root"),
        ];
        let owner = LayerId::new("owner");
        let lookup = |id: &LayerId| {
            layers
                .iter()
                .find(|layer| &layer.layer_id == id)
                .map(ClipTriggerLayer::from)
        };
        assert!(clip_trigger_source_is_eligible(
            &owner,
            &LayerId::new("owner-lane"),
            layers.len(),
            lookup,
        ));
        assert!(clip_trigger_source_is_eligible(
            &owner,
            &LayerId::new("group-lane"),
            layers.len(),
            lookup,
        ));
        assert!(!clip_trigger_source_is_eligible(
            &owner,
            &LayerId::new("sibling-lane"),
            layers.len(),
            lookup,
        ));
        assert!(!clip_trigger_source_is_eligible(
            &owner,
            &LayerId::new("root-lane"),
            layers.len(),
            lookup,
        ));
        assert!(!clip_trigger_source_is_eligible(
            &owner,
            &LayerId::new("missing"),
            layers.len(),
            lookup,
        ));

        let invalid_layers = [
            effect_layer("group", LayerType::Group, None),
            effect_layer("owner", LayerType::Video, Some("sibling")),
            trigger("group-lane", "group"),
        ];
        let invalid_lookup = |id: &LayerId| {
            invalid_layers
                .iter()
                .find(|layer| &layer.layer_id == id)
                .map(ClipTriggerLayer::from)
        };
        assert!(!clip_trigger_source_is_eligible(
            &LayerId::new("owner"),
            &LayerId::new("group-lane"),
            invalid_layers.len(),
            invalid_lookup,
        ));

        let cycle_layers = [
            effect_layer("group-a", LayerType::Group, Some("group-b")),
            effect_layer("group-b", LayerType::Group, Some("group-a")),
            trigger("group-b-lane", "group-b"),
        ];
        let cycle_lookup = |id: &LayerId| {
            cycle_layers
                .iter()
                .find(|layer| &layer.layer_id == id)
                .map(ClipTriggerLayer::from)
        };
        assert!(clip_trigger_source_is_eligible(
            &LayerId::new("group-a"),
            &LayerId::new("group-b-lane"),
            cycle_layers.len(),
            cycle_lookup,
        ));
        assert!(!clip_trigger_source_is_eligible(
            &LayerId::new("group-a"),
            &LayerId::new("group-b-lane"),
            0,
            cycle_lookup,
        ));
    }

    #[test]
    fn options_are_timeline_ordered_and_limited_to_owner_and_group_ancestors() {
        let mut project = crate::project::Project::default();
        project.timeline.layers = vec![
            effect_layer("group", LayerType::Group, None),
            trigger("group-lane", "group"),
            trigger("unrelated", "missing"),
            effect_layer("owner", LayerType::Video, Some("group")),
            trigger("owner-lane", "owner"),
            effect_layer("sibling", LayerType::Video, Some("group")),
            trigger("sibling-lane", "sibling"),
        ];
        let target = GraphTarget::Effect(EffectId::new("owner-effect"));
        assert_eq!(
            project.clip_trigger_source_options(&target),
            vec![LayerId::new("group-lane"), LayerId::new("owner-lane")]
        );
        assert!(project.can_assign_clip_trigger_source(
            &target,
            "clipFire",
            &ClipTriggerSource::Lane { layer_id: LayerId::new("owner-lane") }
        ));
        assert!(project.can_assign_clip_trigger_source(
            &target,
            "clipFire",
            &ClipTriggerSource::Lane { layer_id: LayerId::new("group-lane") }
        ));
        assert!(!project.can_assign_clip_trigger_source(
            &target,
            "clipFire",
            &ClipTriggerSource::Lane { layer_id: LayerId::new("sibling-lane") }
        ));
        assert!(project.can_assign_clip_trigger_source(&target, "clipFire", &ClipTriggerSource::OwnLayer));
        assert!(project.can_assign_clip_trigger_source(&target, "clipFire", &ClipTriggerSource::Disabled));
        assert!(!project.can_assign_clip_trigger_source(&target, "missing", &ClipTriggerSource::OwnLayer));
    }

    #[test]
    fn target_resolution_excludes_master_clip_and_trigger_owners_and_accepts_generators() {
        let mut project = crate::project::Project::default();
        let mut owner = generator_layer("generator", LayerType::Generator, None);
        let generator_id = owner.layer_id.clone();
        let clip_effect = effect("clip-effect");
        let mut clip = crate::clip::TimelineClip::new_video("media".into(), Beats::ZERO, Beats(1.0), crate::Seconds::ZERO);
        clip.effects.push(clip_effect);
        owner.clips.push(clip);
        let trigger_owner = effect_layer("trigger-owner", LayerType::Trigger, None);
        let trigger_effect_id = trigger_owner.effects.as_ref().unwrap()[0].id.clone();
        project.timeline.layers = vec![owner, trigger_owner];
        project.settings.master_effects.push(effect("master"));

        let generator_target = GraphTarget::Generator(generator_id);
        assert!(project.clip_trigger_target_layer(&generator_target).is_some());
        assert!(project.can_assign_clip_trigger_source(
            &generator_target,
            "clipFire",
            &ClipTriggerSource::OwnLayer
        ));
        assert!(project.clip_trigger_target_layer(&GraphTarget::Effect(EffectId::new("clip-effect"))).is_none());
        assert!(project.clip_trigger_target_layer(&GraphTarget::Effect(trigger_effect_id)).is_none());
        assert!(project.clip_trigger_target_layer(&GraphTarget::Effect(EffectId::new("master"))).is_none());
    }

    #[test]
    fn clip_trigger_assignment_rejects_trigger_gate_parameters() {
        let mut project = crate::project::Project::default();
        let mut gate = generator_layer("gate", LayerType::Generator, None);
        let gate_id = gate.layer_id.clone();
        gate.gen_params_mut().unwrap().params.get_mut("clipFire").unwrap().spec.is_trigger_gate = true;
        project.timeline.layers.push(gate);
        assert!(!project.can_assign_clip_trigger_source(
            &GraphTarget::Generator(gate_id),
            "clipFire",
            &ClipTriggerSource::OwnLayer,
        ));
    }

    #[test]
    fn malformed_group_cycles_terminate_after_each_local_ancestor() {
        let mut project = crate::project::Project::default();
        let group_a = effect_layer("group-a", LayerType::Group, Some("group-b"));
        let group_b = effect_layer("group-b", LayerType::Group, Some("group-a"));
        project.timeline.layers = vec![
            group_a,
            trigger("group-a-lane", "group-a"),
            group_b,
            trigger("group-b-lane", "group-b"),
        ];
        let target = GraphTarget::Effect(EffectId::new("group-a-effect"));
        assert_eq!(
            project.clip_trigger_source_options(&target),
            vec![LayerId::new("group-a-lane"), LayerId::new("group-b-lane")]
        );
    }

    #[test]
    fn all_supported_owner_kinds_accept_local_lanes_and_reject_unrelated_sources() {
        let mut project = crate::project::Project::default();
        let kinds = [
            ("video", LayerType::Video),
            ("generator", LayerType::Generator),
            ("group", LayerType::Group),
            ("audio", LayerType::Audio),
            ("dmx", LayerType::Dmx),
        ];
        for (index, (id, kind)) in kinds.iter().copied().enumerate() {
            let mut owner = if kind == LayerType::Generator {
                let mut layer = generator_layer(id, kind, None);
                layer.effects = Some(vec![effect(&format!("{id}-effect"))]);
                layer
            } else {
                effect_layer(id, kind, None)
            };
            owner.index = (index * 2) as i32;
            project.timeline.layers.push(owner);
            project.timeline.layers.push(trigger(&format!("{id}-lane"), id));
        }
        let unrelated = LayerId::new("missing-parent-lane");
        let mut missing_parent = Layer::new_trigger("missing-parent".into(), LayerId::new("ghost"), 99);
        missing_parent.layer_id = unrelated.clone();
        project.timeline.layers.push(missing_parent);

        for (id, _) in kinds {
            let target = GraphTarget::Effect(EffectId::new(format!("{id}-effect")));
            let local = LayerId::new(format!("{id}-lane"));
            assert_eq!(project.clip_trigger_source_options(&target), vec![local.clone()]);
            assert!(project.can_assign_clip_trigger_source(
                &target,
                "clipFire",
                &ClipTriggerSource::Lane { layer_id: local }
            ));
            assert!(!project.can_assign_clip_trigger_source(
                &target,
                "clipFire",
                &ClipTriggerSource::Lane { layer_id: unrelated.clone() }
            ));
            let other = if id == "video" { "generator-lane" } else { "video-lane" };
            assert!(!project.can_assign_clip_trigger_source(
                &target, "clipFire", &ClipTriggerSource::Lane { layer_id: LayerId::new(other) }
            ));
        }
        let target = GraphTarget::SceneModifier {
            owner: Box::new(GraphTarget::Generator(LayerId::new("generator"))),
            modifier_id: crate::NodeId::new("modifier"),
        };
        assert!(project.can_assign_clip_trigger_source(
            &target,
            "clipFire",
            &ClipTriggerSource::Disabled
        ));
        assert!(!project.can_assign_clip_trigger_source(
            &target,
            "missing",
            &ClipTriggerSource::OwnLayer
        ));
    }
}
