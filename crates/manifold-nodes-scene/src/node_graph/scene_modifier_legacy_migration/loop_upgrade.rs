//! Load-time upgrade for the fixed-row Scene Loop graph shape.
//!
//! This module deliberately knows only the old graph signature and the small
//! set of parameter renames needed to move it into the corridor shape.  It
//! does not construct a modifier, infer an attachment, or search for a
//! possible camera takeover.

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::effects::ParamConvert;

const LOOP_SIGNATURE: &[(&str, &str)] = &[
    ("loop_phase", "node.beat_ramp"),
    ("scene_array", "node.scene_array"),
    ("loop_camera", "node.loop_camera"),
    ("loop_cam_switch", "node.camera_switch"),
];

// Catalog tests live outside this module; production visibility stays local.

manifold_core::testkit_visible! {
/// Upgrade a complete, known fixed-row Scene Loop graph in place.
///
/// A graph with no Loop identity is ignored.  A graph with any Loop identity
/// but without the complete four-node signature is rejected before mutation;
/// this makes pre-switch saves lossless instead of guessing which camera they
/// used to own.  The old `scene_array.count` parameter is the shape gate, so a
/// current corridor graph is an idempotent no-op.
pub(super) fn upgrade_known_loop(def: &mut EffectGraphDef) -> Result<bool, String> {
    let mut docs = Vec::with_capacity(LOOP_SIGNATURE.len());
    let mut present = false;

    for &(node_id, type_id) in LOOP_SIGNATURE {
        let matches: Vec<&EffectGraphNode> = def
            .nodes
            .iter()
            .filter(|node| node.node_id.as_str() == node_id)
            .collect();
        if !matches.is_empty() {
            present = true;
        }
        if matches.len() > 1 {
            return Err(format!(
                "Scene Loop upgrade rejected: duplicate node identity {node_id}"
            ));
        }
        if let Some(node) = matches.first() {
            if node.type_id != type_id {
                return Err(format!(
                    "Scene Loop upgrade rejected: {node_id} has type {}, expected {type_id}",
                    node.type_id
                ));
            }
            docs.push((node_id, node.id));
        }
    }

    if !present {
        return Ok(false);
    }

    if docs.len() != LOOP_SIGNATURE.len() {
        let missing = LOOP_SIGNATURE
            .iter()
            .filter_map(|(node_id, _)| {
                (!docs.iter().any(|(found, _)| found == node_id)).then_some(*node_id)
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "Scene Loop upgrade rejected: incomplete fixed-row signature (missing {missing}); graph unchanged"
        ));
    }

    let array_doc = doc_id(&docs, "scene_array");
    let camera_doc = doc_id(&docs, "loop_camera");
    let array = def
        .nodes
        .iter()
        .find(|node| node.id == array_doc)
        .expect("validated scene_array identity");
    if !array.params.contains_key("count") {
        return Ok(false);
    }

    // These are authored numeric values in the legacy shape.  Do not silently
    // reinterpret a future/hand-written storage variant or a non-finite value
    // as the default: preserving the source is safer than a lossy upgrade.
    if !param_f32(def, array_doc, "count").is_ok_and(|value| value.is_some()) {
        return Ok(false);
    }

    // Read all source values before cloning and mutating the candidate.
    let jitter_period = match param_f32(def, array_doc, "jitter_period") {
        Ok(value) => value.unwrap_or(1.0),
        Err(_) => return Ok(false),
    }
    .round()
    .clamp(1.0, 8.0);
    let stride = match param_f32(def, camera_doc, "stride") {
        Ok(value) => value.unwrap_or(1.0),
        Err(_) => return Ok(false),
    }
    .round()
    .clamp(1.0, 8.0);
    let patterns_per_loop = (stride / jitter_period).round().clamp(1.0, 8.0);

    let mut candidate = def.clone();
    let array = node_mut(&mut candidate, array_doc);
    array.params.remove("count");
    array.params.remove("jitter_period");
    array.params.insert(
        "pattern_length".to_string(),
        SerializedParamValue::Float {
            value: jitter_period,
        },
    );
    array.exposed_params.remove("count");
    array.exposed_params.remove("jitter_period");
    array.exposed_params.insert("pattern_length".to_string());

    let camera = node_mut(&mut candidate, camera_doc);
    camera.params.remove("stride");
    camera.params.insert(
        "patterns_per_loop".to_string(),
        SerializedParamValue::Float {
            value: patterns_per_loop,
        },
    );
    camera.params.insert(
        "pattern_length".to_string(),
        SerializedParamValue::Float {
            value: jitter_period,
        },
    );
    camera.exposed_params.remove("stride");
    camera
        .exposed_params
        .insert("patterns_per_loop".to_string());

    if !candidate.wires.iter().any(|wire| {
        wire.from_node == camera_doc && wire.to_node == array_doc && wire.to_port == "camera"
    }) {
        candidate.wires.push(EffectGraphWire {
            from_node: camera_doc,
            from_port: "out".to_string(),
            to_node: array_doc,
            to_port: "camera".to_string(),
        });
    }

    repair_metadata(&mut candidate, jitter_period, patterns_per_loop)?;
    *def = candidate;
    Ok(true)
}
}

fn doc_id(docs: &[(&str, u32)], node_id: &str) -> u32 {
    docs.iter()
        .find_map(|(found, doc)| (*found == node_id).then_some(*doc))
        .expect("validated loop signature")
}

manifold_core::testkit_visible! {
fn node_mut(def: &mut EffectGraphDef, doc_id: u32) -> &mut EffectGraphNode {
    def.nodes
        .iter_mut()
        .find(|node| node.id == doc_id)
        .expect("validated loop document id")
}
}

fn param_f32(def: &EffectGraphDef, doc_id: u32, name: &str) -> Result<Option<f32>, String> {
    let Some(value) = def
        .nodes
        .iter()
        .find(|node| node.id == doc_id)
        .and_then(|node| node.params.get(name))
    else {
        return Ok(None);
    };
    let SerializedParamValue::Float { value } = value else {
        return Err(format!("unsupported legacy numeric storage for {name}"));
    };
    value
        .is_finite()
        .then_some(*value)
        .ok_or_else(|| format!("non-finite legacy numeric value for {name}"))
        .map(Some)
}

fn repair_metadata(def: &mut EffectGraphDef, pattern: f32, stride: f32) -> Result<(), String> {
    let Some(metadata) = def.preset_metadata.as_mut() else {
        return Ok(());
    };

    let mut pattern_ids = Vec::new();
    let mut stride_ids = Vec::new();
    let mut retired_ids = Vec::new();
    metadata.bindings.retain_mut(|binding| {
        let BindingTarget::Node { node_id, param } = &mut binding.target else {
            return true;
        };
        if node_id.as_str() == "scene_array" && param == "count" {
            *param = "pattern_length".to_string();
            pattern_ids.push(binding.id.clone());
            true
        } else if node_id.as_str() == "loop_camera" && param == "stride" {
            *param = "patterns_per_loop".to_string();
            stride_ids.push(binding.id.clone());
            true
        } else if node_id.as_str() == "scene_array" && param == "jitter_period" {
            retired_ids.push(binding.id.clone());
            false
        } else {
            true
        }
    });
    metadata
        .params
        .retain(|spec| !retired_ids.iter().any(|id| id == &spec.id));
    for spec in &mut metadata.params {
        if pattern_ids.iter().any(|id| id == &spec.id) {
            spec.name = "Pattern".to_string();
            spec.default_value = pattern;
            spec.min = spec.min.min(1.0);
            spec.max = spec.max.max(8.0);
            spec.whole_numbers = true;
        } else if stride_ids.iter().any(|id| id == &spec.id) {
            spec.name = "Stride".to_string();
            spec.default_value = stride;
            spec.min = spec.min.min(1.0);
            spec.max = spec.max.max(8.0);
            spec.whole_numbers = true;
        }
    }
    for binding in &mut metadata.bindings {
        if pattern_ids.iter().any(|id| id == &binding.id) {
            binding.convert = ParamConvert::IntRound;
            binding.default_value = pattern;
            binding.label = "Pattern".to_string();
        } else if stride_ids.iter().any(|id| id == &binding.id) {
            binding.convert = ParamConvert::IntRound;
            binding.default_value = stride;
            binding.label = "Stride".to_string();
        }
    }
    ensure_shared_binding(
        metadata,
        "scene_array",
        "pattern_length",
        "loop_camera",
        "pattern_length",
    )?;
    ensure_shared_binding(
        metadata,
        "loop_camera",
        "cell_size",
        "scene_array",
        "cell_size",
    )?;
    Ok(())
}

fn ensure_shared_binding(
    metadata: &mut manifold_core::effect_graph_def::PresetMetadata,
    source_node: &str,
    source_param: &str,
    target_node: &str,
    target_param: &str,
) -> Result<(), String> {
    let source_index = metadata.bindings.iter().position(|binding| {
        binding.target
            == BindingTarget::Node {
                node_id: manifold_core::NodeId::new(source_node),
                param: source_param.to_string(),
            }
    });
    let Some(source_index) = source_index else {
        return Ok(());
    };
    let source = metadata.bindings[source_index].clone();
    let target_indices: Vec<usize> = metadata
        .bindings
        .iter()
        .enumerate()
        .filter_map(|(index, binding)| {
            (binding.target
                == BindingTarget::Node {
                    node_id: manifold_core::NodeId::new(target_node),
                    param: target_param.to_string(),
                })
            .then_some(index)
        })
        .collect();
    if target_indices.len() > 1 {
        return Err(format!(
            "Scene Loop upgrade rejected: duplicate secondary binding {target_node}.{target_param}"
        ));
    }
    let desired_target = BindingTarget::Node {
        node_id: manifold_core::NodeId::new(target_node),
        param: target_param.to_string(),
    };
    let mut desired = source.clone();
    desired.target = desired_target;
    if let Some(target_index) = target_indices.first().copied() {
        let existing = &mut metadata.bindings[target_index];
        if existing.id != source.id
            || existing.convert != source.convert
            || existing.scale != source.scale
            || existing.offset != source.offset
            || existing.user_added != source.user_added
            || existing.default_mirrors_node_param != source.default_mirrors_node_param
        {
            return Err(format!(
                "Scene Loop upgrade rejected: conflicting secondary binding {target_node}.{target_param}"
            ));
        }
        *existing = desired;
    } else {
        metadata.bindings.push(desired);
    }
    Ok(())
}
