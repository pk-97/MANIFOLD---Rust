//! Shared graph-build pipeline used by every JSON-to-runtime path.
//!
//! Two callers consume this module today:
//!
//! - **Generator path** ([`crate::node_graph::persistence::EffectGraphDefExt::into_graph`])
//!   instantiates a standalone preset into a fresh [`Graph`]. Every node in
//!   the def, including the `system.generator_input` and `system.final_output`
//!   boundaries, becomes a regular graph node.
//! - **Effect splice path** ([`crate::node_graph::chain_spec::splice_def_into_chain`])
//!   grafts an effect preset's worker subgraph into an existing chain
//!   [`Graph`]. The def's `system.source` boundary disappears (its fan-out
//!   re-anchors to the chain's previous endpoint), and `system.final_output`
//!   disappears (the wire feeding it identifies the spliced subgraph's
//!   output endpoint).
//!
//! Both paths share every per-node feature: WGSL source install, per-param
//! type-checked overrides, per-output format overrides, per-output canvas-
//! scale overrides, exposed-param seeding. The same single function applies
//! the same set of features so neither side can silently lack one — this
//! module's existence is the structural fix for the drift bug class that
//! produced the May 2026 Blob Track HUD outage (commits 3500e7a7, a69a71bf,
//! and the audit follow-up).
//!
//! Step B adds post-compile resource pre-allocation ([`pre_allocate_resources`])
//! to the same shared layer. Both callers now pre-allocate Array<T>
//! buffers + Texture3D volumes and run the post-allocation audit through
//! one function — the effect-side `pre_allocate_array_buffers_effect`
//! shim (added in commit 3500e7a7) is replaced by this single canonical
//! pipeline.

use std::borrow::Cow;

use ahash::AHashMap;

use manifold_core::effect_graph_def::{
    EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef, EffectGraphNode,
};
use manifold_gpu::{
    GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

use crate::node_graph::backend::Backend;
use crate::node_graph::boundary_nodes::{
    FINAL_OUTPUT_TYPE_ID, GENERATOR_INPUT_TYPE_ID, SOURCE_TYPE_ID,
};
use crate::node_graph::effect_node::{NodeInstanceId, ParamValues};
use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId};
use crate::node_graph::graph::Graph;
use crate::node_graph::metal_backend::MetalBackend;
use crate::node_graph::parameters::{ParamType, ParamValue};
use crate::node_graph::ports::PortType;
use crate::node_graph::persistence::{PrimitiveRegistry, format_from_str};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// How handle names declared in the def map onto the live [`Graph`].
#[derive(Debug, Clone, Copy)]
pub enum HandleScope {
    /// Register every handle on the graph itself via `add_node_named`.
    /// Used by the generator path — one preset per graph, so handle
    /// names cannot collide.
    Global,
    /// Return handles in [`NodeInstantiation::effect_local_handles`] and
    /// do not register them on the graph. Used by the effect splice
    /// path — multiple presets share one chain graph and may declare
    /// colliding handle names ("mix", "feedback").
    PerSplice,
}

/// What to do with the def's boundary nodes during instantiation.
#[derive(Debug, Clone, Copy)]
pub enum BoundaryHandling {
    /// Instantiate every boundary node (`system.source`,
    /// `system.generator_input`, `system.final_output`) as a regular
    /// graph node. Wire translation is straight `id_map` remapping.
    /// Used by the generator path.
    Standalone,
    /// Fold `system.source` and `system.final_output` away. Wires fanning
    /// out from `system.source` re-anchor to `source_endpoint`; the wire
    /// feeding `system.final_output` identifies the spliced subgraph's
    /// output endpoint, returned in
    /// [`NodeInstantiation::output_endpoint`]. `system.generator_input`,
    /// if present, is instantiated (effect per-frame scalar boundary).
    Splice {
        source_endpoint: (NodeInstanceId, &'static str),
    },
}

/// Errors produced by [`instantiate_def`]. Both callers convert these
/// into their own error surfaces — [`crate::node_graph::LoadError`] on
/// the generator path, `Option::None` + structured log on the splice
/// path. Every variant carries enough context (`node_id`, `type_id`,
/// optional `handle`) for a future editor surface to highlight the
/// affected node.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphBuildError {
    UnsupportedVersion {
        found: u32,
        max: u32,
    },
    DuplicateNodeId(u32),
    UnknownTypeId {
        node_id: u32,
        type_id: String,
    },
    UnknownNodeRef {
        wire_index: usize,
        node_id: u32,
        side: WireSide,
    },
    UnknownParam {
        node_id: u32,
        type_id: String,
        param: String,
    },
    ParamTypeMismatch {
        node_id: u32,
        type_id: String,
        param: String,
        expected: &'static str,
        got: &'static str,
    },
    InvalidWire {
        wire_index: usize,
        reason: String,
    },
    UnknownOutputFormat {
        node_id: u32,
        type_id: String,
        port: String,
        format: String,
    },
    OutputFormatNotSupported {
        node_id: u32,
        type_id: String,
        port: String,
        format: String,
    },
    MissingBoundarySource,
    MissingBoundaryFinalOutput,
    /// A node group failed to flatten into a flat document before
    /// instantiation. See [`manifold_core::flatten::FlattenError`].
    Flatten(manifold_core::flatten::FlattenError),
}

/// Which side of a wire failed to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireSide {
    From,
    To,
}

/// What [`instantiate_def`] produced. Every field is populated regardless
/// of which boundary mode was requested; fields that don't apply to the
/// requested mode are `None` / empty.
#[derive(Debug)]
pub struct NodeInstantiation {
    /// Doc-id → runtime-id remap. Exposed so callers can perform further
    /// wire surgery (today only the editor's snapshot path uses this; the
    /// chain build does not).
    pub id_map: AHashMap<u32, NodeInstanceId>,

    /// Effect-local handle map. Populated when `handle_scope = PerSplice`.
    /// Empty when `handle_scope = Global` — handles are on the graph
    /// itself in that case.
    pub effect_local_handles: Vec<(Cow<'static, str>, NodeInstanceId)>,

    /// Output endpoint of the spliced subgraph. `Some` for
    /// `BoundaryHandling::Splice`, `None` for `Standalone`.
    pub output_endpoint: Option<(NodeInstanceId, &'static str)>,

    /// The instantiated `system.generator_input` node id, if the def
    /// contained one. Both boundary modes return this when present.
    /// Generator path: `JsonGraphGenerator::set_frame_context` writes to
    /// this node. Effect path (planned phase 2): chain runner pushes
    /// per-frame scalars to this node so effects can react to project
    /// BPM / beat / aspect alongside their texture input.
    pub generator_input_id: Option<NodeInstanceId>,

    /// The instantiated `system.final_output` node id, present only for
    /// `BoundaryHandling::Standalone`. The host pre-binds the target
    /// texture to its `in` resource.
    pub final_output_id: Option<NodeInstanceId>,
}

// ---------------------------------------------------------------------------
// The shared per-node + per-wire pipeline
// ---------------------------------------------------------------------------

/// Instantiate every (non-folded) node from `def` into `graph`, apply
/// every per-node JSON feature, then translate wires according to
/// `boundary`.
///
/// Per-node features applied in order, before the node is moved into
/// the graph:
///
/// 1. **`wgsl_source`** — installed on the boxed node pre-`add_node` so
///    dynamic-shape primitives (`node.wgsl_compute`) reparse their port
///    list before parameter validation reads it.
/// 2. **`params`** — type-checked against the node's declared
///    [`ParamType`] list. Mismatches emit
///    [`GraphBuildError::ParamTypeMismatch`].
/// 3. **`output_formats`** — applied via [`Graph::set_output_format`]
///    with a post-set audit: primitives whose shader hard-codes its
///    output format silently no-op `set_output_format`, so writing
///    `outputFormats` against them silently dropped the override before
///    this audit existed. Now a no-op write is
///    [`GraphBuildError::OutputFormatNotSupported`].
/// 4. **`output_canvas_scales`** — applied via
///    [`Graph::set_output_canvas_scale`]. No audit (no shipping primitive
///    accepts canvas-scale yet besides `node.wgsl_compute`, which honours
///    every write).
/// 5. **Handle registration** — `add_node_named` for
///    [`HandleScope::Global`], owned `Cow::Owned` for
///    [`HandleScope::PerSplice`].
///
/// Wire translation then runs according to `boundary`:
///
/// - **`Standalone`** — every wire's `(from_node, to_node)` pair gets
///   remapped via `id_map`. Boundary nodes are regular graph nodes.
/// - **`Splice { source_endpoint }`** — wires from the def's
///   `system.source` re-anchor to `source_endpoint`; the wire feeding
///   the def's `system.final_output` identifies the splice's output
///   endpoint and is not connected. All other wires remap normally.
///
/// Migrate every node's `type_id` (recursing into group bodies) via
/// [`manifold_core::type_id_migration::migrate_type_id`], then apply any
/// matching [`manifold_core::type_id_migration::PARAM_SEED_MIGRATIONS`]
/// entry — a rename whose retired node had no direct id-for-id equivalent
/// (e.g. `node.rotate_vec2_90`'s fixed 90° folding into `node.rotate_vector`'s
/// general `angle` param) writes the params that reproduce the retired
/// node's fixed behavior, keyed by the ORIGINAL id so group-internal nodes
/// get seeded exactly like top-level ones. Seeding never overwrites a param
/// key already present on the node (`entry().or_insert()`), matching
/// "seed the default", not "force the value" — a document that already
/// carries an explicit value for that param (from a later hand edit)
/// keeps it. Returns `Some` only when at least one id actually changed, so
/// the overwhelmingly common already-current document — every bundled
/// preset, every project saved since its last rename — passes through
/// borrowed rather than cloned.
fn migrate_def_type_ids(def: &EffectGraphDef, registry: &PrimitiveRegistry) -> Option<EffectGraphDef> {
    fn migrate_nodes(nodes: &mut [EffectGraphNode], registry: &PrimitiveRegistry, changed: &mut bool) {
        for n in nodes {
            let old_id = n.type_id.clone();
            let new_id = manifold_core::type_id_migration::migrate_type_id(&old_id);
            if new_id != old_id {
                n.type_id = new_id.to_string();
                *changed = true;
                for (seed_old, seed_new, seed_params) in
                    manifold_core::type_id_migration::PARAM_SEED_MIGRATIONS
                {
                    if *seed_old == old_id && *seed_new == new_id {
                        for (param_name, param_value) in *seed_params {
                            n.params
                                .entry((*param_name).to_string())
                                .or_insert_with(|| param_value.clone());
                        }
                    }
                }
                // A rename is not always port/param-identical (e.g.
                // node.ssao_from_depth -> node.ssao_gtao, CINEMATIC_POST
                // D9: `bias` has no successor). A param key the new node
                // doesn't declare would otherwise hit
                // `GraphBuildError::UnknownParam` below and hard-fail the
                // load of every pre-rename project/preset that still
                // carries it. Drop params the successor doesn't declare —
                // per the round-trip gate (DESIGN_DOC_STANDARD §5), a
                // migrated load must succeed, not merely a fresh save.
                if let Some(new_node) = registry.construct(new_id) {
                    let known: ahash::AHashSet<&str> =
                        new_node.parameters().iter().map(|p| p.name.as_ref()).collect();
                    n.params.retain(|k, _| known.contains(k.as_str()));
                }
            }
            if let Some(group) = &mut n.group {
                migrate_nodes(&mut group.nodes, registry, changed);
            }
        }
    }

    let mut changed = false;
    let mut owned = def.clone();
    migrate_nodes(&mut owned.nodes, registry, &mut changed);
    changed.then_some(owned)
}

/// GLTF_ANIM_RUNTIME_V2_DESIGN.md D5: a project/preset saved before P2
/// baked keyframe/topology payload straight into the three glTF sampler
/// nodes' Table params (`node.gltf_skeleton_pose`'s six tables,
/// `node.gltf_animation_source`'s `translation_track`/`rotation_track`/
/// `scale_track`, `node.gltf_morph_weights`' `weight_tracks`). This strips
/// those dead Tables and stamps a `path` stringBinding from the SAME
/// group's sibling `node.gltf_mesh_source`/`node.gltf_skinned_mesh_source`
/// node, when one resolves — so the sampler reads from the shared
/// `gltf_anim_cache` instead. `target_node`/`skin_index` are NOT
/// recoverable from the old flat Tables (they never recorded which glTF
/// scene-node index the baked values came from) — the migration leaves
/// them at their existing default (0 unless the def already carries a
/// stamped value, the P1-era case for `skeleton_pose`'s `skin_index`),
/// documented residual limitation for a genuinely pre-P2 multi-object
/// asset, never silently wrong for the common single-object-per-group
/// case the importer has always produced. A node whose sibling path can't
/// be resolved keeps its tables — inert but present, per the round-trip
/// corollary (never a silent drop, never a parallel sampling path reading
/// old tables) — and one warning names it. Idempotent: a def carrying none
/// of the old table keys is untouched (`false`, no clone). Same
/// single-choke-point placement as `migrate_def_type_ids` — every loader
/// converges on `instantiate_def`.
fn migrate_gltf_anim_v2(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::BindingTarget;

    const POSE_KEYS: &[&str] = &[
        "joint_parent_table",
        "joint_root_world_table",
        "inverse_bind_table",
        "translation_tracks",
        "rotation_tracks",
        "scale_tracks",
    ];
    const ANIM_KEYS: &[&str] = &["translation_track", "rotation_track", "scale_track"];
    const MORPH_KEYS: &[&str] = &["weight_tracks"];

    fn old_table_keys(type_id: &str) -> Option<&'static [&'static str]> {
        match type_id {
            "node.gltf_skeleton_pose" => Some(POSE_KEYS),
            "node.gltf_animation_source" => Some(ANIM_KEYS),
            "node.gltf_morph_weights" => Some(MORPH_KEYS),
            _ => None,
        }
    }

    // node_id -> path default, for every stringBinding that already
    // targets SOME node's `path` param (sampler nodes stamped by P1+, and
    // every mesh-source-family node the importer has always stamped).
    let existing_paths: ahash::AHashMap<String, String> = def
        .preset_metadata
        .as_ref()
        .map(|m| {
            m.string_bindings
                .iter()
                .filter_map(|b| match &b.target {
                    BindingTarget::Node { node_id, param } if param == "path" => {
                        Some((node_id.as_str().to_string(), b.default_value.clone()))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    // Recurse scope-by-scope (top-level `def.nodes`, then each group body —
    // same "structurally identical at any depth" shape
    // `scene_object_migration::migrate_scope` uses): within one scope, a
    // mesh-source-family sibling's resolved path is this scope's fallback
    // for every sampler node in it that doesn't already have its own.
    #[allow(clippy::too_many_arguments)]
    fn migrate_scope(
        nodes: &mut [EffectGraphNode],
        existing_paths: &ahash::AHashMap<String, String>,
        can_add_binding: bool,
        new_bindings: &mut Vec<(String, String)>,
        changed: &mut bool,
        warnings: &mut Vec<String>,
    ) {
        let sibling_path: Option<&String> = nodes
            .iter()
            .find(|n| matches!(n.type_id.as_str(), "node.gltf_mesh_source" | "node.gltf_skinned_mesh_source"))
            .and_then(|n| existing_paths.get(n.node_id.as_str()));

        for n in nodes.iter_mut() {
            if let Some(keys) = old_table_keys(&n.type_id) {
                let carries_old = keys.iter().any(|k| n.params.contains_key(*k));
                if carries_old {
                    let node_id = n.node_id.as_str().to_string();
                    let own_path = existing_paths.get(&node_id);
                    // A NEW binding (the sibling-fallback case) can only be
                    // applied when `def.preset_metadata` exists to hold it
                    // — a per-instance override without metadata (see
                    // `PresetMetadata`'s doc comment) has nowhere to put
                    // one, so stripping the tables there would leave the
                    // node with neither payload nor a path. Own-path
                    // (already stamped, P1+) never needs a new binding.
                    let path = own_path.or(if can_add_binding { sibling_path } else { None });
                    match path {
                        Some(p) => {
                            for k in keys {
                                n.params.remove(*k);
                            }
                            if own_path.is_none() {
                                new_bindings.push((node_id, p.clone()));
                            }
                            *changed = true;
                        }
                        None => warnings.push(format!(
                            "{node_id} ({}): no sibling mesh-source path resolved — keyframe \
                             tables kept inert, not stripped",
                            n.type_id
                        )),
                    }
                }
            }
            if let Some(group) = &mut n.group {
                migrate_scope(&mut group.nodes, existing_paths, can_add_binding, new_bindings, changed, warnings);
            }
        }
    }

    let mut changed = false;
    let mut new_bindings = Vec::new();
    let mut warnings = Vec::new();
    let can_add_binding = def.preset_metadata.is_some();
    migrate_scope(&mut def.nodes, &existing_paths, can_add_binding, &mut new_bindings, &mut changed, &mut warnings);

    for w in &warnings {
        log::warn!("gltf_anim_v2 migration: {w}");
    }
    if !new_bindings.is_empty()
        && let Some(meta) = def.preset_metadata.as_mut()
    {
        for (node_id, path) in new_bindings {
            meta.string_bindings.push(manifold_core::effect_graph_def::StringBindingDef {
                id: format!("gltf_anim_v2_migrated_path_{node_id}"),
                label: "Model File".to_string(),
                default_value: path,
                target: BindingTarget::Node {
                    node_id: manifold_core::NodeId::new(&node_id),
                    param: "path".to_string(),
                },
            });
        }
    }

    changed
}

/// Returns the [`NodeInstantiation`] on success. On any error the
/// graph's state is the union of every successful step before the
/// failure — both callers handle this by either propagating
/// (generator, where the whole load aborts) or falling back to a
/// canonical def (splice, where the orphaned partial graph is the
/// price of "try divergent, then canonical").
pub fn instantiate_def(
    graph: &mut Graph,
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    handle_scope: HandleScope,
    boundary: BoundaryHandling,
) -> Result<NodeInstantiation, GraphBuildError> {
    if def.version > EFFECT_GRAPH_VERSION_WITH_METADATA {
        return Err(GraphBuildError::UnsupportedVersion {
            found: def.version,
            max: EFFECT_GRAPH_VERSION_WITH_METADATA,
        });
    }

    // Migrate legacy node type_ids before anything else runs, including
    // inside group bodies — the group flatten below only rewires structure,
    // so a rename must land first or a group-internal node keeps its stale
    // id forever. Every loader (generator load, effect splice, freeze/proof
    // harnesses) converges here, so this is the single choke point content
    // written before a rename ever needs (see
    // `manifold_core::type_id_migration`). Old ids are never reused, so this
    // is a pure, idempotent string swap. A document needing no migration
    // (the common case, always true once a project has been opened and
    // resaved once) passes through as a borrow, matching the group-flatten
    // pattern just below.
    let migrated;
    let def = match migrate_def_type_ids(def, registry) {
        Some(owned) => {
            migrated = owned;
            &migrated
        }
        None => def,
    };

    // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D5: migrate `node.render_scene`'s
    // legacy per-object port wiring (`mesh_k`/`material_k`/17 maps/
    // `transform_k`/`instances_k`) into `node.scene_object` nodes feeding
    // `object_k`. Same single-choke-point placement as the type-id
    // migration just above — every loader (project load, bundled/reference
    // preset load, user-library preset load, `graph_tool migrate`, AND the
    // live glTF importer's freshly-built output, which still emits the
    // legacy shape until P3 repoints it) converges on `instantiate_def`, so
    // landing the migration here covers all of them without a separate
    // call site per producer. Structural, no version gate — idempotence is
    // the gate (a def with no legacy wires is untouched, `false`, no
    // clone). Must run AFTER type-id migration (a renamed old-shape
    // render_scene, if one ever existed, is caught first) and BEFORE the
    // group flatten below (the mint needs the def's still-nested group
    // structure to place scene_object in the right scope).
    let mut scene_object_migrated = def.clone();
    let def = if manifold_core::scene_object_migration::migrate_scene_object_wires(
        &mut scene_object_migrated,
    ) {
        &scene_object_migrated
    } else {
        def
    };

    // GLTF_ANIM_RUNTIME_V2_DESIGN.md D5/P2: same single-choke-point
    // placement — strips pre-P2 keyframe/topology Tables off the three
    // glTF sampler node types, stamping `path` from a sibling mesh-source
    // when resolvable. Order-independent relative to the two migrations
    // above (disjoint node types/params); placed after both for the same
    // "every migration converges here" reason.
    let mut gltf_anim_migrated = def.clone();
    let def = if migrate_gltf_anim_v2(&mut gltf_anim_migrated) { &gltf_anim_migrated } else { def };

    // Fold any node groups before anything else runs. After this the def
    // contains no `group` nodes, so every path below — the boundary scan,
    // per-node construction, wire translation — sees a flat document and is
    // unchanged. Groupless documents pass through as a cheap clone, so the
    // overwhelmingly common case is untouched. This is the *group* boundary
    // (`system.group_input`/`output`); the *effect* boundary
    // (`system.source`/`final_output`) handled below is a separate layer.
    let flattened;
    let def = if def.nodes.iter().any(|n| n.group.is_some()) {
        flattened = manifold_core::flatten::flatten_groups(def).map_err(GraphBuildError::Flatten)?;
        &flattened
    } else {
        def
    };

    // For Splice, identify the def's Source and FinalOutput up front so
    // we know which nodes to skip during instantiation and which wires
    // to fold during translation.
    let (def_source_id, def_final_id) = match boundary {
        BoundaryHandling::Standalone => (None, None),
        BoundaryHandling::Splice { .. } => {
            let mut src: Option<u32> = None;
            let mut fin: Option<u32> = None;
            for n in &def.nodes {
                if n.type_id == SOURCE_TYPE_ID {
                    src = Some(n.id);
                } else if n.type_id == FINAL_OUTPUT_TYPE_ID {
                    fin = Some(n.id);
                }
            }
            (
                Some(src.ok_or(GraphBuildError::MissingBoundarySource)?),
                Some(fin.ok_or(GraphBuildError::MissingBoundaryFinalOutput)?),
            )
        }
    };

    let mut id_map: AHashMap<u32, NodeInstanceId> = AHashMap::default();
    let mut effect_local_handles: Vec<(Cow<'static, str>, NodeInstanceId)> = Vec::new();
    let mut generator_input_id: Option<NodeInstanceId> = None;
    let mut final_output_id: Option<NodeInstanceId> = None;

    // ── Per-node instantiation pass ──
    for node_doc in &def.nodes {
        // Splice folds these two boundary nodes — don't instantiate.
        if Some(node_doc.id) == def_source_id || Some(node_doc.id) == def_final_id {
            continue;
        }

        if id_map.contains_key(&node_doc.id) {
            return Err(GraphBuildError::DuplicateNodeId(node_doc.id));
        }

        let mut boxed = registry
            .construct(&node_doc.type_id)
            .ok_or_else(|| GraphBuildError::UnknownTypeId {
                node_id: node_doc.id,
                type_id: node_doc.type_id.clone(),
            })?;

        // (1) WGSL source — install on the box BEFORE `add_node` so the
        // node's reparse runs while we still own it. Static-shape
        // primitives' `set_wgsl_source` is a no-op, so this is free for
        // the common case.
        if let Some(source) = node_doc.wgsl_source.as_deref() {
            boxed.set_wgsl_source(source);
        }

        // Reconfigure dynamic-surface nodes from the doc's params BEFORE the
        // snapshot below. `node.reconfigure` (a no-op for static-shape
        // primitives) rebuilds a node's port/param surface from its
        // reconfigure params — `objects`/`lights` for `node.render_scene`,
        // `num_inputs` for `node.mux_texture`/`node.multi_blend`. The runtime
        // already calls it after every node build (graph.rs, snapshot.rs,
        // freeze/region.rs); the loader was the one path that didn't, so a
        // node whose PARAM set grows with a reconfigure param (render_scene:
        // `pos_x_2`.. exist only when `objects >= 3`) had those params
        // validated against the default-count surface and rejected as unknown
        // — the "unknown parameter 'pos_x_2'" glTF-import load failure. Seed
        // the declared defaults, override with the doc's values, reconfigure;
        // then the snapshot reflects the true surface. Mirrors snapshot.rs.
        {
            let seed: Vec<(std::borrow::Cow<'static, str>, ParamValue)> = boxed
                .parameters()
                .iter()
                .map(|p| (p.name.clone(), p.default.clone()))
                .collect();
            let mut reconfig_params: ParamValues = ahash::AHashMap::default();
            for (name, default) in &seed {
                reconfig_params.insert(name.clone(), default.clone());
            }
            for (key, value) in &node_doc.params {
                if let Some((name, _)) = seed.iter().find(|(n, _)| *n == key.as_str()) {
                    reconfig_params.insert(name.clone(), value.clone().into());
                }
            }
            boxed.reconfigure(&reconfig_params);
        }

        // Snapshot the declared param surface BEFORE moving `boxed` into
        // the graph — we need this for type-checked param overrides
        // below, plus for the exposed-params validation pass.
        let param_defs: Vec<(&'static str, ParamType)> = boxed
            .parameters()
            .iter()
            .map(|p| (crate::node_graph::effect_node::intern_name(&p.name), p.ty))
            .collect();

        let runtime_id = match handle_scope {
            HandleScope::Global => {
                if let Some(handle) = node_doc.handle.as_deref() {
                    // `add_node_named` requires `&'static str`. We leak
                    // the handle string — bounded leak (one per inner
                    // node per preset load, ~30 per preset), amortized
                    // over the process lifetime. Same pattern persistence
                    // used pre-unification.
                    let static_handle: &'static str =
                        Box::leak(handle.to_string().into_boxed_str());
                    graph.add_node_named(static_handle, boxed)
                } else {
                    graph.add_node(boxed)
                }
            }
            HandleScope::PerSplice => graph.add_node(boxed),
        };
        id_map.insert(node_doc.id, runtime_id);
        // Copy the stable document identity onto the live instance so param
        // bindings can resolve to it regardless of handle / nesting. A
        // node's id **defaults to its handle** when the document carries
        // none — pre-node-id documents, or any graph JSON loaded outside
        // `Project` normalization (hand-authored defs, `from_json_str`).
        // This is the runtime chokepoint every def→graph path funnels
        // through, so the "node_id defaults to handle" convention (shared
        // with the preset stamp + the `BindingTarget` deserialize) holds
        // uniformly here: a handle-targeted binding resolves no matter how
        // the def reached us.
        let resolved_node_id = if node_doc.node_id.is_empty() {
            node_doc
                .handle
                .as_deref()
                .map(manifold_core::NodeId::new)
                .unwrap_or_default()
        } else {
            node_doc.node_id.clone()
        };
        graph.set_node_id(runtime_id, resolved_node_id);

        // Author-supplied display title — honored for every node type now.
        // The snapshot builder adds the `(WGSL)` marker for wgsl_compute, so
        // a hand-written shader still reads as custom; regular nodes just get
        // the friendly name the author chose (e.g. a `node.value` hub labelled
        // "Amount" vs "Speed" instead of two identical "Value" headers).
        if let Some(title) = &node_doc.title
            && let Some(inst) = graph.get_node_mut(runtime_id)
        {
            inst.title = Some(title.clone());
        }

        // PerSplice: record the handle in the effect-local map. Owned
        // Cow because the handle string comes off disk and we don't
        // want to leak per-chain-build.
        if let HandleScope::PerSplice = handle_scope
            && let Some(handle_name) = node_doc.handle.as_deref()
        {
            effect_local_handles.push((Cow::Owned(handle_name.to_owned()), runtime_id));
        }

        // (2) Param overrides — type-checked.
        for (key, value) in &node_doc.params {
            let Some(&(name_static, expected_ty)) =
                param_defs.iter().find(|(n, _)| *n == key.as_str())
            else {
                return Err(GraphBuildError::UnknownParam {
                    node_id: node_doc.id,
                    type_id: node_doc.type_id.clone(),
                    param: key.clone(),
                });
            };
            let pv: ParamValue = value.clone().into();
            if !param_value_matches_type(&pv, expected_ty) {
                return Err(GraphBuildError::ParamTypeMismatch {
                    node_id: node_doc.id,
                    type_id: node_doc.type_id.clone(),
                    param: key.clone(),
                    expected: param_type_name(expected_ty),
                    got: param_type_label(&pv),
                });
            }
            // set_param can only fail with NodeNotFound (just added) or
            // ParamNotFound (just validated). Both impossible here.
            graph
                .set_param(runtime_id, name_static, pv)
                .expect("validated above");
        }

        // (3) Exposed params — Global scope only. Splice path effects
        // expose via `PresetInstance.user_param_bindings` at a different
        // layer.
        if let HandleScope::Global = handle_scope {
            for exposed_name in &node_doc.exposed_params {
                if let Some(&(name_static, _)) =
                    param_defs.iter().find(|(n, _)| *n == exposed_name.as_str())
                {
                    graph
                        .set_param_exposed(runtime_id, name_static, true)
                        .expect("just added");
                }
            }
        }

        // (4) Output format overrides + audit.
        for (port_name, fmt_str) in &node_doc.output_formats {
            let Some(fmt) = format_from_str(fmt_str) else {
                return Err(GraphBuildError::UnknownOutputFormat {
                    node_id: node_doc.id,
                    type_id: node_doc.type_id.clone(),
                    port: port_name.clone(),
                    format: fmt_str.clone(),
                });
            };
            graph
                .set_output_format(runtime_id, port_name, fmt)
                .expect("just added");
            // Audit: a primitive whose shader hardcodes its output format
            // has a no-op `set_output_format`. Writing `outputFormats`
            // against it silently dropped before this check existed;
            // catch it loudly at load time.
            let inst = graph.get_node(runtime_id).expect("just added");
            if inst.node.output_format(port_name) != Some(fmt) {
                return Err(GraphBuildError::OutputFormatNotSupported {
                    node_id: node_doc.id,
                    type_id: node_doc.type_id.clone(),
                    port: port_name.clone(),
                    format: fmt_str.clone(),
                });
            }
        }

        // (5) Output canvas-scale overrides. Honoured today only by
        // `node.wgsl_compute`; every other primitive has a no-op default.
        for (port_name, scale) in &node_doc.output_canvas_scales {
            let &[num, denom] = scale;
            graph
                .set_output_canvas_scale(runtime_id, port_name, (num, denom))
                .expect("just added");
        }

        // (6) Stash boundary node ids on the way through so the caller
        // can find them without a second scan.
        if node_doc.type_id == GENERATOR_INPUT_TYPE_ID {
            generator_input_id = Some(runtime_id);
        }
        if node_doc.type_id == FINAL_OUTPUT_TYPE_ID {
            // Only reachable on Standalone — Splice folded this above.
            final_output_id = Some(runtime_id);
        }
    }

    // ── Wire translation pass ──
    let mut output_endpoint: Option<(NodeInstanceId, &'static str)> = None;
    for (wire_index, w) in def.wires.iter().enumerate() {
        match boundary {
            BoundaryHandling::Standalone => {
                let from_chain = *id_map
                    .get(&w.from_node)
                    .ok_or(GraphBuildError::UnknownNodeRef {
                        wire_index,
                        node_id: w.from_node,
                        side: WireSide::From,
                    })?;
                let to_chain =
                    *id_map.get(&w.to_node).ok_or(GraphBuildError::UnknownNodeRef {
                        wire_index,
                        node_id: w.to_node,
                        side: WireSide::To,
                    })?;
                let from_port = resolve_output_port(graph, from_chain, &w.from_port).ok_or_else(
                    || GraphBuildError::InvalidWire {
                        wire_index,
                        reason: format!(
                            "from node {} has no output port '{}'",
                            w.from_node, w.from_port
                        ),
                    },
                )?;
                let to_port = resolve_input_port(graph, to_chain, &w.to_port).ok_or_else(|| {
                    GraphBuildError::InvalidWire {
                        wire_index,
                        reason: format!(
                            "to node {} has no input port '{}'",
                            w.to_node, w.to_port
                        ),
                    }
                })?;
                graph
                    .connect((from_chain, from_port), (to_chain, to_port))
                    .map_err(|e| GraphBuildError::InvalidWire {
                        wire_index,
                        reason: format!("{e:?}"),
                    })?;
            }
            BoundaryHandling::Splice { source_endpoint } => {
                // Source-fanout: re-anchor.
                if Some(w.from_node) == def_source_id {
                    let to_chain = *id_map.get(&w.to_node).ok_or(
                        GraphBuildError::UnknownNodeRef {
                            wire_index,
                            node_id: w.to_node,
                            side: WireSide::To,
                        },
                    )?;
                    let to_port = resolve_input_port(graph, to_chain, &w.to_port).ok_or_else(
                        || GraphBuildError::InvalidWire {
                            wire_index,
                            reason: format!(
                                "to node {} has no input port '{}'",
                                w.to_node, w.to_port
                            ),
                        },
                    )?;
                    graph
                        .connect(source_endpoint, (to_chain, to_port))
                        .map_err(|e| GraphBuildError::InvalidWire {
                            wire_index,
                            reason: format!("{e:?}"),
                        })?;
                    continue;
                }
                // FinalOutput-feed: identify output endpoint, do not connect.
                if Some(w.to_node) == def_final_id {
                    let from_chain = *id_map.get(&w.from_node).ok_or(
                        GraphBuildError::UnknownNodeRef {
                            wire_index,
                            node_id: w.from_node,
                            side: WireSide::From,
                        },
                    )?;
                    let from_port =
                        resolve_output_port(graph, from_chain, &w.from_port).ok_or_else(|| {
                            GraphBuildError::InvalidWire {
                                wire_index,
                                reason: format!(
                                    "from node {} has no output port '{}'",
                                    w.from_node, w.from_port
                                ),
                            }
                        })?;
                    output_endpoint = Some((from_chain, from_port));
                    continue;
                }
                // Normal wire.
                let from_chain = *id_map.get(&w.from_node).ok_or(
                    GraphBuildError::UnknownNodeRef {
                        wire_index,
                        node_id: w.from_node,
                        side: WireSide::From,
                    },
                )?;
                let to_chain =
                    *id_map.get(&w.to_node).ok_or(GraphBuildError::UnknownNodeRef {
                        wire_index,
                        node_id: w.to_node,
                        side: WireSide::To,
                    })?;
                let from_port = resolve_output_port(graph, from_chain, &w.from_port).ok_or_else(
                    || GraphBuildError::InvalidWire {
                        wire_index,
                        reason: format!(
                            "from node {} has no output port '{}'",
                            w.from_node, w.from_port
                        ),
                    },
                )?;
                let to_port = resolve_input_port(graph, to_chain, &w.to_port).ok_or_else(|| {
                    GraphBuildError::InvalidWire {
                        wire_index,
                        reason: format!(
                            "to node {} has no input port '{}'",
                            w.to_node, w.to_port
                        ),
                    }
                })?;
                graph
                    .connect((from_chain, from_port), (to_chain, to_port))
                    .map_err(|e| GraphBuildError::InvalidWire {
                        wire_index,
                        reason: format!("{e:?}"),
                    })?;
            }
        }
    }

    Ok(NodeInstantiation {
        id_map,
        effect_local_handles,
        output_endpoint,
        generator_input_id,
        final_output_id,
    })
}

// ---------------------------------------------------------------------------
// Param-type helpers
// ---------------------------------------------------------------------------

/// Whether a [`ParamValue`] satisfies a declared [`ParamType`]. The Int
/// collapse means `ParamType::Int` accepts `ParamValue::Float` (storage
/// is `Float` only since the legacy Int variant was removed).
pub(crate) fn param_value_matches_type(v: &ParamValue, ty: ParamType) -> bool {
    matches!(
        (ty, v),
        (ParamType::Float, ParamValue::Float(_))
            | (ParamType::Angle, ParamValue::Float(_))
            | (ParamType::Frequency, ParamValue::Float(_))
            | (ParamType::Int, ParamValue::Float(_))
            | (ParamType::Bool, ParamValue::Bool(_))
            | (ParamType::Vec2, ParamValue::Vec2(_))
            | (ParamType::Vec3, ParamValue::Vec3(_))
            | (ParamType::Vec4, ParamValue::Vec4(_))
            | (ParamType::Color, ParamValue::Color(_))
            | (ParamType::Enum, ParamValue::Enum(_))
            | (ParamType::Table, ParamValue::Table(_))
            | (ParamType::String, ParamValue::String(_))
            | (ParamType::Trigger, ParamValue::Float(_))
    )
}

/// Tag for the declared `ParamType` side of a mismatch error.
pub(crate) fn param_type_name(ty: ParamType) -> &'static str {
    match ty {
        ParamType::Float => "Float",
        ParamType::Angle => "Angle",
        ParamType::Frequency => "Frequency",
        ParamType::Int => "Int",
        ParamType::Bool => "Bool",
        ParamType::Vec2 => "Vec2",
        ParamType::Vec3 => "Vec3",
        ParamType::Vec4 => "Vec4",
        ParamType::Color => "Color",
        ParamType::Enum => "Enum",
        ParamType::Table => "Table",
        ParamType::String => "String",
        ParamType::Trigger => "Trigger",
    }
}

/// Tag for the `ParamValue` side of a mismatch error.
pub(crate) fn param_type_label(v: &ParamValue) -> &'static str {
    match v {
        ParamValue::Float(_) => "Float",
        ParamValue::Bool(_) => "Bool",
        ParamValue::Vec2(_) => "Vec2",
        ParamValue::Vec3(_) => "Vec3",
        ParamValue::Vec4(_) => "Vec4",
        ParamValue::Color(_) => "Color",
        ParamValue::Enum(_) => "Enum",
        ParamValue::Table(_) => "Table",
        ParamValue::String(_) => "String",
    }
}

/// Single-line structured log helper. The terminal-readable shape callers
/// agree on so logs grep cleanly and the future editor surface can
/// attach errors to the right node.
pub fn log_build_error(context: &str, err: &GraphBuildError) {
    use std::fmt::Write;

    let mut buf = String::with_capacity(120);
    let _ = write!(buf, "[graph-build] {context}: ");
    match err {
        GraphBuildError::UnsupportedVersion { found, max } => {
            let _ = write!(buf, "unsupported version {found} (max {max})");
        }
        GraphBuildError::DuplicateNodeId(id) => {
            let _ = write!(buf, "duplicate node id {id}");
        }
        GraphBuildError::UnknownTypeId { node_id, type_id } => {
            let _ = write!(buf, "node {node_id}: unknown type id '{type_id}'");
        }
        GraphBuildError::UnknownNodeRef {
            wire_index,
            node_id,
            side,
        } => {
            let _ = write!(
                buf,
                "wire #{wire_index}: {side:?} references unknown node id {node_id}"
            );
        }
        GraphBuildError::UnknownParam {
            node_id,
            type_id,
            param,
        } => {
            let _ = write!(buf, "node {node_id} ({type_id}): unknown param '{param}'");
        }
        GraphBuildError::ParamTypeMismatch {
            node_id,
            type_id,
            param,
            expected,
            got,
        } => {
            let _ = write!(
                buf,
                "node {node_id} ({type_id}): param '{param}' expected {expected}, got {got}"
            );
        }
        GraphBuildError::InvalidWire { wire_index, reason } => {
            let _ = write!(buf, "wire #{wire_index}: {reason}");
        }
        GraphBuildError::UnknownOutputFormat {
            node_id,
            type_id,
            port,
            format,
        } => {
            let _ = write!(
                buf,
                "node {node_id} ({type_id}): output '{port}' unknown format '{format}'"
            );
        }
        GraphBuildError::OutputFormatNotSupported {
            node_id,
            type_id,
            port,
            format,
        } => {
            let _ = write!(
                buf,
                "node {node_id} ({type_id}): outputFormats.{port}='{format}' silently \
                 ignored (primitive's shader hardcodes its format)"
            );
        }
        GraphBuildError::MissingBoundarySource => {
            let _ = write!(buf, "splice def has no system.source boundary");
        }
        GraphBuildError::MissingBoundaryFinalOutput => {
            let _ = write!(buf, "splice def has no system.final_output boundary");
        }
        GraphBuildError::Flatten(e) => {
            let _ = write!(buf, "group flatten failed: {e}");
        }
    }
    eprintln!("{buf}");
}

// ---------------------------------------------------------------------------
// Port-name resolution helpers
// ---------------------------------------------------------------------------

fn resolve_input_port(graph: &Graph, node: NodeInstanceId, name: &str) -> Option<&'static str> {
    graph
        .get_node(node)?
        .node
        .inputs()
        .iter()
        .find(|p| p.name == name)
        .map(|p| crate::node_graph::effect_node::intern_name(&p.name))
}

fn resolve_output_port(graph: &Graph, node: NodeInstanceId, name: &str) -> Option<&'static str> {
    graph
        .get_node(node)?
        .node
        .outputs()
        .iter()
        .find(|p| p.name == name)
        .map(|p| crate::node_graph::effect_node::intern_name(&p.name))
}

// ---------------------------------------------------------------------------
// Post-compile resource pre-allocation (Array<T> + Texture3D + audit)
// ---------------------------------------------------------------------------

/// Errors produced by [`pre_allocate_resources`]. The variants carry
/// the offending node's `type_id`, port name, and handle (when present)
/// so callers can surface them with full context to the operator.
#[derive(Debug, Clone)]
pub enum PreAllocationError {
    /// A primitive declared an `Array<T>` output but
    /// `array_output_capacity()` returned `None` — pre-bound allocation
    /// is a hard contract, so partial allocation is rejected loudly
    /// rather than rendering silently wrong.
    UnsizedArrayOutput {
        node_type: String,
        port: String,
        handle: Option<String>,
    },
    /// A primitive declared a `Texture3D` output but
    /// `texture_3d_output_dims()` returned `None`. Texture3D has no
    /// lazy-alloc path, so a missing sizing implementation can't go
    /// silent.
    UnsizedTexture3DOutput {
        node_type: String,
        port: String,
        handle: Option<String>,
    },
    /// Post-allocation audit catch-all: an `Array<T>` resource has no
    /// bound slot, or its slot has no buffer. Catches alias chain
    /// breaks, canvas-dim-zero skips, and future allocation paths
    /// that fail silently — anything the cause-layer checks above
    /// haven't enumerated.
    UnboundArrayResource {
        producer_node_type: String,
        producer_port: String,
        producer_handle: Option<String>,
        cause: &'static str,
    },
}

impl std::fmt::Display for PreAllocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsizedArrayOutput {
                node_type,
                port,
                handle,
            } => {
                let h = handle.as_deref().map(|s| format!(" (handle `{s}`)")).unwrap_or_default();
                write!(
                    f,
                    "primitive `{node_type}`{h} Array<T> output port `{port}` has no \
                     concrete size — `array_output_capacity` returned None. \
                     Add a `max_capacity` param, or override the method to derive \
                     size from a forward-dep input."
                )
            }
            Self::UnsizedTexture3DOutput {
                node_type,
                port,
                handle,
            } => {
                let h = handle.as_deref().map(|s| format!(" (handle `{s}`)")).unwrap_or_default();
                write!(
                    f,
                    "primitive `{node_type}`{h} Texture3D output port `{port}` has no \
                     concrete dims — `texture_3d_output_dims` returned None."
                )
            }
            Self::UnboundArrayResource {
                producer_node_type,
                producer_port,
                producer_handle,
                cause,
            } => {
                let h = producer_handle
                    .as_deref()
                    .map(|s| format!(" (handle `{s}`)"))
                    .unwrap_or_default();
                write!(
                    f,
                    "Array<T> output of `{producer_node_type}.{producer_port}`{h} \
                     has no bound buffer after chain build: {cause}"
                )
            }
        }
    }
}

impl std::error::Error for PreAllocationError {}

/// Pre-allocate every `Array<T>` and `Texture3D` resource the compiled
/// plan declares, then run the post-allocation audit. Both callers
/// (generator + effect chain) invoke this after `compile()` and before
/// the executor's first frame.
///
/// Three steps run in order:
///
/// 1. **Array<T> buffer pre-allocation.** Walks every step in topo
///    order, gathers each Array input's already-bound capacity (the
///    plan is sorted so producer outputs are bound first), then asks
///    each Array output's producer to size itself via
///    [`crate::node_graph::EffectNode::array_output_capacity`]. Honours
///    `aliased_array_io()` (stateful array sims share one buffer
///    between in and out ports) and `canvas_sized_array_outputs()`
///    (scatter accumulators sized to the backend's canvas dims).
///
/// 2. **Texture3D volume pre-allocation.** Mirror of step 1 for
///    Texture3D outputs. Uses
///    [`crate::node_graph::EffectNode::texture_3d_output_dims`] to
///    size volumes; format defaults to `Rgba16Float`, overridable per
///    output via the existing JSON `outputFormats` mechanism.
///
/// 3. **Post-allocation audit.** Walks every `Array<T>` resource on
///    the plan and asserts the backend has (a) a slot mapping and
///    (b) a real backing buffer. First failure returns
///    [`PreAllocationError::UnboundArrayResource`] naming the producer.
///    The architectural invariant: this function returns `Ok` only when
///    every resource is bound, or `Err` otherwise. No third state.
pub fn pre_allocate_resources(
    graph: &Graph,
    plan: &ExecutionPlan,
    device: &GpuDevice,
    backend: &mut MetalBackend,
) -> Result<(), PreAllocationError> {
    pre_allocate_array_buffers(graph, plan, device, backend)?;
    pre_allocate_texture_3d_volumes(graph, plan, device, backend)?;
    audit_array_resource_bindings(graph, plan, backend)?;
    Ok(())
}

fn pre_allocate_array_buffers(
    graph: &Graph,
    plan: &ExecutionPlan,
    device: &GpuDevice,
    backend: &mut MetalBackend,
) -> Result<(), PreAllocationError> {
    // Reverse handle map for error context. The audit / size-failure
    // paths name the producer's handle so the operator can find it in
    // the editor.
    let handle_by_node: AHashMap<NodeInstanceId, &'static str> =
        graph.handles().map(|(h, id)| (id, h)).collect();

    let mut input_capacities: Vec<(&str, u32)> = Vec::with_capacity(8);

    for step in plan.steps() {
        let Some(node_inst) = graph.get_node(step.node) else {
            continue;
        };
        let node_type = node_inst.node.type_id().as_str();

        input_capacities.clear();
        for (port_name, res_id) in &step.inputs {
            let Some(PortType::Array(layout)) = plan.resource_type(*res_id) else {
                continue;
            };
            let Some(slot) = backend.slot_for(*res_id) else {
                continue;
            };
            let Some(buf) = Backend::array_buffer(backend, slot) else {
                continue;
            };
            let count = (buf.size / layout.item_size as u64) as u32;
            input_capacities.push((*port_name, count));
        }

        let aliased_pairs = node_inst.node.aliased_array_io();
        let canvas_sized_outputs = node_inst.node.canvas_sized_array_outputs();
        let atomic_outputs = node_inst.node.atomic_outputs();
        let (canvas_w, canvas_h) = Backend::canvas_dims(backend as &dyn Backend);

        for (port_name, res_id) in &step.outputs {
            let Some(PortType::Array(layout)) = plan.resource_type(*res_id) else {
                continue;
            };

            // Atomic-accumulator outputs (e.g. node.draw_particles' u32 grid)
            // are read-modify-written: the downstream node.resolve_scatter
            // reads then zeros the buffer, so the buffer's contract is that it
            // STARTS at zero. Metal's create_buffer* does not zero-init, so a
            // fresh allocation would let frame 0 resolve the splat on top of
            // uninitialized VRAM — garbage that, in a feedback sim, amplifies
            // into run-to-run non-determinism (see the FluidSim2D
            // determinism guard in freeze::proof). Zero it once here so the
            // clear-after-read contract holds from the first frame.
            let needs_zero_init = atomic_outputs.contains(port_name);

            // Aliased in/out pairs (stateful array sims) — route the
            // output's resource id to the input's slot. No new
            // allocation; the simulator reads + writes the same
            // storage in place.
            let aliased_input_port = aliased_pairs
                .iter()
                .find(|(_, out_port)| *out_port == *port_name)
                .map(|(in_port, _)| *in_port);
            if let Some(in_port) = aliased_input_port {
                let in_res = step
                    .inputs
                    .iter()
                    .find(|(name, _)| *name == in_port)
                    .map(|(_, id)| *id);
                if let Some(in_res) = in_res
                    && let Some(in_slot) = backend.slot_for(in_res)
                {
                    backend.alias_array_resource(*res_id, in_slot);
                    continue;
                }
                log::warn!(
                    "[graph-loader] node `{node_type}` declared aliased pair \
                     `{in_port}` → `{port_name}` but `{in_port}` is not wired \
                     or has no pre-bound slot. Falling back to a fresh \
                     allocation; the simulator's in-place dispatch will \
                     write to a standalone buffer."
                );
            }

            // Canvas-sized output: scatter accumulators and similar
            // primitives whose Array output must align pixel-for-pixel
            // with the host canvas.
            if canvas_sized_outputs.contains(port_name) {
                if canvas_w == 0 || canvas_h == 0 {
                    log::warn!(
                        "[graph-loader] node `{node_type}` port `{port_name}` is \
                         canvas-sized but backend canvas dims are 0×0 (mock backend \
                         or unconfigured). Skipping allocation."
                    );
                    continue;
                }
                let capacity = (canvas_w as u64) * (canvas_h as u64);
                let bytes = capacity * layout.item_size as u64;
                let buffer = device.create_buffer_shared(bytes);
                if needs_zero_init {
                    buffer.zero_fill();
                }
                backend.pre_bind_array(*res_id, buffer);
                continue;
            }

            let Some(capacity) = node_inst.node.array_output_capacity(
                port_name,
                &node_inst.params,
                &input_capacities,
            ) else {
                return Err(PreAllocationError::UnsizedArrayOutput {
                    node_type: node_type.to_string(),
                    port: port_name.to_string(),
                    handle: handle_by_node.get(&step.node).map(|h| h.to_string()),
                });
            };
            let bytes = capacity as u64 * layout.item_size as u64;
            if bytes == 0 {
                log::warn!(
                    "[graph-loader] node `{node_type}` port `{port_name}` resolved \
                     to a zero-byte Array<T> buffer (capacity={capacity}, \
                     item_size={}). Skipping allocation.",
                    layout.item_size,
                );
                continue;
            }
            let buffer = device.create_buffer_shared(bytes);
            if needs_zero_init {
                buffer.zero_fill();
            }
            backend.pre_bind_array(*res_id, buffer);
        }
    }
    Ok(())
}

fn pre_allocate_texture_3d_volumes(
    graph: &Graph,
    plan: &ExecutionPlan,
    device: &GpuDevice,
    backend: &mut MetalBackend,
) -> Result<(), PreAllocationError> {
    let handle_by_node: AHashMap<NodeInstanceId, &'static str> =
        graph.handles().map(|(h, id)| (id, h)).collect();

    let mut input_dims: Vec<(&str, (u32, u32, u32))> = Vec::with_capacity(4);

    for step in plan.steps() {
        let Some(node_inst) = graph.get_node(step.node) else {
            continue;
        };
        let node_type = node_inst.node.type_id().as_str();

        input_dims.clear();
        for (port_name, res_id) in &step.inputs {
            if !matches!(plan.resource_type(*res_id), Some(PortType::Texture3D)) {
                continue;
            }
            let Some(slot) = backend.slot_for(*res_id) else {
                continue;
            };
            let Some(tex) = backend.texture_3d(slot) else {
                continue;
            };
            input_dims.push((*port_name, (tex.width, tex.height, tex.depth)));
        }

        for (port_name, res_id) in &step.outputs {
            if !matches!(plan.resource_type(*res_id), Some(PortType::Texture3D)) {
                continue;
            }
            // Already pre-bound (e.g. an alias pinned it earlier) — skip.
            if backend.slot_for(*res_id).is_some() {
                continue;
            }
            let Some((w, h, d)) = node_inst.node.texture_3d_output_dims(
                port_name,
                &node_inst.params,
                &input_dims,
            ) else {
                return Err(PreAllocationError::UnsizedTexture3DOutput {
                    node_type: node_type.to_string(),
                    port: port_name.to_string(),
                    handle: handle_by_node.get(&step.node).map(|h| h.to_string()),
                });
            };
            let format = node_inst
                .node
                .output_format(port_name)
                .unwrap_or(GpuTextureFormat::Rgba16Float);
            let label = format!("graph_loader 3d volume: {node_type}.{port_name}");
            let label_static: &'static str = Box::leak(label.into_boxed_str());
            let texture = device.create_texture(&GpuTextureDesc {
                width: w,
                height: h,
                depth: d,
                format,
                dimension: GpuTextureDimension::D3,
                usage: GpuTextureUsage::RENDER_TARGET_FULL,
                label: label_static,
                mip_levels: 1,
            });
            backend.pre_bind_texture_3d(*res_id, texture);
        }
    }
    Ok(())
}

fn audit_array_resource_bindings(
    graph: &Graph,
    plan: &ExecutionPlan,
    backend: &MetalBackend,
) -> Result<(), PreAllocationError> {
    let handle_by_node: AHashMap<NodeInstanceId, &'static str> =
        graph.handles().map(|(h, id)| (id, h)).collect();

    let total = plan.resource_count();
    for raw in 0..total {
        let res_id = ResourceId(raw as u32);
        // Only Array<T> resources are pre-bind-only; Texture2D /
        // Texture3D resources have either lazy-alloc pools or their own
        // pre-bind paths.
        let Some(PortType::Array(_)) = plan.resource_type(res_id) else {
            continue;
        };

        let has_slot = backend.slot_for(res_id).is_some();
        let has_buffer = backend
            .slot_for(res_id)
            .and_then(|s| backend.array_buffer(s))
            .is_some();
        if has_slot && has_buffer {
            continue;
        }

        let (producer_node_type, producer_port, producer_handle) = plan
            .steps()
            .iter()
            .find_map(|step| {
                step.outputs
                    .iter()
                    .find(|(_, id)| *id == res_id)
                    .map(|(port_name, _)| {
                        let node_type = graph
                            .get_node(step.node)
                            .map(|n| n.node.type_id().as_str().to_string())
                            .unwrap_or_else(|| "<unknown>".to_string());
                        let handle = handle_by_node
                            .get(&step.node)
                            .map(|h| h.to_string());
                        (node_type, port_name.to_string(), handle)
                    })
            })
            .unwrap_or_else(|| {
                (
                    "<no producer step>".to_string(),
                    "<unknown port>".to_string(),
                    None,
                )
            });

        return Err(PreAllocationError::UnboundArrayResource {
            producer_node_type,
            producer_port,
            producer_handle,
            cause: if !has_slot {
                "no slot mapping (allocation skipped — possibly canvas dims 0×0, \
                 zero-byte capacity, or a failed alias)"
            } else {
                "slot exists but has no buffer (alias chain broken or \
                 pre_bind_array not called)"
            },
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::boundary_nodes::{FinalOutput, Source};

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    /// Standalone instantiation: every boundary node lives in the graph;
    /// `final_output_id` is populated and `output_endpoint` is None.
    #[test]
    fn standalone_instantiates_every_boundary() {
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.uv_field", "handle": "uv" },
                { "id": 2, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");
        let mut graph = Graph::new();
        let inst = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
        )
        .expect("standalone instantiates cleanly");
        assert!(inst.generator_input_id.is_some());
        assert!(inst.final_output_id.is_some());
        assert!(inst.output_endpoint.is_none());
        assert!(inst.effect_local_handles.is_empty());
        assert_eq!(graph.node_count(), 3);
        assert_eq!(graph.wires().len(), 1);
        assert!(graph.node_id_by_handle("uv").is_some());
    }

    /// Splice instantiation: system.source + system.final_output are
    /// folded, output_endpoint captures the wire that fed final_output,
    /// handles return in effect_local_handles (NOT on graph.handles()).
    #[test]
    fn splice_folds_boundaries_and_returns_output_endpoint() {
        // A trivial 1-effect splice: source → threshold → final_output.
        // The chain graph's prev_node is the host source we connect to.
        let mut graph = Graph::new();
        let host_source = graph.add_node(Box::new(Source::new()));
        let host_final = graph.add_node(Box::new(FinalOutput::new()));

        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.source" },
                { "id": 1, "typeId": "node.threshold", "handle": "thresh" },
                { "id": 2, "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "source" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");

        let inst = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_source, "out"),
            },
        )
        .expect("splice instantiates cleanly");

        // The two boundary nodes were folded.
        assert!(inst.final_output_id.is_none());
        assert!(inst.generator_input_id.is_none());

        // The threshold is the output endpoint.
        let (endpoint_node, endpoint_port) = inst.output_endpoint.expect("splice has endpoint");
        assert_eq!(endpoint_port, "out");
        let thresh_id = inst
            .id_map
            .get(&1)
            .copied()
            .expect("threshold id mapped");
        assert_eq!(endpoint_node, thresh_id);

        // Handle was returned locally, NOT registered on the graph.
        assert_eq!(inst.effect_local_handles.len(), 1);
        assert_eq!(inst.effect_local_handles[0].0.as_ref(), "thresh");
        assert!(graph.node_id_by_handle("thresh").is_none());

        // Wire host_source.out → thresh.source connected.
        let wires = graph.wires();
        assert!(
            wires.iter().any(|w| w.from.0 == host_source && w.to.0 == thresh_id),
            "source re-anchor wire missing; got wires: {wires:?}"
        );

        // host_source, host_final, threshold = 3 nodes. The def's
        // Source/FinalOutput were folded, never instantiated.
        assert_eq!(graph.node_count(), 3);
        let _ = host_final;
    }

    /// Drift bug regression #1: output_formats audit now fires on the
    /// splice path. Pre-unification, splice silently dropped
    /// outputFormats overrides on primitives whose shader hardcoded the
    /// format. Now it errors.
    #[test]
    fn splice_audits_output_format_overrides() {
        let mut graph = Graph::new();
        let host_source = graph.add_node(Box::new(Source::new()));

        // Threshold's output format is hard-coded in its shader. An
        // outputFormats override against it must be rejected.
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.source" },
                {
                    "id": 1,
                    "typeId": "node.threshold",
                    "outputFormats": { "out": "rgba32float" }
                },
                { "id": 2, "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "source" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");

        let result = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_source, "out"),
            },
        );
        assert!(
            matches!(
                result,
                Err(GraphBuildError::OutputFormatNotSupported { .. })
            ),
            "splice should reject outputFormats against a hardcoded-format primitive; got {result:?}",
        );
    }

    /// Drift bug regression #2: param type validation now runs on the
    /// splice path. Pre-unification, splice silently coerced or
    /// dropped values; now it errors loudly.
    #[test]
    fn splice_rejects_param_type_mismatch() {
        let mut graph = Graph::new();
        let host_source = graph.add_node(Box::new(Source::new()));

        // Threshold.level is Float; this writes Bool.
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.source" },
                {
                    "id": 1,
                    "typeId": "node.threshold",
                    "params": { "level": { "type": "Bool", "value": true } }
                },
                { "id": 2, "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "source" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");

        let result = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_source, "out"),
            },
        );
        assert!(
            matches!(result, Err(GraphBuildError::ParamTypeMismatch { .. })),
            "splice should reject param type mismatch; got {result:?}",
        );
    }

    /// The unknown-type-id error names the offending type for the
    /// future editor surface.
    #[test]
    fn unknown_type_id_includes_context() {
        let mut graph = Graph::new();
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input" },
                { "id": 1, "typeId": "node.nonexistent" },
                { "id": 2, "typeId": "system.final_output" }
            ],
            "wires": []
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");
        let err = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
        )
        .unwrap_err();
        match err {
            GraphBuildError::UnknownTypeId { node_id, type_id } => {
                assert_eq!(node_id, 1);
                assert_eq!(type_id, "node.nonexistent");
            }
            other => panic!("expected UnknownTypeId; got {other:?}"),
        }
    }

    /// Sanity: a fixture without `system.source` is rejected at the
    /// splice boundary check, not later during wire translation.
    #[test]
    fn splice_rejects_missing_source_boundary() {
        let mut graph = Graph::new();
        let host_source = graph.add_node(Box::new(Source::new()));
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "node.threshold" },
                { "id": 1, "typeId": "system.final_output" }
            ],
            "wires": []
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");
        let err = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_source, "out"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, GraphBuildError::MissingBoundarySource));
    }

    // ───────────────────────────────────────────────────────────
    // pre_allocate_resources regressions
    // ───────────────────────────────────────────────────────────

    /// Post-allocation audit fires when an `Array<T>` resource has no
    /// bound slot. Ported from the previous
    /// `wire_audit_errors_when_array_resource_has_no_bound_buffer`
    /// test in `json_graph_generator.rs`. Now that audit lives in
    /// the shared pipeline, it MUST cover both the generator AND
    /// chain-graph callers — this regression pins the contract on
    /// the shared layer where regressions would surface for both.
    #[test]
    fn audit_fires_on_unbound_array_resource() {
        use crate::node_graph::primitives::{EulerStepParticles, GridUvField, SeedParticles};
        use crate::node_graph::{compile, MetalBackend};

        let device = std::sync::Arc::new(GpuDevice::new());
        let mut graph = Graph::new();
        let seed = graph.add_node(Box::new(SeedParticles::new()));
        let step = graph.add_node(Box::new(EulerStepParticles::new()));
        graph.connect((seed, "particles"), (step, "in")).unwrap();
        let forces = graph.add_node(Box::new(GridUvField::new()));
        graph.connect((forces, "uv"), (step, "forces")).unwrap();
        let plan = compile(&graph).expect("seed → step graph compiles");

        // Construct a backend but deliberately skip the Array<T>
        // pre-allocation; only run the audit directly so it has to
        // catch the dangling resources on its own.
        let backend = MetalBackend::new(std::sync::Arc::clone(&device), 256, 256, GpuTextureFormat::Rgba16Float);
        let err = audit_array_resource_bindings(&graph, &plan, &backend)
            .expect_err("audit must reject plan with unbound Array<T> resource");

        match err {
            PreAllocationError::UnboundArrayResource {
                producer_node_type,
                ..
            } => {
                assert!(
                    producer_node_type.contains("spawn_particles")
                        || producer_node_type.contains("move_particles")
                        || producer_node_type.contains("grid_uv_field"),
                    "error must name an Array<T> producer from the graph; got {producer_node_type}"
                );
            }
            other => panic!("expected UnboundArrayResource, got {other:?}"),
        }
    }

    /// Sanity: a fully-bound plan passes both pre-allocation steps
    /// and the post-allocation audit. Negative test above is paired
    /// with this so a regression that makes the audit always-error
    /// or always-pass fails CI loudly.
    #[test]
    fn pre_allocate_resources_accepts_fully_bound_plan() {
        use crate::node_graph::primitives::SeedParticles;
        use crate::node_graph::{compile, MetalBackend};

        let device = std::sync::Arc::new(GpuDevice::new());
        let mut graph = Graph::new();
        graph.add_node(Box::new(SeedParticles::new()));
        let plan = compile(&graph).expect("seed-only graph compiles");

        let mut backend = MetalBackend::new(std::sync::Arc::clone(&device), 256, 256, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&graph, &plan, &device, &mut backend)
            .expect("full pre-allocate pipeline succeeds for seed-only graph");
    }

    /// Drift bug regression: `pre_allocate_resources` runs on the
    /// effect chain side too. Before Step B the chain build's
    /// `pre_allocate_array_buffers_effect` shim had no audit and no
    /// Texture3D pass — features added on the generator side were
    /// silently absent from chains. This test pins that both callers
    /// invoke the same function (via the public re-export at
    /// `crate::node_graph::pre_allocate_resources`), so any future
    /// drift would have to introduce a new code path rather than
    /// silently lack one.
    #[test]
    fn shared_pre_allocate_is_the_single_callable() {
        // Module path identity check — if the function ever moves or
        // gets shadowed, this test fails to compile, surfacing the
        // change as a compile-time error rather than silent drift.
        let _: fn(&Graph, &ExecutionPlan, &GpuDevice, &mut MetalBackend) -> Result<(), PreAllocationError> =
            pre_allocate_resources;
        let _: fn(&Graph, &ExecutionPlan, &GpuDevice, &mut MetalBackend) -> Result<(), PreAllocationError> =
            crate::node_graph::pre_allocate_resources;
    }

    // ── docs/NODE_VOCABULARY_AUDIT.md §3 test (a) ──
    //
    // `type_id_migration::TYPE_ID_MIGRATIONS` is empty in every shipped
    // build except one fixture entry
    // (`__vocab_migration_test_old__` → `__vocab_migration_test_new__`),
    // kept unconditionally (not `#[cfg(test)]`) so it's visible from every
    // crate's tests, not just this one — see the module doc on
    // `manifold_core::type_id_migration` for why.

    /// Builder mirroring `manifold_core::flatten`'s test helpers: a bare
    /// node with no params/wires/position, for readable fixtures.
    fn bare_node(id: u32, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: manifold_core::NodeId::default(),
            type_id: type_id.to_string(),
            handle: None,
            params: Default::default(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        }
    }

    /// A `group`-type node whose single body node carries `inner_type_id`.
    fn grouped_node(id: u32, inner_type_id: &str) -> EffectGraphNode {
        use manifold_core::effect_graph_def::{GROUP_TYPE_ID, GroupDef, GroupInterface};
        let mut g = bare_node(id, GROUP_TYPE_ID);
        g.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: vec![],
                outputs: vec![],
                params: vec![],
            },
            nodes: vec![bare_node(100, inner_type_id)],
            wires: vec![],
            tint: None,
        }));
        g
    }

    /// (a) A fixture graph written with old ids — one at top level, one
    /// buried inside a group body — loads (via `migrate_def_type_ids`, the
    /// function `instantiate_def` runs before the group flatten) structurally
    /// identical to its hand-authored new-id twin. Proves both the ordering
    /// (migrate before flatten) and the recursion into group bodies; a
    /// migration applied only at top level, or only after flatten, would
    /// leave the inner node's id stale and fail the `assert_eq!`.
    #[test]
    fn migrate_def_type_ids_matches_new_id_twin_including_group_bodies() {
        let registry = PrimitiveRegistry::with_builtin();
        let old_def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![
                bare_node(0, "__vocab_migration_test_old__"),
                grouped_node(1, "__vocab_migration_test_old__"),
            ],
            wires: vec![],
        };
        let new_twin = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![
                bare_node(0, "__vocab_migration_test_new__"),
                grouped_node(1, "__vocab_migration_test_new__"),
            ],
            wires: vec![],
        };

        let migrated =
            migrate_def_type_ids(&old_def, &registry).expect("old ids present, migration must produce Some");
        assert_eq!(migrated, new_twin);

        // A document already on current ids needs no migration at all —
        // the cheap-clone-free common-case path `instantiate_def` relies on.
        assert!(migrate_def_type_ids(&new_twin, &registry).is_none());
    }

    /// docs/NODE_VOCABULARY_AUDIT.md §7.1: the retired `node.rotate_vec2_90`
    /// folds into `node.rotate_vector` AND seeds `angle = PI/2` (radians —
    /// the stored representation, not the UI's degrees) via
    /// `PARAM_SEED_MIGRATIONS`, reproducing the retired node's fixed +90°
    /// rotation. A node buried in a group body gets seeded exactly like a
    /// top-level one (same recursion the plain id-rewrite uses).
    #[test]
    fn migrate_def_type_ids_seeds_params_for_legacy_fold() {
        let registry = PrimitiveRegistry::with_builtin();
        let old_def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![
                bare_node(0, "node.rotate_vec2_90"),
                grouped_node(1, "node.rotate_vec2_90"),
            ],
            wires: vec![],
        };

        let migrated = migrate_def_type_ids(&old_def, &registry)
            .expect("old id present, migration must produce Some");

        let top = &migrated.nodes[0];
        assert_eq!(top.type_id, "node.rotate_vector");
        assert_eq!(
            top.params.get("angle"),
            Some(&manifold_core::effect_graph_def::SerializedParamValue::Float {
                value: std::f32::consts::FRAC_PI_2
            })
        );

        let inner = &migrated.nodes[1].group.as_ref().unwrap().nodes[0];
        assert_eq!(inner.type_id, "node.rotate_vector");
        assert_eq!(
            inner.params.get("angle"),
            Some(&manifold_core::effect_graph_def::SerializedParamValue::Float {
                value: std::f32::consts::FRAC_PI_2
            })
        );
    }

    /// Seeding never overwrites a param key the document already carries —
    /// "seed the default", not "force the value" (see the doc comment on
    /// `migrate_def_type_ids`).
    #[test]
    fn migrate_def_type_ids_seed_does_not_overwrite_existing_param() {
        let registry = PrimitiveRegistry::with_builtin();
        let mut node = bare_node(0, "node.rotate_vec2_90");
        node.params.insert(
            "angle".to_string(),
            manifold_core::effect_graph_def::SerializedParamValue::Float { value: 1.0 },
        );
        let old_def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![node],
            wires: vec![],
        };

        let migrated = migrate_def_type_ids(&old_def, &registry)
            .expect("old id present, migration must produce Some");
        assert_eq!(
            migrated.nodes[0].params.get("angle"),
            Some(&manifold_core::effect_graph_def::SerializedParamValue::Float { value: 1.0 }),
            "explicit stored value must survive the fold unseeded"
        );
    }

    /// docs/NODE_VOCABULARY_AUDIT.md §7.2: the retired
    /// `node.fluid_project_scatter_2d` is a plain rename fold (port-identical
    /// to `node.draw_particles_camera`, no param seed) — proves the rename
    /// side of the same choke point still works with zero
    /// `PARAM_SEED_MIGRATIONS` entries matching.
    #[test]
    fn migrate_def_type_ids_plain_rename_seeds_no_params() {
        let registry = PrimitiveRegistry::with_builtin();
        let old_def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![bare_node(0, "node.fluid_project_scatter_2d")],
            wires: vec![],
        };
        let migrated = migrate_def_type_ids(&old_def, &registry)
            .expect("old id present, migration must produce Some");
        assert_eq!(migrated.nodes[0].type_id, "node.draw_particles_camera");
        assert!(migrated.nodes[0].params.is_empty());
    }

    // ── GLTF_ANIM_RUNTIME_V2_DESIGN.md P2/D5 — old-shape sampler migration ──

    fn minimal_preset_metadata() -> manifold_core::effect_graph_def::PresetMetadata {
        manifold_core::effect_graph_def::PresetMetadata {
            id: manifold_core::PresetTypeId::new("test.migration_fixture"),
            display_name: "Migration Fixture".to_string(),
            category: "Spatial".to_string(),
            osc_prefix: "migration_fixture".to_string(),
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
        }
    }

    fn old_table(rows: usize) -> manifold_core::effect_graph_def::SerializedParamValue {
        manifold_core::effect_graph_def::SerializedParamValue::Table {
            rows: (0..rows).map(|_| vec![0.0]).collect(),
        }
    }

    /// D5's core round trip: a pre-P2 rigid `node.gltf_animation_source`
    /// (old `translation_track`/`rotation_track` Tables, no `path`) sits
    /// alongside its sibling `node.gltf_mesh_source` INSIDE one group —
    /// the importer's own group-per-object shape. Migration must strip the
    /// old Tables, stamp a NEW `path` stringBinding pointing at the
    /// sibling's own resolved path, and stay idempotent on a second pass.
    #[test]
    fn migrate_gltf_anim_v2_strips_tables_and_stamps_sibling_path() {
        let mut mesh = bare_node(100, "node.gltf_mesh_source");
        mesh.node_id = manifold_core::NodeId::new("mesh_0");
        let mut anim = bare_node(101, "node.gltf_animation_source");
        anim.node_id = manifold_core::NodeId::new("anim_0");
        anim.params.insert("translation_track".to_string(), old_table(2));
        anim.params.insert("rotation_track".to_string(), old_table(2));

        let mut group = bare_node(0, manifold_core::effect_graph_def::GROUP_TYPE_ID);
        group.group = Some(Box::new(manifold_core::effect_graph_def::GroupDef {
            interface: manifold_core::effect_graph_def::GroupInterface {
                inputs: vec![],
                outputs: vec![],
                params: vec![],
            },
            nodes: vec![mesh, anim],
            wires: vec![],
            tint: None,
        }));

        let mut meta = minimal_preset_metadata();
        meta.string_bindings.push(manifold_core::effect_graph_def::StringBindingDef {
            id: "modelFile".to_string(),
            label: "Model File".to_string(),
            default_value: "/fixtures/old_shape.glb".to_string(),
            target: manifold_core::effect_graph_def::BindingTarget::Node {
                node_id: manifold_core::NodeId::new("mesh_0"),
                param: "path".to_string(),
            },
        });

        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: Some(meta),
            nodes: vec![group],
            wires: vec![],
        };

        assert!(migrate_gltf_anim_v2(&mut def), "old-shape tables present -> migration must report changed");

        let inner = &def.nodes[0].group.as_ref().unwrap().nodes[1];
        assert_eq!(inner.type_id, "node.gltf_animation_source");
        assert!(!inner.params.contains_key("translation_track"), "old table must be stripped");
        assert!(!inner.params.contains_key("rotation_track"), "old table must be stripped");

        let anim_path_binding = def
            .preset_metadata
            .as_ref()
            .unwrap()
            .string_bindings
            .iter()
            .find(|b| matches!(&b.target, manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } if node_id.as_str() == "anim_0" && param == "path"))
            .expect("a NEW path stringBinding must be stamped for the sampler node");
        assert_eq!(anim_path_binding.default_value, "/fixtures/old_shape.glb");

        // Idempotence: a second pass over the already-migrated def is a no-op.
        assert!(!migrate_gltf_anim_v2(&mut def), "already-migrated def must report unchanged");
    }

    /// D5's inert-but-present branch: no sibling mesh-source path resolves
    /// (no `node.gltf_mesh_source`/`node.gltf_skinned_mesh_source` in
    /// scope) — the old Tables must survive UNTOUCHED, never a silent
    /// drop, and the function still reports `changed = false` (nothing was
    /// actually mutated).
    #[test]
    fn migrate_gltf_anim_v2_leaves_unresolvable_node_inert() {
        let mut anim = bare_node(0, "node.gltf_animation_source");
        anim.node_id = manifold_core::NodeId::new("anim_orphan");
        anim.params.insert("translation_track".to_string(), old_table(1));

        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: Some(minimal_preset_metadata()),
            nodes: vec![anim],
            wires: vec![],
        };

        assert!(!migrate_gltf_anim_v2(&mut def), "no sibling path resolvable -> nothing changed");
        assert!(
            def.nodes[0].params.contains_key("translation_track"),
            "unresolvable node's tables must stay inert, never dropped"
        );
    }

    /// (a, continued) The same fixture through the real `instantiate_def`
    /// entry point, boundary nodes only (the sentinel isn't a registered
    /// primitive, so it can't sit mid-graph and still construct) — proves
    /// the choke point is actually wired into the function tests above call
    /// directly, not just defined alongside it.
    #[test]
    fn instantiate_def_migrates_boundary_free_standing_old_id_graph() {
        // `system.*` ids are exempt from migration (§2 rule 7) and are the
        // only ids `instantiate_def` can build without a real registry
        // entry, so this proves migration runs inside `instantiate_def`
        // without disturbing boundary handling — the fixture above proves
        // the id-rewrite itself.
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.source", "handle": "source" },
                { "id": 1, "typeId": "system.final_output", "handle": "final" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");
        let mut graph = Graph::new();
        instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
        )
        .expect("system.* boundary ids are unaffected by migration and always construct");
    }

    /// `docs/CINEMATIC_POST_DESIGN.md` P5/I8: a saved graph carrying the
    /// retired `node.ssao_from_depth` type id loads through the real
    /// `instantiate_def` entry point, resolves to `node.ssao_gtao`, and its
    /// `radius`/`intensity` params carry over untouched — the round-trip
    /// gate (DESIGN_DOC_STANDARD §5) for the D9(b) seam. The old fixture
    /// also carries `bias` (every project saved before this rename has it),
    /// proving `migrate_def_type_ids`'s new "drop params the successor
    /// doesn't declare" step — without it this load would hard-fail with
    /// `GraphBuildError::UnknownParam` on every pre-rename project.
    #[test]
    fn ssao_from_depth_migrates_to_gtao() {
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                {
                    "id": 1,
                    "typeId": "node.ssao_from_depth",
                    "handle": "ssao",
                    "params": {
                        "radius": { "type": "Float", "value": 0.75 },
                        "intensity": { "type": "Float", "value": 1.25 },
                        "bias": { "type": "Float", "value": 0.025 }
                    }
                },
                { "id": 2, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");
        let mut graph = Graph::new();
        instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
        )
        .expect("old id + stale `bias` param must still load after the D9(b) rename");

        let ssao_id = graph.node_id_by_handle("ssao").expect("ssao handle present");
        let inst = graph.get_node(ssao_id).expect("node exists");
        assert_eq!(inst.node.type_id().as_str(), "node.ssao_gtao", "resolves to the new atom");

        assert!(
            inst.node.parameters().iter().any(|p| p.name == "radius"),
            "radius param declared"
        );
        assert_eq!(
            inst.params.get("radius"),
            Some(&ParamValue::Float(0.75)),
            "radius carries the OLD document's stored value, not the descriptor default"
        );
        assert_eq!(
            inst.params.get("intensity"),
            Some(&ParamValue::Float(1.25)),
            "intensity carries over unchanged"
        );
        assert!(
            !inst.node.parameters().iter().any(|p| p.name == "bias"),
            "node.ssao_gtao declares no `bias` param (D9(b))"
        );
        assert!(
            !inst.params.contains_key("bias"),
            "the stale `bias` value from the old document must be dropped, not just \
             unreferenced by the descriptor — see migrate_def_type_ids's params.retain"
        );
    }
}
