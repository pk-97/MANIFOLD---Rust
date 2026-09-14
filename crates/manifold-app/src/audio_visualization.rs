//! Project-side discovery for audio visualizer graph sources.

use ahash::AHashSet;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_core::effects::PresetInstance;
use manifold_core::id::AudioSendId;
use manifold_core::project::Project;

/// Return every send read by an enabled audio waveform or spectrum source.
/// Empty source strings intentionally resolve to the first configured send.
pub(crate) fn visualizer_consumed_sends(project: &Project) -> AHashSet<AudioSendId> {
    let mut out = AHashSet::new();
    let first = project
        .audio_setup
        .sends
        .first()
        .map(|send| send.id.clone());
    let mut visit_instance = |instance: &PresetInstance| {
        if !instance.enabled {
            return;
        }
        let def = instance
            .graph_def()
            .as_ref()
            .or_else(|| manifold_renderer::node_graph::bundled_preset_def(instance.effect_type()));
        if let Some(def) = def {
            visit_graph(def, &first, &mut out);
        }
    };

    for instance in &project.settings.master_effects {
        visit_instance(instance);
    }
    for layer in &project.timeline.layers {
        if let Some(effects) = layer.effects.as_ref() {
            for instance in effects {
                visit_instance(instance);
            }
        }
        for clip in &layer.clips {
            for instance in &clip.effects {
                visit_instance(instance);
            }
        }
        if let Some(instance) = layer.gen_params() {
            visit_instance(instance);
        }
    }
    out.retain(|id| project.audio_setup.find_send(id).is_some());
    out
}

fn visit_graph(def: &EffectGraphDef, first: &Option<AudioSendId>, out: &mut AHashSet<AudioSendId>) {
    for node in &def.nodes {
        visit_node(node, first, out);
    }
}

fn visit_node(
    node: &EffectGraphNode,
    first: &Option<AudioSendId>,
    out: &mut AHashSet<AudioSendId>,
) {
    if matches!(
        node.type_id.as_str(),
        "node.audio_waveform" | "node.audio_spectrum"
    ) {
        let value = node.params.get("send").and_then(|value| match value {
            SerializedParamValue::String { value } => Some(value.as_str()),
            _ => None,
        });
        match value {
            Some(value) if !value.is_empty() => {
                out.insert(AudioSendId::new(value));
            }
            _ => {
                if let Some(id) = first {
                    out.insert(id.clone());
                }
            }
        }
    }
    if let Some(group) = node.group.as_ref() {
        for nested in &group.nodes {
            visit_node(nested, first, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::{EffectGraphNode, SerializedParamValue};
    use std::collections::BTreeMap;

    #[test]
    fn empty_visual_source_uses_first_send() {
        let first = AudioSendId::new("first");
        let mut params = BTreeMap::new();
        params.insert(
            "send".into(),
            SerializedParamValue::String {
                value: String::new(),
            },
        );
        let node = EffectGraphNode {
            id: 1,
            node_id: Default::default(),
            type_id: "node.audio_waveform".into(),
            handle: None,
            params,
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        };
        let mut graph = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![node],
            wires: Vec::new(),
        };
        let mut out = AHashSet::new();
        visit_graph(&graph, &Some(first.clone()), &mut out);
        assert!(out.contains(&first));
        graph.nodes[0].type_id = "node.value".into();
        out.clear();
        visit_graph(&graph, &Some(first), &mut out);
        assert!(out.is_empty());
    }
}
