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

use ahash::{AHashMap, AHashSet};

use manifold_core::effect_graph_def::{
    EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    GROUP_INPUT_TYPE_ID,
};
use manifold_gpu::{
    GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

use crate::exec::backend::Backend;
use crate::scene::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, GENERATOR_INPUT_TYPE_ID, SOURCE_TYPE_ID};
use crate::exec::effect_node::{NodeInstanceId, ParamValues};
use crate::exec::execution_plan::{ExecutionPlan, ResourceId};
use crate::graph::Graph;
use crate::exec::metal_backend::MetalBackend;
use crate::parameters::{ParamType, ParamValue};
use crate::ports::PortType;
use crate::persistence::{PrimitiveRegistry, format_from_str};

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
    SceneModifier(crate::load::expand::SceneModifierExpandError),
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
    InvalidMaterialFeatureMode {
        node_id: u32,
        param: String,
        value: u32,
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
    /// A prepared mesh-rule sidecar entry (design
    /// `docs/SCENE_MODIFIER_RT_DESIGN.md` §3.3) could not be installed:
    /// the stable node id matched no document node (`reason` says
    /// "unknown"), matched more than one ("duplicate"), or the live node
    /// refused the rules ("uninstalled", carrying the node's message).
    /// Preparation errors fail the load — never a silent canonical
    /// fallback.
    MeshRules {
        node_id: manifold_core::NodeId,
        reason: String,
    },
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
                // per the round-trip gate (DESIGN_DOC_STANDARD section 5), a
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

/// Whether any node, at any group depth or in any scene modifier, has a type
/// listed in [`manifold_core::type_id_migration::RETIRED_PARAMS`]. Lets the
/// common load borrow the document instead of cloning it.
pub fn has_retired_params(def: &EffectGraphDef) -> bool {
    fn any(nodes: &[EffectGraphNode]) -> bool {
        nodes.iter().any(|node| manifold_core::type_id_migration::retires_params(&node.type_id)
            || node.group.as_ref().is_some_and(|group| any(&group.nodes)))
    }
    any(&def.nodes) || def.scene_modifiers.iter().any(|modifier| has_retired_params(&modifier.graph))
}

/// Strip every [`manifold_core::type_id_migration::RETIRED_PARAMS`] entry
/// from a saved graph: the value, exposure, wires into the port, group
/// interface params and inputs that only fed it, and cards that only drove
/// it. Runs before group flattening and before preset metadata is captured.
pub(crate) fn retire_params(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::BindingTarget;

    fn declared_ids(nodes: &[EffectGraphNode], ids: &mut AHashSet<String>) {
        for node in nodes {
            if !node.node_id.is_empty() { ids.insert(node.node_id.to_string()); }
            if let Some(group) = &node.group { declared_ids(&group.nodes, ids); }
        }
    }

    // Local numeric endpoints drive wires; scoped handles drive group aliases;
    // stable identities drive authored cards. Keep these address spaces separate.
    fn scope(
        nodes: &mut [EffectGraphNode], wires: &mut Vec<EffectGraphWire>,
        prefix: &str, declared: &AHashSet<String>, identities: &mut AHashSet<(String, String)>,
    ) -> (AHashSet<(String, String)>, AHashSet<String>, bool) {
        let mut handles = AHashSet::new();
        let mut endpoints = AHashSet::new();
        let mut changed = false;
        for node in nodes.iter_mut() {
            let handle = node.handle.as_deref().unwrap_or_default();
            let full_handle = format!("{prefix}{handle}");
            let mut retired: AHashSet<String> = manifold_core::type_id_migration::retired_params(&node.type_id)
                .map(str::to_string).collect();
            if let Some(group) = &mut node.group {
                let (inner, inputs, inner_changed) = scope(
                    &mut group.nodes, &mut group.wires, &format!("{full_handle}/"), declared, identities,
                );
                changed |= inner_changed;
                group.interface.params.retain(|param| {
                    let dead = inner.contains(&(param.target_handle.clone(), param.target_param.clone()));
                    if dead { retired.insert(param.name.clone()); changed = true; }
                    !dead
                });
                group.interface.inputs.retain(|port| {
                    if inputs.contains(&port.name) { retired.insert(port.name.clone()); changed = true; false } else { true }
                });
                for (inner_handle, param) in inner {
                    handles.insert((format!("{handle}/{inner_handle}"), param));
                }
            }
            for param in retired {
                changed |= node.params.remove(&param).is_some();
                changed |= node.exposed_params.remove(&param);
                endpoints.insert((node.id, param.clone()));
                if !handle.is_empty() {
                    handles.insert((handle.to_string(), param.clone()));
                    // A legacy handleNode has this identity, unless an explicit
                    // stable identity belongs to another node in the same graph.
                    if !declared.contains(&full_handle) || node.node_id.as_str() == full_handle {
                        identities.insert((full_handle.clone(), param.clone()));
                    }
                }
                if !node.node_id.as_str().is_empty() {
                    identities.insert((node.node_id.to_string(), param));
                }
            }
        }
        let boundary_inputs: AHashSet<_> = nodes.iter().filter(|node| node.type_id == GROUP_INPUT_TYPE_ID)
            .map(|node| node.id).collect();
        let mut inputs = AHashSet::new();
        wires.retain(|wire| {
            let dead = endpoints.contains(&(wire.to_node, wire.to_port.clone()));
            if dead && boundary_inputs.contains(&wire.from_node) { inputs.insert(wire.from_port.clone()); }
            changed |= dead;
            !dead
        });
        // An interface input can fan out to a live target as well.
        inputs.retain(|port| !wires.iter().any(|wire|
            boundary_inputs.contains(&wire.from_node) && &wire.from_port == port));
        changed |= !inputs.is_empty();
        (handles, inputs, changed)
    }

    let mut declared = AHashSet::new();
    declared_ids(&def.nodes, &mut declared);
    let mut identities = AHashSet::new();
    let (_, _, mut changed) = scope(&mut def.nodes, &mut def.wires, "", &declared, &mut identities);
    if let Some(metadata) = &mut def.preset_metadata {
        let mut retired_cards = AHashSet::new();
        metadata.bindings.retain(|binding| {
            let dead = matches!(&binding.target, BindingTarget::Node { node_id, param }
                if identities.contains(&(node_id.to_string(), param.clone())));
            if dead { retired_cards.insert(binding.id.clone()); }
            !dead
        });
        changed |= !retired_cards.is_empty();
        // Preserve a card with other live targets, including mixed fan-out.
        retired_cards.retain(|id| !metadata.bindings.iter().any(|binding| &binding.id == id));
        for id in &retired_cards {
            log::warn!("retired param: card '{id}' removed, its only target no longer exists");
        }
        metadata.params.retain(|param| !retired_cards.contains(&param.id));
        metadata.param_aliases.retain(|alias| !retired_cards.contains(&alias.old)
            && alias.new.as_ref().is_none_or(|id| !retired_cards.contains(id)));
        metadata.value_aliases.retain(|alias| !retired_cards.contains(&alias.param_id));
    }
    for modifier in &mut def.scene_modifiers {
        changed |= retire_params(&mut modifier.graph);
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
    mesh_rules: &crate::scene::mesh_change::PreparedMeshRules,
) -> Result<NodeInstantiation, GraphBuildError> {
    if def.version == 0 || def.version > EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS {
        return Err(GraphBuildError::UnsupportedVersion {
            found: def.version,
            max: EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS,
        });
    }

    // Standalone preset definitions can reach the renderer without passing
    // through the project loader (catalog imports, previews, and editor
    // snapshots). Apply the one-shot Phong migration at this shared choke
    // point, while the common current-document path stays borrowed.
    let phong_migrated;
    let def = if manifold_core::phong_migration::contains_phong_materials(def) {
        let mut owned = def.clone();
        manifold_core::phong_migration::migrate_phong_to_pbr(&mut owned);
        phong_migrated = owned;
        &phong_migrated
    } else {
        def
    };

    let retired;
    let def = if has_retired_params(def) {
        let mut owned = def.clone();
        retire_params(&mut owned);
        retired = owned;
        &retired
    } else {
        def
    };

    // All raw host loads use the same structural preparation before any
    // primitive is installed. A standalone recipe still requires attachment.
    let modifier_owner = def;
    let prepared = if manifold_core::scene_modifier_preset::has_scene_modifier_data(def) {
        if !matches!(boundary, BoundaryHandling::Standalone) {
            return Err(GraphBuildError::SceneModifier(crate::load::expand::SceneModifierExpandError::InvalidRecipe {
                path: "sceneModifiers".into(), detail: "scene modifiers require a standalone generator owner".into(),
            }));
        }
        Some(crate::load::expand::prepare_scene_modifiers(def, registry)
            .map_err(GraphBuildError::SceneModifier)?)
    } else { None };
    let def = prepared.as_ref().map_or(def, |prepared| &prepared.def);

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

    let before_flatten = super::migration::run_stage(def, super::migration::MigrationStage::BeforeFlatten);
    let def = before_flatten.as_ref();

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

    let after_flatten = super::migration::run_stage(def, super::migration::MigrationStage::AfterFlatten);
    let def = after_flatten.as_ref();

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
            .map(|p| (crate::exec::effect_node::intern_name(&p.name), p.ty))
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
            if node_doc.type_id == "node.pbr_material"
                && is_material_feature_mode(key)
                && matches!(&pv, ParamValue::Enum(value) if *value > 3)
            {
                let ParamValue::Enum(value) = pv else { unreachable!() };
                return Err(GraphBuildError::InvalidMaterialFeatureMode {
                    node_id: node_doc.id,
                    param: key.clone(),
                    value,
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

    // Coupled physics is graph-owned runtime metadata. Resolve stable scene
    // identities against this exact def and this instantiation's id_map;
    // the graph-wide stable-id lookup is not valid for effect splices.
    let has_fluid = def
        .nodes
        .iter()
        .any(|node| manifold_core::liquid_domain::is_liquid_domain(&node.type_id));
    let has_rigid = def.nodes.iter().any(|node| node.type_id == "node.physics_world");
    if has_fluid && has_rigid {
        let bindings = crate::load::expand::prepare_coupled_scenes(def, registry)
            .map_err(GraphBuildError::SceneModifier)?;
        for binding in bindings {
            let fluid_doc = def
                .nodes
                .iter()
                .find(|node| node.node_id == binding.fluid)
                .ok_or_else(|| GraphBuildError::SceneModifier(
                    crate::load::expand::SceneModifierExpandError::MissingTarget {
                        path: binding.fluid.to_string(),
                        detail: "prepared coupled fluid node is missing from the instantiated def".into(),
                    },
                ))?;
            let rigid_doc = def
                .nodes
                .iter()
                .find(|node| node.node_id == binding.rigid)
                .ok_or_else(|| GraphBuildError::SceneModifier(
                    crate::load::expand::SceneModifierExpandError::MissingTarget {
                        path: binding.rigid.to_string(),
                        detail: "prepared coupled rigid node is missing from the instantiated def".into(),
                    },
                ))?;
            let fluid = *id_map.get(&fluid_doc.id).ok_or_else(|| {
                GraphBuildError::SceneModifier(
                    crate::load::expand::SceneModifierExpandError::MissingTarget {
                        path: binding.fluid.to_string(),
                        detail: "prepared coupled fluid node has no runtime mapping".into(),
                    },
                )
            })?;
            let rigid = *id_map.get(&rigid_doc.id).ok_or_else(|| {
                GraphBuildError::SceneModifier(
                    crate::load::expand::SceneModifierExpandError::MissingTarget {
                        path: binding.rigid.to_string(),
                        detail: "prepared coupled rigid node has no runtime mapping".into(),
                    },
                )
            })?;
            graph
                .add_coupled_scene(fluid, rigid, binding.colliders)
                .map_err(|error| GraphBuildError::InvalidWire {
                    wire_index: usize::MAX,
                    reason: format!("failed to register coupled scene: {error}"),
                })?;
        }
    }

    // Prepared mesh-rule sidecar (design §3.3): after every node exists
    // and its params/sources are installed, forward the compiler-provided
    // rules to the live nodes. Failures are preparation errors, not a
    // silent revert to rebuild-everything defaults. Runs before the
    // prepared-budget install below, which only annotates the graph and
    // adds no nodes.
    install_prepared_mesh_rules(graph, def, &id_map, mesh_rules)?;

    if let Some(prepared) = prepared {
        let budget = crate::load::expand::PreparedModifierBufferBudget::prepare(
            modifier_owner, &prepared.routes, graph, &AHashMap::default(),
        ).map_err(GraphBuildError::SceneModifier)?;
        graph.set_modifier_buffer_budget(budget);
        crate::load::expand::PreparedModifierParameterGuards::prepare(modifier_owner)
            .and_then(|guards| guards.install(graph)).map_err(GraphBuildError::SceneModifier)?;
    }

    Ok(NodeInstantiation {
        id_map,
        effect_local_handles,
        output_endpoint,
        generator_input_id,
        final_output_id,
    })
}

/// Install a prepared mesh-rule sidecar (design
/// `docs/SCENE_MODIFIER_RT_DESIGN.md` §3.3) onto the live nodes of a
/// graph built from `def`. Each sidecar entry is keyed by the stable
/// document [`manifold_core::NodeId`]; resolution uses the same
/// `node_id`-then-handle rule as the instantiation pass above, and the
/// numeric document id reaches the live node through `id_map`. Handle
/// names are never used as stable node ids. Unknown (no document node
/// resolves to the key), duplicate (more than one does), and uninstalled
/// (the node refuses) entries are preparation errors.
pub(crate) fn install_prepared_mesh_rules(
    graph: &mut Graph,
    def: &EffectGraphDef,
    id_map: &AHashMap<u32, NodeInstanceId>,
    rules: &crate::scene::mesh_change::PreparedMeshRules,
) -> Result<(), GraphBuildError> {
    if rules.is_empty() {
        return Ok(());
    }
    let mut claimed: AHashSet<&manifold_core::NodeId> = AHashSet::default();
    for node_doc in &def.nodes {
        let resolved_node_id = if node_doc.node_id.is_empty() {
            node_doc
                .handle
                .as_deref()
                .map(manifold_core::NodeId::new)
                .unwrap_or_default()
        } else {
            node_doc.node_id.clone()
        };
        let Some((key, rule_set)) = rules.get_key_value(&resolved_node_id) else {
            continue;
        };
        if !claimed.insert(key) {
            return Err(GraphBuildError::MeshRules {
                node_id: resolved_node_id.clone(),
                reason: "duplicate: more than one document node resolves to this node id"
                    .to_string(),
            });
        }
        let Some(&runtime_id) = id_map.get(&node_doc.id) else {
            return Err(GraphBuildError::MeshRules {
                node_id: resolved_node_id.clone(),
                reason: format!(
                    "document node {} was not instantiated (boundary-folded or skipped)",
                    node_doc.id
                ),
            });
        };
        let Some(inst) = graph.get_node_mut(runtime_id) else {
            return Err(GraphBuildError::MeshRules {
                node_id: resolved_node_id.clone(),
                reason: "instantiated node missing from graph".to_string(),
            });
        };
        inst.node.install_mesh_output_rules(rule_set).map_err(|e| {
            GraphBuildError::MeshRules {
                node_id: resolved_node_id.clone(),
                reason: format!("uninstalled: {e}"),
            }
        })?;
    }
    for node_id in rules.keys() {
        if !claimed.contains(node_id) {
            return Err(GraphBuildError::MeshRules {
                node_id: node_id.clone(),
                reason: "unknown: no document node resolves to this node id".to_string(),
            });
        }
    }
    Ok(())
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

fn is_material_feature_mode(param: &str) -> bool {
    matches!(
        param,
        "coat_mode"
            | "iridescence_mode"
            | "emission_mode"
            | "glass_mode"
            | "sheen_mode"
            | "anisotropy_mode"
            | "translucency_mode"
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
        GraphBuildError::InvalidMaterialFeatureMode {
            node_id,
            param,
            value,
        } => {
            let _ = write!(
                buf,
                "node {node_id}: material feature mode '{param}' has invalid value {value}"
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
        GraphBuildError::SceneModifier(error) => {
            let _ = write!(buf, "scene modifier preparation failed: {error}");
        }
        GraphBuildError::Flatten(e) => {
            let _ = write!(buf, "group flatten failed: {e}");
        }
        GraphBuildError::MeshRules { node_id, reason } => {
            let _ = write!(buf, "mesh rules for node {}: {reason}", node_id.as_str());
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
        .map(|p| crate::exec::effect_node::intern_name(&p.name))
}

fn resolve_output_port(graph: &Graph, node: NodeInstanceId, name: &str) -> Option<&'static str> {
    graph
        .get_node(node)?
        .node
        .outputs()
        .iter()
        .find(|p| p.name == name)
        .map(|p| crate::exec::effect_node::intern_name(&p.name))
}

// ---------------------------------------------------------------------------
// Post-compile resource pre-allocation (Array<T> + Texture3D + audit)
// ---------------------------------------------------------------------------

/// Errors produced by [`pre_allocate_resources`]. The variants carry
/// the offending node's `type_id`, port name, and handle (when present)
/// so callers can surface them with full context to the operator.
#[derive(Debug, Clone)]
pub enum PreAllocationError {
    ModifierAdmission(crate::load::expand::SceneModifierExpandError),
    /// The graph has modifier-owned allocations but the active GPU backend
    /// cannot provide a memory snapshot for admission.
    ModifierMemoryUnavailable,
    /// A staged candidate could not obtain one of its native resources.
    AllocationFailed(String),
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
            Self::ModifierAdmission(error) => error.fmt(f),
            Self::ModifierMemoryUnavailable => write!(
                f,
                "scene modifier memory admission unavailable: the GPU did not expose current allocated size and working-set capacity"
            ),
            Self::AllocationFailed(error) => write!(f, "GPU resource allocation failed: {error}"),
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
    graph: &mut Graph,
    plan: &ExecutionPlan,
    device: &GpuDevice,
    backend: &mut MetalBackend,
) -> Result<(), PreAllocationError> {
    // Every install path comes through here, so nodes that build pipelines
    // ahead of run() are ready before the first frame on all of them.
    for node in graph.nodes_mut() {
        node.node.prepare_pipelines(device);
    }
    allocate_resources(graph, plan, device, backend)
}

/// Allocation only, for a graph that is already installed (its nodes'
/// pipelines prepared by [`pre_allocate_resources`]). Resize is the caller:
/// it reallocates against a candidate backend from a shared borrow.
pub fn allocate_resources(
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
    use crate::exec::resource_allocation::{ArrayAllocationAction, ArrayStorage, plan_array_allocations};

    // Snapshot existing physical storage once. Several resources may already
    // share a backend slot; preserve that identity in the pure allocation plan.
    let mut prebound = AHashMap::default();
    let mut prebound_buffers = AHashMap::default();
    let mut roots = AHashMap::default();
    for raw in 0..plan.resource_count() {
        let resource = ResourceId(raw as u32);
        if !matches!(plan.resource_type(resource), Some(PortType::Array(_))) {
            continue;
        }
        let Some(slot) = backend.slot_for(resource) else { continue; };
        let Some(buffer) = Backend::array_buffer(backend, slot) else { continue; };
        let root = *roots.entry(slot).or_insert(resource);
        prebound.insert(resource, ArrayStorage { root, bytes: buffer.size });
        prebound_buffers.entry(root).or_insert_with(|| buffer.clone());
    }
    let allocation = plan_array_allocations(
        graph, plan, Backend::canvas_dims(backend), &prebound,
    )?;
    if let Some(budget) = graph.modifier_buffer_budget() {
        // Capture immediately before allocation. `currentAllocatedSize`
        // includes the live scene being replaced, so the projected peak must
        // conservatively cover old and candidate resources overlapping.
        let snapshot = device
            .modifier_memory_snapshot()
            .ok_or(PreAllocationError::ModifierMemoryUnavailable)?;
        let usage = budget
            .check_with_snapshot(&allocation, Some(snapshot))
            .map_err(PreAllocationError::ModifierAdmission)?;
        log::debug!("prepared modifier buffer usage: {usage:?}");
    }
    for warning in &allocation.warnings {
        log::warn!("[graph-loader] {warning}");
    }
    // Sizing, capacity propagation and alias decisions have one authority.
    // Admission inspects these same actions before any buffer is allocated:
    // the whole scene first, then each buffer against what is live by then.
    let scene_bytes = allocation
        .actions
        .iter()
        .map(|action| match action {
            ArrayAllocationAction::Allocate(allocation) => allocation.bytes,
            _ => 0,
        })
        .fold(0u64, u64::saturating_add);
    crate::load::expand::admit_scene_bytes(device.modifier_memory_snapshot(), scene_bytes)
        .map_err(PreAllocationError::AllocationFailed)?;
    for action in allocation.actions {
        match action {
            ArrayAllocationAction::Allocate(allocation) => {
                // Include live resources and earlier staged allocations before
                // asking Metal for each new buffer, including non-modifier graphs.
                crate::load::expand::admit_candidate_bytes(
                    device.modifier_memory_snapshot(), allocation.bytes,
                ).map_err(|error| PreAllocationError::AllocationFailed(error.to_string()))?;
                #[cfg(all(test, feature = "gpu-proofs"))]
                crate::gpu::render_target::allocation_checkpoint().map_err(PreAllocationError::AllocationFailed)?;
                let buffer = device
                    .try_create_buffer_shared(allocation.bytes)
                    .map_err(PreAllocationError::AllocationFailed)?;
                if allocation.zero_init { buffer.zero_fill(); }
                backend.pre_bind_array(allocation.resource, buffer);
            }
            ArrayAllocationAction::Reuse { resource, root } => {
                let slot = backend.slot_for(root).or_else(|| {
                    prebound_buffers
                        .get(&root)
                        .cloned()
                        .map(|buffer| backend.pre_bind_array(root, buffer))
                }).ok_or_else(|| PreAllocationError::UnboundArrayResource {
                    producer_node_type: "<resize>".into(),
                    producer_port: format!("resource_{resource:?}"),
                    producer_handle: None,
                    cause: "reused array root has no live backing buffer",
                })?;
                if resource != root {
                    backend.alias_array_resource(resource, slot);
                }
            }
            ArrayAllocationAction::Alias { resource, input } => {
                let slot = backend.slot_for(input)
                    .expect("array plan only aliases known storage");
                backend.alias_array_resource(resource, slot);
            }
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
            let texture = device
                .try_create_texture(&GpuTextureDesc {
                width: w,
                height: h,
                depth: d,
                format,
                dimension: GpuTextureDimension::D3,
                usage: GpuTextureUsage::RENDER_TARGET_FULL,
                label: label_static,
                mip_levels: 1,
            })
            .map_err(PreAllocationError::AllocationFailed)?;
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
    use crate::scene::boundary_nodes::Source;

    #[test]
    fn scene_modifier_v3_runtime_requires_attachment_before_node_installation() {
        let mut doc: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 3, "nodes": [], "wires": [],
            "sceneModifiers": [{
                "id": "modifier", "scene": {"node": "scene"},
                "targets": "allObjects",
                "graph": {"version": 3, "nodes": [], "wires": []}
            }]
        })).unwrap();
        let mut graph = Graph::new();
        let registry = PrimitiveRegistry::new();
        let result = instantiate_def(
            &mut graph, &doc, &registry, HandleScope::Global, BoundaryHandling::Standalone,
        &crate::scene::mesh_change::PreparedMeshRules::default());
        assert!(matches!(result, Err(GraphBuildError::SceneModifier(_))));
        assert_eq!(graph.nodes().count(), 0);

        // The malformed attachment is refused before installation. An ordinary
        // v3 graph still takes the existing runtime path.
        doc.scene_modifiers.clear();
        assert!(instantiate_def(
            &mut graph, &doc, &registry, HandleScope::Global, BoundaryHandling::Standalone,
        &crate::scene::mesh_change::PreparedMeshRules::default()).is_ok());
    }
    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
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
        &crate::scene::mesh_change::PreparedMeshRules::default())
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
        &crate::scene::mesh_change::PreparedMeshRules::default())
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
        use crate::testkit::graph::GraphFixture;
        use crate::exec::{execution_plan::compile, metal_backend::MetalBackend};

        let device = crate::gpu::context::test_gpu_device("graph_loader tests");
        let mut graph = Graph::new();
        let seed = graph.add_node(Box::new(GraphFixture::particles()));
        let step = graph.add_node(Box::new(GraphFixture::particle_step()));
        graph.connect((seed, "particles"), (step, "in")).unwrap();
        let forces = graph.add_node(Box::new(GraphFixture::forces()));
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
        use crate::testkit::graph::GraphFixture;
        use crate::exec::{execution_plan::compile, metal_backend::MetalBackend};

        let device = crate::gpu::context::test_gpu_device("graph_loader tests");
        let mut graph = Graph::new();
        graph.add_node(Box::new(GraphFixture::particles()));
        let plan = compile(&graph).expect("seed-only graph compiles");

        let mut backend = MetalBackend::new(std::sync::Arc::clone(&device), 256, 256, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&mut graph, &plan, &device, &mut backend)
            .expect("full pre-allocate pipeline succeeds for seed-only graph");
    }

    // ── docs/NODE_VOCABULARY_AUDIT.md section 3 test (a) ──
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
            scene_modifiers: Vec::new(),
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
            scene_modifiers: Vec::new(),
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

    /// docs/NODE_VOCABULARY_AUDIT.md section 7.1: the retired `node.rotate_vec2_90`
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
            scene_modifiers: Vec::new(),
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
            scene_modifiers: Vec::new(),
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

    /// docs/NODE_VOCABULARY_AUDIT.md section 7.2: the retired
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
            scene_modifiers: Vec::new(),
            nodes: vec![bare_node(0, "node.fluid_project_scatter_2d")],
            wires: vec![],
        };
        let migrated = migrate_def_type_ids(&old_def, &registry)
            .expect("old id present, migration must produce Some");
        assert_eq!(migrated.nodes[0].type_id, "node.draw_particles_camera");
        assert!(migrated.nodes[0].params.is_empty());
    }

    #[test]
    fn retired_params_saved_nondefault_values_load() {
        let registry = registry();
        for &(type_id, param) in manifold_core::type_id_migration::RETIRED_PARAMS {
            let declared = registry.construct(type_id).expect("retired params belong to a live node type");
            assert!(declared.parameters().iter().all(|p| p.name != param), "{type_id}.{param} is still declared");
            let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
                "version": 1, "nodes": [{"id": 1, "nodeId": "n", "typeId": type_id, "handle": "n",
                    "params": {param: {"type": "Float", "value": 137.0}}}], "wires": []
            })).unwrap();
            assert!(has_retired_params(&def));
            let mut graph = Graph::new();
            instantiate_def(&mut graph, &def, &registry, HandleScope::Global,
                BoundaryHandling::Standalone, &crate::scene::mesh_change::PreparedMeshRules::default())
                .unwrap_or_else(|e| panic!("{type_id}.{param}: saved retired value must load: {e:?}"));
            let node = graph.get_node(graph.node_id_by_handle("n").unwrap()).unwrap();
            assert!(node.params.get(param).is_none(), "{type_id}.{param}");
        }
    }

    #[test]
    fn retired_params_preserve_stable_identity_that_matches_another_handle() {
        let mut def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 1, "nodes": [
                {"id": 1, "nodeId": "solver", "typeId": "node.gpu_flip_step", "handle": "step"},
                {"id": 2, "nodeId": "step", "typeId": "node.scalar", "handle": "unrelated",
                 "params": {"top_speed": {"type": "Float", "value": 23.0}}}
            ], "wires": []
        })).unwrap();
        let mut metadata = minimal_preset_metadata();
        metadata.bindings = serde_json::from_value(serde_json::json!([
            {"id": "speed", "label": "Speed", "defaultValue": 23.0,
             "target": {"kind": "node", "nodeId": "step", "param": "top_speed"}}
        ])).unwrap();
        def.preset_metadata = Some(metadata);
        let original = def.clone();
        assert!(!retire_params(&mut def));
        assert_eq!(def, original, "the actual stable identity takes precedence over another node's handle");
    }

    #[test]
    fn retired_params_isolate_modifier_node_identities() {
        let mut def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 3,
            "nodes": [{"id": 1, "nodeId": "step", "typeId": "node.scalar", "handle": "step",
                "params": {"top_speed": {"type": "Float", "value": 23.0}}}], "wires": [],
            "sceneModifiers": [{"id": "modifier", "scene": {"node": "scene"}, "targets": "allObjects",
                "graph": {"version": 3, "nodes": [
                    {"id": 1, "nodeId": "step", "typeId": "node.gpu_flip_step", "handle": "step",
                     "params": {"top_speed": {"type": "Float", "value": 137.0}}}], "wires": []}}]
        })).unwrap();
        let binding = serde_json::from_value(serde_json::json!({
            "id": "speed", "label": "Speed", "defaultValue": 23.0,
            "target": {"kind": "node", "nodeId": "step", "param": "top_speed"}
        })).unwrap();
        let mut parent_metadata = minimal_preset_metadata();
        parent_metadata.bindings.push(binding);
        def.preset_metadata = Some(parent_metadata.clone());
        def.scene_modifiers[0].graph.preset_metadata = Some(parent_metadata.clone());
        let parent_nodes = def.nodes.clone();
        assert!(retire_params(&mut def));
        assert_eq!(def.nodes, parent_nodes);
        assert_eq!(def.preset_metadata, Some(parent_metadata), "modifier-local identities cannot erase parent bindings");
        assert!(def.scene_modifiers[0].graph.nodes[0].params.is_empty());
        assert!(def.scene_modifiers[0].graph.preset_metadata.as_ref().unwrap().bindings.is_empty());
        let once = def.clone();
        assert!(!retire_params(&mut def));
        assert_eq!(def, once);
    }

    #[test]
    fn retired_params_nested_cards_and_wires_preserve_live_fanout() {
        use manifold_core::effect_graph_def::{BindingTarget, ParamSpecDef};
        let mut def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 1, "nodes": [
                {"id": 0, "typeId": "node.scalar", "handle": "source"},
                {"id": 1, "typeId": "group", "nodeId": "group", "handle": "group",
                 "params": {"speed": {"type": "Float", "value": 93.0}}, "exposedParams": ["speed"],
                 "group": {
                    "interface": {"inputs": [{"name": "dead", "portType": "Scalar(F32)"},
                                              {"name": "shared", "portType": "Scalar(F32)"}], "outputs": [],
                        "params": [{"name": "speed", "targetHandle": "inner/step", "targetParam": "top_speed"}]},
                    "nodes": [
                        {"id": 10, "typeId": "system.group_input"},
                        {"id": 11, "typeId": "group", "handle": "inner", "group": {
                            "interface": {"inputs": [], "outputs": []},
                            "nodes": [{"id": 20, "typeId": "node.gpu_flip_step", "nodeId": "step", "handle": "step",
                                "params": {"top_speed": {"type": "Float", "value": 137.0}}, "exposedParams": ["top_speed"]}],
                            "wires": []}},
                        {"id": 12, "typeId": "node.gpu_flip_step", "nodeId": "other_step", "handle": "other_step"},
                        {"id": 13, "typeId": "node.scalar", "nodeId": "unrelated", "handle": "unrelated",
                         "params": {"top_speed": {"type": "Float", "value": 23.0}}, "exposedParams": ["top_speed"]}
                    ], "wires": [
                        {"fromNode": 10, "fromPort": "dead", "toNode": 12, "toPort": "top_speed"},
                        {"fromNode": 10, "fromPort": "shared", "toNode": 12, "toPort": "top_speed"},
                        {"fromNode": 10, "fromPort": "shared", "toNode": 13, "toPort": "top_speed"}
                    ]}}
            ], "wires": [{"fromNode": 0, "fromPort": "value", "toNode": 1, "toPort": "speed"},
                         {"fromNode": 0, "fromPort": "value", "toNode": 1, "toPort": "dead"},
                         {"fromNode": 0, "fromPort": "value", "toNode": 1, "toPort": "shared"}]
        })).unwrap();
        let mut metadata = minimal_preset_metadata();
        for id in ["retired", "fanout", "group_speed", "legacy"] {
            metadata.params.push(ParamSpecDef { id: id.into(), name: id.into(), ..Default::default() });
        }
        metadata.bindings = serde_json::from_value(serde_json::json!([
            {"id": "retired", "label": "Retired", "defaultValue": 137.0, "userAdded": true,
             "target": {"kind": "node", "nodeId": "step", "param": "top_speed"}},
            {"id": "legacy", "label": "Legacy", "defaultValue": 137.0,
             "target": {"kind": "handleNode", "handle": "group/inner/step", "param": "top_speed"}},
            {"id": "group_speed", "label": "Group", "defaultValue": 93.0,
             "target": {"kind": "node", "nodeId": "group", "param": "speed"}},
            {"id": "fanout", "label": "Mixed", "defaultValue": 23.0,
             "target": {"kind": "node", "nodeId": "step", "param": "top_speed"}},
            {"id": "fanout", "label": "Mixed", "defaultValue": 23.0,
             "target": {"kind": "node", "nodeId": "unrelated", "param": "top_speed"}}
        ])).unwrap();
        def.preset_metadata = Some(metadata);
        let unrelated = def.nodes[1].group.as_ref().unwrap().nodes[3].clone();
        assert!(retire_params(&mut def));
        let group_node = &def.nodes[1];
        assert!(group_node.params.is_empty() && group_node.exposed_params.is_empty());
        let group = group_node.group.as_ref().unwrap();
        assert!(group.interface.params.is_empty());
        assert_eq!(group.interface.inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["shared"]);
        assert_eq!(group.nodes[3], unrelated, "same-named unrelated input stays byte-for-byte intact");
        let step = &group.nodes[1].group.as_ref().unwrap().nodes[0];
        assert!(step.params.is_empty() && step.exposed_params.is_empty());
        assert_eq!(group.wires.len(), 1);
        assert_eq!(def.wires.len(), 1);
        let metadata = def.preset_metadata.as_ref().unwrap();
        assert_eq!(metadata.params.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["fanout"]);
        assert_eq!(metadata.bindings.len(), 1);
        assert!(matches!(&metadata.bindings[0].target, BindingTarget::Node {node_id, param}
            if node_id.as_str() == "unrelated" && param == "top_speed"));
        let once = def.clone();
        assert!(!retire_params(&mut def));
        assert_eq!(def, once, "repeat migration leaves the normalized document unchanged");
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
                layer_types: None,
            params: Vec::new(),
            bindings: Vec::new(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
            scene_modifier: None,
            scene_bounds: None,
        }
    }

    /// (a, continued) The same fixture through the real `instantiate_def`
    /// entry point, boundary nodes only (the sentinel isn't a registered
    /// primitive, so it can't sit mid-graph and still construct) — proves
    /// the choke point is actually wired into the function tests above call
    /// directly, not just defined alongside it.
    #[test]
    fn instantiate_def_migrates_boundary_free_standing_old_id_graph() {
        // `system.*` ids are exempt from migration (section 2 rule 7) and are the
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
        &crate::scene::mesh_change::PreparedMeshRules::default())
        .expect("system.* boundary ids are unaffected by migration and always construct");
    }

}
