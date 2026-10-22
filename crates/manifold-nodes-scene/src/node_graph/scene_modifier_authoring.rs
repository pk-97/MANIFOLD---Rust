//! Shared preparation for a newly attached scene modifier.
//!
//! This is the authoring seam used by the importer, browser, and validator.
//! It creates a fresh local recipe snapshot, applies authored calibration
//! against the real owner's bounds, and captures static mesh frames once.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::id::NodeId;
use manifold_core::scene_modifier_preset::{
    SceneAxis, SceneModifierInstanceDef, SceneScalarExpr, SceneTargetSelection,
    initialize_scene_modifier_snapshot, validate_scene_modifier_schema,
};

use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::load::expand::{SceneModifierExpandError, resolve_modifier_mesh_frames, validate_modifier_attachment};

static SCENE_MODIFIER_REGISTRY: std::sync::LazyLock<PrimitiveRegistry> =
    std::sync::LazyLock::new(PrimitiveRegistry::with_builtin);

/// Prepare a fresh scene modifier instance for attachment to `owner`.
///
/// The recipe must carry bare `presetMetadata.sceneModifier` metadata and may
/// not be an already-attached stack. Initializers and calibrations are
/// evaluated against the owner's imported scene bounds. Recipes without
/// either remain byte-for-byte cloned and do not require bounds. Static mesh
/// frames are captured from the selected owner targets after preparation.
pub fn prepare_new_scene_modifier(
    owner: &EffectGraphDef,
    recipe: &EffectGraphDef,
    id: NodeId,
    scene: NodeId,
    targets: SceneTargetSelection,
) -> Result<SceneModifierInstanceDef, SceneModifierExpandError> {
    let graph = initialize_scene_modifier_graph(owner, recipe)?;

    let mut instance = SceneModifierInstanceDef {
        id,
        scene,
        targets,
        mesh_frames: Vec::new(),
        legacy_math_view_carrier: None,
        graph: Box::new(graph),
    };
    instance.mesh_frames = resolve_modifier_mesh_frames(owner, &instance)?;
    Ok(instance)
}

/// Validate a freshly prepared attachment through the same expansion contract
/// used when a graph is loaded.  Authoring surfaces use this at structural
/// sync time to explain unavailable recipes without maintaining a second set
/// of compatibility rules.
pub fn validate_new_scene_modifier(
    owner: &EffectGraphDef,
    instance: &SceneModifierInstanceDef,
) -> Result<(), SceneModifierExpandError> {
    validate_modifier_attachment(owner, instance, &SCENE_MODIFIER_REGISTRY)
}

/// `owner` with `instance` appended and one host card param per live recipe
/// param, bound to the instance by node id. Pure: it builds a graph value for
/// the compiler, the preset checker and tests; it never writes a project.
pub fn attach_scene_modifier(
    owner: &EffectGraphDef,
    instance: SceneModifierInstanceDef,
) -> Result<EffectGraphDef, String> {
    use manifold_core::effect_graph_def::{BindingDef, BindingTarget, PresetMetadata, StringBindingDef};
    use manifold_core::effects::ParamConvert;

    if owner.scene_modifiers.iter().any(|item| item.id == instance.id) {
        return Err(format!("modifier id '{}' is already attached", instance.id));
    }
    let local = instance
        .graph
        .preset_metadata
        .clone()
        .ok_or_else(|| format!("modifier '{}' has no recipe card", instance.id))?;
    let preparation: &[String] = local.scene_modifier.as_ref().map_or(&[], |recipe| &recipe.preparation_params);
    let mut graph = owner.clone();
    graph.version = graph.version.max(3);
    let id = instance.id.clone();
    graph.scene_modifiers.push(instance);
    let metadata = graph.preset_metadata.as_mut().ok_or("the host has no preset metadata")?;
    let host_id = |param: &str| {
        let tuple = serde_json::to_string(&(id.as_str(), param)).unwrap_or_default();
        format!("sceneModifier:{tuple}")
    };
    let in_use = |metadata: &PresetMetadata, key: &str| {
        metadata.params.iter().any(|param| param.id == key)
            || metadata.string_params.iter().any(|param| param.id == key)
            || metadata.bindings.iter().any(|binding| binding.id == key)
            || metadata.string_bindings.iter().any(|binding| binding.id == key)
    };
    for param in local.params.iter().filter(|param| !preparation.contains(&param.id)) {
        let key = host_id(&param.id);
        if in_use(metadata, &key) {
            return Err(format!("host param id '{key}' is already in use"));
        }
        let mut outer = param.clone();
        outer.id = key.clone();
        metadata.params.push(outer);
        metadata.bindings.push(BindingDef {
            id: key,
            label: param.name.clone(),
            default_value: param.default_value,
            target: BindingTarget::SceneModifier { modifier_id: id.clone(), param_id: param.id.clone() },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        });
    }
    for param in &local.string_params {
        let key = host_id(&param.id);
        if in_use(metadata, &key) {
            return Err(format!("host param id '{key}' is already in use"));
        }
        let mut outer = param.clone();
        outer.id = key.clone();
        metadata.string_params.push(outer);
        metadata.string_bindings.push(StringBindingDef {
            id: key,
            label: param.name.clone(),
            default_value: param.default_value.clone(),
            target: BindingTarget::SceneModifier { modifier_id: id.clone(), param_id: param.id.clone() },
        });
    }
    validate_scene_modifier_schema(&graph).map_err(|error| error.to_string())?;
    Ok(graph)
}

/// A stock force from the scene modifier catalog, tuned by [`tune_force_card`].
pub fn stock_force(id: &str, card: &[(&str, f32)]) -> Result<EffectGraphDef, String> {
    let catalog = manifold_node_engine::load::preset_loader::SCENE_MODIFIER_CATALOG.load();
    let json = catalog.json(id).ok_or_else(|| format!("The stock {id} force is missing"))?;
    let mut recipe: EffectGraphDef =
        serde_json::from_str(&json).map_err(|error| format!("The stock {id} force does not read: {error}"))?;
    tune_force_card(&mut recipe, card).map_err(|error| format!("The stock {id} force: {error}"))?;
    Ok(recipe)
}

/// Set a force's card defaults to `card` values, written through to the
/// params those rows drive so the graph agrees with its card. The one helper
/// for tuning a shipped force; works on a recipe or a prepared instance's
/// graph.
pub fn tune_force_card(force: &mut EffectGraphDef, card: &[(&str, f32)]) -> Result<(), String> {
    use manifold_core::effect_graph_def::{BindingTarget, SerializedParamValue};
    let metadata = force.preset_metadata.as_mut().ok_or("it has no card")?;
    let mut driven = Vec::new();
    for &(param, value) in card {
        let spec = metadata
            .params
            .iter_mut()
            .find(|spec| spec.id == param)
            .ok_or_else(|| format!("it has no {param}"))?;
        spec.default_value = value;
        for binding in metadata.bindings.iter_mut().filter(|binding| binding.id == param) {
            binding.default_value = value;
            if let BindingTarget::Node { node_id, param } = &binding.target {
                driven.push((node_id.clone(), param.clone(), value));
            }
        }
    }
    for (node_id, param, value) in driven {
        let slot = find_param(&mut force.nodes, &node_id, &param).ok_or_else(|| format!("it has no {node_id}.{param}"))?;
        *slot = match slot {
            SerializedParamValue::Enum { .. } => SerializedParamValue::Enum { value: value.round() as u32 },
            _ => SerializedParamValue::Float { value },
        };
    }
    Ok(())
}

/// The param `param` of the node with stable id `node_id`, at any depth.
fn find_param<'a>(
    nodes: &'a mut [manifold_core::effect_graph_def::EffectGraphNode],
    node_id: &NodeId,
    param: &str,
) -> Option<&'a mut manifold_core::effect_graph_def::SerializedParamValue> {
    for node in nodes {
        if node.node_id == *node_id {
            return node.params.get_mut(param);
        }
        if let Some(found) = node.group.as_deref_mut().and_then(|body| find_param(&mut body.nodes, node_id, param)) {
            return Some(found);
        }
    }
    None
}

/// Initialize a catalog recipe against its owner without mesh I/O. Used both
/// when attaching a modifier and when comparing or restoring its preset.
pub fn initialize_scene_modifier_graph(
    owner: &EffectGraphDef,
    recipe: &EffectGraphDef,
) -> Result<EffectGraphDef, SceneModifierExpandError> {
    validate_scene_modifier_schema(recipe)?;
    let recipe_metadata = recipe
        .preset_metadata
        .as_ref()
        .and_then(|metadata| metadata.scene_modifier.as_ref())
        .ok_or_else(|| SceneModifierExpandError::InvalidRecipe {
            path: "graph.presetMetadata.sceneModifier".into(),
            detail: "fresh scene-modifier attachment requires bare sceneModifier metadata".into(),
        })?;

    let graph = if recipe_metadata.initializers.is_empty()
        && recipe_metadata.calibrations.is_empty()
    {
        recipe.clone()
    } else {
        let mut axes = [false; 3];
        for initializer in &recipe_metadata.initializers {
            required_axes(&initializer.value, &mut axes);
        }
        for calibration in &recipe_metadata.calibrations {
            for expr in [
                &calibration.min,
                &calibration.max,
                &calibration.default_value,
            ] {
                required_axes(expr, &mut axes);
            }
        }
        let bounds = owner
            .preset_metadata
            .as_ref()
            .and_then(|metadata| metadata.scene_bounds)
            .map(|(min, max)| (min.map(f64::from), max.map(f64::from)))
            // Constant-only initializers have no geometric dependency.
            .or_else(|| (!axes.iter().any(|required| *required)).then_some(([0.0; 3], [0.0; 3])))
            .ok_or_else(|| SceneModifierExpandError::UnsupportedCoordinateFrame {
                path: "owner.presetMetadata.sceneBounds".into(),
                detail: "scene modifier calibration requires imported scene bounds".into(),
            })?;
        for (axis, required) in axes.iter().enumerate() {
            if *required
                && (!bounds.0[axis].is_finite()
                    || !bounds.1[axis].is_finite()
                    || bounds.0[axis] >= bounds.1[axis])
            {
                return Err(SceneModifierExpandError::UnsupportedCoordinateFrame {
                        path: format!("owner.presetMetadata.sceneBounds[{axis}]"),
                        detail: "bounds-dependent calibration requires positive finite extent on each referenced axis".into(),
                    });
            }
        }
        initialize_scene_modifier_snapshot(recipe, bounds)?
    };

    Ok(graph)
}

fn required_axes(expr: &SceneScalarExpr, axes: &mut [bool; 3]) {
    match expr {
        SceneScalarExpr::Constant { .. } => {}
        SceneScalarExpr::BoundsMin { axis } | SceneScalarExpr::BoundsMax { axis } => {
            axes[match axis {
                SceneAxis::X => 0,
                SceneAxis::Y => 1,
                SceneAxis::Z => 2,
            }] = true
        }
        SceneScalarExpr::Add { a, b }
        | SceneScalarExpr::Subtract { a, b }
        | SceneScalarExpr::Multiply { a, b }
        | SceneScalarExpr::Max { a, b } => {
            required_axes(a, axes);
            required_axes(b, axes);
        }
    }
}

/// Stable object choices for authoring and preview menus. Both use the same
/// scene reachability index as compilation, including nested object groups.
pub fn scene_modifier_objects(
    owner: &EffectGraphDef,
    scene: &NodeId,
) -> Result<Vec<NodeId>, SceneModifierExpandError> {
    manifold_node_engine::load::expand::scene_objects_for_authoring(owner, scene)
}
