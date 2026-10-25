//! The watched graph's snapshot, which the graph editor's canvas draws.
//!
//! The content thread publishes it in every `ContentState` push; the ui-snap
//! script runner, which absorbs the content commands a flow sends, publishes
//! it into its own `ContentState` through the same function. The snapshot is
//! a pure function of the project and the preset registry, so neither caller
//! needs the compositor or the GPU.

use std::sync::Arc;

use manifold_core::GraphTarget;
use manifold_core::project::Project;
use manifold_node_engine::snapshot::GraphSnapshot;

use crate::content_command::ContentCommand;

/// Cache entry for the watched editor-canvas snapshot (effect OR generator).
/// Holds the snapshot inside an `Arc` so the per-frame
/// `ContentState::active_graph_snapshot` field can clone the refcount
/// (no deep copy). At most one entry — only one canvas is visible at a time.
///
/// Invalidated when any of the snapshot's inputs change:
/// - `target` — the watched instance changed (canvas swap).
/// - `preset_type` — the instance's preset type changed in place
///   (effect/generator type swap via the picker while watched).
/// - `version` — the instance's `graph_version` bumped (a graph-editor edit
///   landed on it).
/// - `fingerprint` — the EmbeddedPresets stamp: the catalog overlay was reinstalled
///   (a fork or recalibrate), which can change the registry-sourced
///   routings / default graph even when `version` didn't move.
pub struct CachedGraphSnapshot {
    pub target: Arc<GraphTarget>,
    pub preset_type: manifold_core::PresetTypeId,
    pub version: u32,
    pub fingerprint: u64,
    pub snapshot: Arc<GraphSnapshot>,
}

/// The watched target a watch command sets, `None` when `cmd` is not one.
/// A target with no host never resolves, so it watches nothing.
pub(crate) fn watch_command_target(cmd: &ContentCommand) -> Option<Option<GraphTarget>> {
    match cmd {
        ContentCommand::WatchEffectGraph(effect_id) => {
            Some(effect_id.clone().map(GraphTarget::Effect))
        }
        ContentCommand::WatchGeneratorGraph(layer_id) => {
            Some(layer_id.clone().map(GraphTarget::Generator))
        }
        ContentCommand::WatchGraphTarget(target) => Some(
            target
                .clone()
                .filter(|target| target.host_target().is_some()),
        ),
        _ => None,
    }
}

/// What one state push publishes for the canvas: the watched target and its
/// snapshot, both `None` when nothing is watched or it no longer resolves.
/// `fingerprint` is the EmbeddedPresets change stamp.
pub(crate) fn active_graph(
    cache: &mut Option<CachedGraphSnapshot>,
    project: Option<&Project>,
    target: Option<&GraphTarget>,
    fingerprint: u64,
) -> (Option<Arc<GraphTarget>>, Option<Arc<GraphSnapshot>>) {
    let snapshot = project
        .zip(target)
        .and_then(|(project, target)| cached(cache, project, target, fingerprint));
    let target = snapshot
        .as_ref()
        .and_then(|_| cache.as_ref().map(|cache| Arc::clone(&cache.target)));
    (target, snapshot)
}

/// Build (or return cached) the editor canvas's graph snapshot for `target` —
/// effect or generator (fork #10).
///
/// Two paths per kind, mirrored:
/// - The instance has `graph: Some(def)` → build directly from the
///   per-instance override (renders what the user saved). Effects fill
///   `outer_routings` from the preset view (the outer→inner map is static
///   per type); generators project routings from the override's own
///   (bundle-grafted) `preset_metadata`, which also captures user-added
///   bindings.
/// - The instance has `graph: None` → the type-keyed `LoadedPresetView` via
///   `snapshot_for_view`.
///
/// Cached behind `cache`, keyed by (target, preset_type, graph_version,
/// catalog fingerprint) so the rebuild runs once per edit, not once per
/// content tick. Returns `None` if the target no longer resolves, the
/// generator has no type, or the type has no JSON preset.
fn cached(
    cache: &mut Option<CachedGraphSnapshot>,
    project: &Project,
    target: &GraphTarget,
    fingerprint: u64,
) -> Option<Arc<GraphSnapshot>> {
    let (preset_type, version) = match target {
        GraphTarget::Effect(eid) => {
            let inst = project.find_effect_by_id(eid)?;
            (inst.effect_type().clone(), inst.graph_version)
        }
        GraphTarget::Generator(lid) => {
            let (_, layer) = project.timeline.find_layer_by_id(lid)?;
            let gp = layer.gen_params()?;
            if gp.generator_type().is_none() {
                return None;
            }
            (gp.generator_type().clone(), gp.graph_version)
        }
        GraphTarget::SceneModifier { .. } => {
            let owner = project.graph_target_owner(target)?;
            (project.instance_preset_id(target)?, owner.graph_version)
        }
    };

    if let Some(hit) = cache.as_ref()
        && hit.target.as_ref() == target
        && hit.preset_type == preset_type
        && hit.version == version
        && hit.fingerprint == fingerprint
    {
        return Some(Arc::clone(&hit.snapshot));
    }

    let snapshot = Arc::new(build(project, target)?);
    *cache = Some(CachedGraphSnapshot {
        target: Arc::new(target.clone()),
        preset_type,
        version,
        fingerprint,
        snapshot: Arc::clone(&snapshot),
    });
    Some(snapshot)
}

fn build(project: &Project, target: &GraphTarget) -> Option<GraphSnapshot> {
    use manifold_node_engine::load::loaded_preset_view::{
        loaded_preset_view_by_id, outer_routings_from_view, snapshot_for_view,
    };
    Some(match target {
        GraphTarget::Effect(eid) => {
            let instance = project.find_effect_by_id(eid)?;
            if let Some(def) = instance.graph.as_ref() {
                // Per-card override: `from_def` has no live effect, so its
                // outer_routings come out empty. The type's routings are
                // authoritative; fill them in here.
                let mut snap = GraphSnapshot::from_def(def)?;
                snap.outer_routings = loaded_preset_view_by_id(instance.effect_type())
                    .map(|view| outer_routings_from_view(&view))
                    .unwrap_or_default();
                apply_effective_bound_values(&mut snap, def, instance);
                snap
            } else {
                snapshot_for_view(&*loaded_preset_view_by_id(instance.effect_type())?)?
            }
        }
        GraphTarget::Generator(lid) => {
            let (_, layer) = project.timeline.find_layer_by_id(lid)?;
            let gen_type = layer.generator_type();
            if let Some(override_def) = layer.generator_graph() {
                // Per-instance override: exposure state lives on the
                // layer's graph, so build from the override (grafting
                // bundled metadata back if edits dropped it) and project
                // routings from the override def's own metadata — captures
                // user-added bindings the type view wouldn't.
                let mut d = override_def.clone();
                manifold_nodes::registry::graft_preset_metadata_from_bundle(&mut d, gen_type);
                let mut snap = GraphSnapshot::from_def(&d)?;
                if let Some(meta) = d.preset_metadata.as_ref() {
                    use manifold_core::effect_graph_def::BindingTarget;
                    use manifold_node_engine::snapshot::{OuterParamRouting, OuterParamSource};
                    // Recurse into group bodies (BUG-103): a binding whose
                    // target lives inside a group — the glTF importer's
                    // per-object knobs on `mat_k` nodes inside each object's
                    // box — is dropped by a top-level-only handle map, so a
                    // diverged imported scene loses its group-face rows the
                    // same way the pristine path did. Shared helper so both
                    // arms resolve handles identically.
                    let mut handle_by_id: std::collections::HashMap<&str, &str> =
                        std::collections::HashMap::new();
                    manifold_node_engine::load::loaded_preset_view::collect_node_handles(
                        &d.nodes,
                        &mut handle_by_id,
                    );
                    let mut declared = manifold_core::param_lookup::Declared::new(&d);
                    snap.outer_routings = meta
                        .bindings
                        .iter()
                        .filter_map(|b| match &b.target {
                            BindingTarget::Node { node_id, param } => {
                                let handle = handle_by_id.get(node_id.as_str())?;
                                Some(OuterParamRouting {
                                    outer_label: layer
                                        .gen_params()
                                        .and_then(|gp| gp.params.get(&b.id))
                                        .map(|p| p.spec.name.clone())
                                        .or_else(|| declared.as_mut().map(|x| x.name(&b.id)))
                                        .unwrap_or_default(),
                                    outer_param_id: b.id.clone(),
                                    node_handle: handle.to_string(),
                                    inner_param: param.clone(),
                                    source: OuterParamSource::Static,
                                })
                            }
                            BindingTarget::Composite { .. }
                            | BindingTarget::SceneModifier { .. } => None,
                        })
                        .collect();
                }
                if let Some(gp) = layer.gen_params() {
                    apply_effective_bound_values(&mut snap, &d, gp);
                }
                snap
            } else {
                snapshot_for_view(&*loaded_preset_view_by_id(gen_type)?)?
            }
        }
        GraphTarget::SceneModifier { .. } => {
            let local = crate::graph_target::resolve(project, target)?;
            let owner = project.graph_target_owner(target)?;
            let mut snap = GraphSnapshot::from_def(&local)?;
            let mut projection = (*local).clone();
            let metadata = projection.preset_metadata.as_mut()?;
            metadata.bindings = crate::graph_target::modifier_bindings(project, target)?;
            metadata.params = owner
                .params
                .iter()
                .map(|param| param.spec.clone())
                .collect();
            let mut handles = std::collections::HashMap::new();
            manifold_node_engine::load::loaded_preset_view::collect_node_handles(
                &local.nodes,
                &mut handles,
            );
            snap.outer_routings = metadata
                .bindings
                .iter()
                .filter_map(|binding| {
                    let manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } =
                        &binding.target
                    else {
                        return None;
                    };
                    Some(manifold_node_engine::snapshot::OuterParamRouting {
                        outer_label: owner
                            .params
                            .get(&binding.id)
                            .map(|p| p.spec.name.clone())
                            .unwrap_or_default(),
                        outer_param_id: binding.id.clone(),
                        node_handle: handles.get(node_id.as_str())?.to_string(),
                        inner_param: param.clone(),
                        source: if binding.user_added {
                            manifold_node_engine::snapshot::OuterParamSource::User
                        } else {
                            manifold_node_engine::snapshot::OuterParamSource::Static
                        },
                    })
                })
                .collect();
            apply_effective_bound_values(&mut snap, &projection, owner);
            snap
        }
    })
}

/// Overlay the EFFECTIVE
/// (forward-reshaped) value onto every card-bound node-face row, instead of
/// the raw `def.nodes[..].params` value `GraphSnapshot::from_def` reads —
/// which may hold a stale node-face write `apply_bindings` stomps on the
/// next rebuild (BUG-158's snap-back). Walks `def.preset_metadata.bindings`;
/// for each `Node` target, reads the outer card's live value off
/// `instance.params` and pushes it through `apply_card_reshape` before
/// overwriting the matching row's `current_value`. A no-op for every unbound
/// param (no metadata, or no binding targets it) and for a binding whose
/// outer id isn't in the manifest (an authoring-time gap, not a user state).
fn apply_effective_bound_values(
    snap: &mut GraphSnapshot,
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    instance: &manifold_core::effects::PresetInstance,
) {
    use manifold_core::effect_graph_def::BindingTarget;
    let Some(meta) = def.preset_metadata.as_ref() else {
        return;
    };
    for binding in &meta.bindings {
        let BindingTarget::Node { node_id, param } = &binding.target else {
            continue;
        };
        if node_id.is_empty() {
            continue;
        }
        let Some(spec) = meta.params.iter().find(|p| p.id == binding.id) else {
            continue;
        };
        let Some(card_value) = instance.params.get(&binding.id).map(|p| p.value) else {
            continue;
        };
        let effective = manifold_core::effects::apply_card_reshape(
            card_value,
            spec.min,
            spec.max,
            spec.invert,
            spec.curve,
            binding.scale,
            binding.offset,
        );
        if let Some(row) = find_node_param_row_mut(&mut snap.nodes, node_id, param) {
            row.current_value = effective;
        }
    }
}

/// Find the `ParamSnapshot` for `(node_id, param)` anywhere in `nodes`,
/// recursing into group bodies — a bound node can live inside a group
/// (BUG-103's glTF per-object case).
fn find_node_param_row_mut<'a>(
    nodes: &'a mut [manifold_node_engine::snapshot::NodeSnapshot],
    node_id: &manifold_core::NodeId,
    param: &str,
) -> Option<&'a mut manifold_node_engine::snapshot::ParamSnapshot> {
    for node in nodes.iter_mut() {
        if &node.node_id == node_id
            && let Some(row) = node.parameters.iter_mut().find(|p| p.name == param)
        {
            return Some(row);
        }
        if let Some(group) = node.group.as_mut()
            && let Some(row) = find_node_param_row_mut(&mut group.nodes, node_id, param)
        {
            return Some(row);
        }
    }
    None
}
