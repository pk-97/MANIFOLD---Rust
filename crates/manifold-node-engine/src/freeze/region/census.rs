//! Fusion refusal census (FUSION_SOTA P3 / D4): buckets every refusal by the
//! family D4 names and estimates the dispatches saved if each were lifted.
//! Read-only instrumentation: every function here DUPLICATES `classify_node` /
//! `classify_buffer_node` / `build_region`'s gates for reporting. It never
//! disagrees with them about Eligible/Boundary and changes nothing about what
//! fuses. `refusal_census_matches_classify_node` pins that agreement.
//!
//! The report is `docs/fusion_census.md`, written by
//! `cargo run -p manifold-renderer --example fusion_census`.

use super::*;
use crate::persistence::PrimitiveRegistry;
use crate::ports::PortType;
use manifold_core::effect_graph_def::EffectGraphDef;

/// One of the eight buckets FUSION_SOTA_DESIGN.md D4/P3 names. `Other`
/// catches every boundary that isn't one of D4's four under-fusing
/// families or the D3 BufferIndex-shaped family — control wires,
/// register-heavy bodies, non-GPU/IoBridge/DrawCall/CrossFrameState/
/// BarrieredReduction/FusedBundle/ConversionDebt atoms, Texture3D ports,
/// q16/particle-loop cuts, specialization-token cuts, and any
/// `build_region` drop that isn't the fan-out message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RefusalFamily {
    ParamType,
    Arity,
    MultiOutput,
    Resample,
    BufferFanOut,
    StencilDepth,
    BufferIndexShaped,
    Other,
}

impl RefusalFamily {
    fn label(self) -> &'static str {
        match self {
            RefusalFamily::ParamType => "param-type",
            RefusalFamily::Arity => "arity",
            RefusalFamily::MultiOutput => "multi-output",
            RefusalFamily::Resample => "resample",
            RefusalFamily::BufferFanOut => "buffer-fan-out",
            RefusalFamily::StencilDepth => "stencil-depth",
            RefusalFamily::BufferIndexShaped => "buffer-index-shaped",
            RefusalFamily::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct FamilyStats {
    refusals: usize,
    dispatches_saved: usize,
}

/// WHY a node classifies `Boundary` — same gate ORDER as [`classify_node`],
/// stopping at the first cut and naming its D4 family. `None` for an
/// `Eligible` node (nothing to bucket) — the function must never return
/// `Some` where `classify_node` returns `Eligible`, or vice versa; that
/// invariant is exactly what `refusal_census_matches_classify_node` checks.
pub fn classify_refusal(
    node: &EffectGraphNode,
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Option<RefusalFamily> {
    use crate::freeze::classify::{BoundaryReason, FusionKind};

    if node.type_id == SOURCE_TYPE_ID || node.type_id == FINAL_OUTPUT_TYPE_ID {
        return None; // graph endpoints: seams by identity, not a refusal
    }
    let Some(n) = configured_construct(registry, node) else {
        return Some(RefusalFamily::Other); // unknown atom
    };
    if !matches!(
        n.fusion_kind(),
        FusionKind::Pointwise | FusionKind::MultiInputCoincident | FusionKind::Source
    ) {
        return Some(match n.boundary_reason() {
            Some(BoundaryReason::Blocked) => RefusalFamily::BufferIndexShaped,
            _ => RefusalFamily::Other,
        });
    }
    if n.wgsl_body().is_none() {
        return Some(RefusalFamily::Other);
    }
    if n.fusion_register_heavy() {
        return Some(RefusalFamily::Other);
    }
    for p in n.parameters() {
        // Mirrors `classify_node`'s narrowed param gate exactly (P5/D4):
        // Vec3/Vec4/Color now pass via `param_is_fusable`; only
        // Table/String still bucket as `ParamType`. Must stay in
        // lockstep with `classify_node` — `refusal_census_matches_
        // classify_node` enforces it.
        if !crate::freeze::codegen::param_is_fusable(p) {
            return Some(RefusalFamily::ParamType);
        }
    }
    if n.outputs().iter().any(|o| matches!(o.ty, PortType::Array(_))) {
        // Buffer-domain gates (classify_buffer_node): BufferFanOut is a
        // REGION-level refusal (`build_region`'s "fan-out buffer region"
        // error, counted in `region_level_refusals` below), never a
        // per-node one — a node-level buffer refusal here is always Other.
        return match classify_buffer_node(n.as_ref(), node, def, registry) {
            NodeClass::Boundary => Some(RefusalFamily::Other),
            NodeClass::Eligible => None,
        };
    }
    let tex_in = n.inputs().iter().filter(|i| is_texture_port(&i.ty)).count();
    let tex_out = n.outputs().iter().filter(|o| is_texture_port(&o.ty)).count();
    let arity_ok = if matches!(n.fusion_kind(), FusionKind::Source) {
        tex_in == 0
    } else {
        tex_in >= 1
    };
    // Mirrors `classify_node`'s narrowed cut rule 6 exactly (D4/P6): a
    // MULTI-output atom (tex_out >= 2) is no longer a refusal by itself —
    // every atom that declares ≥2 texture outputs on the codegen path is
    // already struct-return, so `RefusalFamily::MultiOutput` now only
    // fires for the true arity mismatch (wrong texture-INPUT count for
    // the atom's FusionKind, or zero texture outputs at all — no register
    // to thread). Kept as its own bucket (not folded into `Arity`) so a
    // future non-struct-return multi-output atom would still show up
    // distinctly if the authoring contract is ever violated — but no atom
    // shipped today reaches it (`refusal_census_matches_classify_node`
    // enforces the lockstep either way).
    if !arity_ok || tex_out == 0 {
        return Some(RefusalFamily::Arity);
    }
    let default_params: crate::exec::effect_node::ParamValues = AHashMap::default();
    for o in n.outputs().iter().filter(|o| is_texture_port(&o.ty)) {
        if let Some(scale) = n.output_canvas_scale(o.name.as_ref(), &default_params)
            && scale != (1, 1)
        {
            return Some(RefusalFamily::Resample);
        }
    }
    if n.inputs().iter().any(|i| i.ty == PortType::Texture3D)
        || n.outputs().iter().any(|o| o.ty == PortType::Texture3D)
    {
        return Some(RefusalFamily::Other);
    }
    let tex_ports: AHashSet<&str> = n
        .inputs()
        .iter()
        .filter(|i| is_texture_port(&i.ty))
        .map(|i| i.name.as_ref())
        .collect();
    let scalar_params: AHashSet<&str> = n
        .parameters()
        .iter()
        .filter(|p| param_wgsl_type(p).is_ok())
        .map(|p| p.name.as_ref())
        .collect();
    let camera_ports: AHashSet<&str> = if n.derived_uniforms().is_empty() {
        AHashSet::default()
    } else {
        n.inputs()
            .iter()
            .filter(|i| matches!(i.ty, PortType::Camera | PortType::Object))
            .map(|i| i.name.as_ref())
            .collect()
    };
    // D3 exemption mirror (BUG-114) — must match `classify_node`'s own
    // `buffer_index_ports` set exactly, or a `BufferIndex`-tagged Array
    // wire (draw_dots' `detections`) wrongly buckets `Other` here once
    // the param gate above (P5) no longer stops execution before this
    // wire loop. Found by `refusal_census_matches_classify_node` going
    // red the moment P5's param-gate narrowing made this code reachable
    // for draw_dots for the first time.
    let buffer_index_ports: AHashSet<&str> = {
        let tex_count = n.inputs().iter().filter(|i| is_texture_port(&i.ty)).count();
        n.inputs()
            .iter()
            .filter(|i| matches!(i.ty, PortType::Array(_)))
            .enumerate()
            .filter(|(idx, _)| {
                n.input_access().get(tex_count + idx) == Some(&InputAccess::BufferIndex)
            })
            .map(|(_, i)| i.name.as_ref())
            .collect()
    };
    for w in &def.wires {
        if w.to_node == node.id
            && !tex_ports.contains(w.to_port.as_str())
            && !scalar_params.contains(w.to_port.as_str())
            && !camera_ports.contains(w.to_port.as_str())
            && !buffer_index_ports.contains(w.to_port.as_str())
        {
            return Some(RefusalFamily::Other);
        }
        if w.from_node == node.id && is_scalar_param_wire(def, registry, w) {
            return Some(RefusalFamily::Other);
        }
    }
    if !q16_tier_enabled()
        && node_on_cycle(node.id, def)
        && !node.output_formats.values().any(|s| s.contains("32float"))
    {
        return Some(RefusalFamily::Other);
    }
    if !node.output_formats.values().any(|s| s.contains("32float"))
        && cycle_contains_array(node.id, def, registry)
    {
        return Some(RefusalFamily::Other);
    }
    for (_, sp_param) in n.wgsl_specialization() {
        if param_is_binding_target(node, sp_param, def)
            || def.wires.iter().any(|w| w.to_node == node.id && w.to_port == *sp_param)
        {
            return Some(RefusalFamily::Other);
        }
    }
    let Some(body) = substituted_body(n.as_ref(), node) else {
        return Some(RefusalFamily::Other);
    };
    let standalone = crate::freeze::codegen::generate_standalone(
        &crate::freeze::codegen::StandaloneKernelSpec {
            fusion_kind: n.fusion_kind(),
            body: &body,
            inputs: n.inputs(),
            params: n.parameters(),
            input_access: n.input_access(),
            derived_uniforms: n.derived_uniforms(),
            outputs: n.outputs(),
            stencil_fetch: n.stencil_fetch(),
            includes: n.wgsl_includes(),
        },
    );
    match standalone {
        Ok(kernel) if naga::front::wgsl::parse_str(&kernel).is_ok() => None,
        _ => Some(RefusalFamily::Other),
    }
}

/// Replays `partition_regions`' union-candidate/convexity pass to find every
/// eligible↔eligible connected component of size ≥2 and the `build_region`
/// verdict for each, so the census can bucket the refusal strings by family.
fn component_build_results(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Vec<(Vec<u32>, Result<(), &'static str>)> {
    let spaces = resolve_output_spaces(def, registry);
    let class: AHashMap<u32, NodeClass> = def
        .nodes
        .iter()
        .map(|n| (n.id, classify_node(n, def, registry)))
        .collect();
    let eligible: AHashSet<u32> = class
        .iter()
        .filter(|(_, c)| **c == NodeClass::Eligible)
        .map(|(id, _)| *id)
        .collect();
    let forward: Vec<(u32, u32)> = def
        .wires
        .iter()
        .filter(|w| !is_state_capture_wire(def, registry, w))
        .map(|w| (w.from_node, w.to_node))
        .collect();
    let mut candidates: Vec<(u32, u32)> = def
        .wires
        .iter()
        .filter(|w| {
            eligible.contains(&w.from_node)
                && eligible.contains(&w.to_node)
                && (is_texture_wire(def, registry, w) || is_array_wire(def, registry, w))
                && wire_coincident_consumed(def, registry, w)
                && (is_array_wire(def, registry, w)
                    || space_of(spaces.as_ref(), w.from_node, &w.from_port)
                        == node_output_space(spaces.as_ref(), def, registry, w.to_node))
        })
        .map(|w| (w.from_node, w.to_node))
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    // D4/P6: mirrors `partition_regions`' gather-bridge guard (see its
    // comment) — must stay in lockstep or this test helper would report a
    // component the real algorithm no longer forms.
    let gather_pairs: Vec<(u32, u32)> = def
        .wires
        .iter()
        .filter(|w| {
            eligible.contains(&w.from_node)
                && eligible.contains(&w.to_node)
                && is_texture_wire(def, registry, w)
                && !wire_coincident_consumed(def, registry, w)
        })
        .map(|w| (w.from_node, w.to_node))
        .collect();
    let mut uf = UnionFind::new(&eligible);
    for (a, b) in candidates {
        if uf.find(a) == uf.find(b) {
            continue;
        }
        let finds: AHashMap<u32, u32> = eligible.iter().map(|&n| (n, uf.find(n))).collect();
        let (ra, rb) = (uf.find(a), uf.find(b));
        let key = |n: u32| -> u32 {
            match finds.get(&n) {
                Some(&r) if r == rb => ra,
                Some(&r) => r,
                None => n,
            }
        };
        let would_bridge_a_gather_wire =
            gather_pairs.iter().any(|&(gp, gc)| key(gp) == key(gc));
        if !collapsed_has_cycle(&forward, &key) && !would_bridge_a_gather_wire {
            uf.union(a, b);
        }
    }
    let mut components: AHashMap<u32, Vec<u32>> = AHashMap::default();
    for &id in &eligible {
        components.entry(uf.find(id)).or_default().push(id);
    }
    let final_reachable = final_reachable_nodes(def);
    let mut out = Vec::new();
    for (_, mut nodes) in components {
        nodes.sort_unstable();
        if nodes.len() < 2 {
            continue;
        }
        let result = build_region(def, registry, &nodes, &final_reachable, spaces.as_ref()).map(|_| ());
        out.push((nodes, result));
    }
    out
}

/// Bucket every refusal in one (already-flattened) def into `stats`. Three
/// passes: (1) per-node `classify_refusal`, with a conservative
/// lower-bound dispatches-saved estimate (documented below); (2)
/// region-level stencil-depth (a gather producer that is itself Eligible,
/// sits in a fused region of ≥2 members, and was NOT absorbed — the exact
/// `MAX_VIRTUAL_CHAIN=1` cut); (3) region-level `build_region` drops
/// (fan-out named explicitly, everything else Other).
fn census_def(def: &EffectGraphDef, registry: &PrimitiveRegistry, stats: &mut AHashMap<RefusalFamily, FamilyStats>,
) {
    let regions = partition_regions(def, registry);
    let member_of: AHashMap<u32, usize> = regions
        .iter()
        .enumerate()
        .flat_map(|(i, r)| r.members.iter().map(move |m| (m.doc_id, i)))
        .collect();

    // Pass 1 — per-node refusals. Dispatches-saved estimator: a
    // CONSERVATIVE LOWER BOUND, documented in docs/fusion_census.md — 1 if
    // the refused node has at least one texture/Array-wired neighbour
    // (either direction) that classifies `Eligible`, else 0. That single
    // neighbour is the case lifting this node would join it to (2
    // dispatches collapse to 1); it does not credit bridging two existing
    // regions together or a chain of >1 newly-eligible neighbours, so the
    // real saving from lifting a whole family is >= what's reported here.
    for n in &def.nodes {
        let Some(family) = classify_refusal(n, def, registry) else { continue;
        };
        let has_eligible_neighbor = def.wires.iter().any(|w| {
            let (other, this_is_from) = if w.from_node == n.id {
                (w.to_node, true)
            } else if w.to_node == n.id {
                (w.from_node, false)
            } else {
                return false;
            };
            let _ = this_is_from;
            (is_texture_wire(def, registry, w) || is_array_wire(def, registry, w))
                && def
                    .nodes
                    .iter()
                    .find(|o| o.id == other)
                    .is_some_and(|on| {
                        classify_node(on, def, registry) == NodeClass::Eligible
                    })
        });
        let e = stats.entry(family).or_default();
        e.refusals += 1;
        e.dispatches_saved += usize::from(has_eligible_neighbor);
    }

    // Pass 2 — stencil-depth: a Gather-consumed external producer that is
    // ITSELF Eligible and sits in a region of >=2 members (i.e. its chain
    // is strictly longer than MAX_VIRTUAL_CHAIN=1) and wasn't absorbed.
    // Saved estimate: 1 (the consumer's own dispatch the fetch would fold
    // away, on top of whatever the chain's own region already fuses).
    for r in &regions {
        for m in &r.members {
            for (idx, (input, access)) in m.inputs.iter().zip(&m.input_access).enumerate() {
                if *access != InputAccess::Gather {
                    continue;
                }
                let RegionInput::External(e) = input else { continue;
                };
                let prod = r.externals[*e].from_node;
                let Some(&prod_region) = member_of.get(&prod) else { continue;
                };
                if regions[prod_region].members.len() < 2 {
                    continue; // single-node chain — MAX_VIRTUAL_CHAIN=1 admits it; not this family
                }
                let absorbed = r
                    .virtual_chains
                    .iter()
                    .any(|vc| vc.consumer == m.doc_id && vc.input_index == idx);
                if absorbed {
                    continue;
                }
                let e_ = stats.entry(RefusalFamily::StencilDepth).or_default();
                e_.refusals += 1;
                e_.dispatches_saved += 1;
            }
        }
    }

    // Pass 3 — region-level `build_region` drops (fan-out named
    // explicitly; every other drop reason is Other — space mismatch, dead
    // region, escaping-to-dead-consumer, cycle).
    for (nodes, result) in component_build_results(def, registry) {
        let Err(reason) = result else { continue };
        let family = if reason.contains("fan-out buffer region") {
            RefusalFamily::BufferFanOut
        } else {
            RefusalFamily::Other
        };
        let e = stats.entry(family).or_default();
        e.refusals += 1;
        e.dispatches_saved += nodes.len().saturating_sub(1);
    }
}

/// The Liveschool fixture's `embedded_presets`: per-instance forked or edited
/// graphs, the one place a real show's graphs diverge from the catalog's
/// canonical defs. The caller loads the project, since `manifold-io` is only a
/// dev-dependency of this crate.
pub enum FixtureCorpus {
    NotFound,
    LoadFailed(String),
    Loaded { embedded: Vec<EffectGraphDef> },
}

/// Run the census over every bundled effect/generator preset plus the
/// fixture corpus, and render the FUSION_SOTA_DESIGN section D4 report.
/// Returns the exact text committed to `docs/fusion_census.md`.
pub fn build_census_report(fixture: FixtureCorpus) -> String {
    let registry = PrimitiveRegistry::with_builtin();
    let mut stats: AHashMap<RefusalFamily, FamilyStats> = AHashMap::default();
    let mut preset_count = 0usize;
    let mut liveschool_count = 0usize;
    let mut liveschool_note = String::from("NOT FOUND (gitignored fixture; skipped)");

    let mut census_json = |json: &str| {
        if let Ok(def) = serde_json::from_str::<manifold_core::effect_graph_def::EffectGraphDef>(json)
            && let Ok(flat) = manifold_core::flatten::flatten_groups(&def)
        {
            census_def(&flat, &registry, &mut stats);
        }
    };

    for type_id in crate::load::catalog_source::preset_type_ids(
        manifold_core::preset_def::PresetKind::Effect,
    ) {
        if let Some(view) = crate::load::loaded_preset_view::loaded_preset_view_by_id(&type_id) {
            let json = serde_json::to_string(&view.canonical_def).unwrap();
            census_json(&json);
            preset_count += 1;
        }
    }
    for type_id in crate::load::catalog_source::preset_type_ids(
        manifold_core::preset_def::PresetKind::Generator,
    ) {
        if let Some(json) = crate::load::catalog_source::preset_json(&type_id) {
            census_json(&json);
            preset_count += 1;
        }
    }

    match fixture {
        FixtureCorpus::NotFound => {
            eprintln!("[census] Liveschool fixture not found on disk — skipping");
        }
        FixtureCorpus::LoadFailed(error) => {
            eprintln!("[census] Liveschool fixture found but failed to load: {error}");
            liveschool_note = format!("load FAILED: {error}");
        }
        FixtureCorpus::Loaded { embedded } => {
            for def in &embedded {
                let Ok(flat) = manifold_core::flatten::flatten_groups(def) else {
                    continue;
                };
                census_def(&flat, &registry, &mut stats);
                liveschool_count += 1;
            }
            liveschool_note = format!(
                "{liveschool_count} embedded_presets censused (of {} total in the project)",
                embedded.len()
            );
        }
    }

    let mut out = String::new();
    out.push_str("# Fusion refusal census (FUSION_SOTA P3 / D4)\n\n");
    out.push_str(
        "Generated by `crates/manifold-node-engine/src/freeze/region/census.rs`'s \
         `build_census_report`. Regenerate with:\n\n```\ncargo run -p manifold-renderer \
         --example fusion_census\n```\n\n",
    );
    out.push_str(&format!(
        "Corpus: {preset_count} bundled effect/generator presets + Liveschool fixture \
         ({liveschool_note}).\n\n",
    ));
    if liveschool_count == 0 {
        out.push_str(
            "**Liveschool note:** the fixture loaded successfully but has ZERO \
             `embedded_presets` — this real show never forked/saved a preset to the project \
             (PROJECT_LIBRARY_DESIGN's \"fork\" mechanism), so every card it plays IS one of \
             the canonical bundled defs above, differing only by param VALUES, which don't \
             change fusion topology. The bundled-preset census already covers Liveschool's \
             graph shapes; the fixture run adds no additional structural coverage this time, \
             but stays wired so a future show that DOES fork a preset gets censused too.\n\n",
        );
    }
    out.push_str(
        "**Dispatches-saved estimator (documented, not hand-waved):** a CONSERVATIVE LOWER \
         BOUND. Per-node families (param-type/arity/multi-output/resample/buffer-index-shaped/\
         other): 1 dispatch saved if the refused node has >=1 texture/Array-wired neighbour \
         that already classifies `Eligible` (the node it would join, collapsing 2 dispatches \
         into 1), else 0 — a lone refused node with no eligible neighbour would form no region \
         even if lifted, so it correctly counts 0. This undercounts bridging (a lifted node \
         connecting two existing regions into one saves MORE than 1) and chains of newly-eligible \
         neighbours. Stencil-depth: 1 per (region, gather-member) pair whose producer is Eligible, \
         sits in a region of >=2 members (the exact MAX_VIRTUAL_CHAIN=1 cut), and wasn't absorbed \
         — the consumer's own dispatch the fetch would fold away, on top of what the chain's own \
         region already fuses. Buffer-fan-out / other region-level drops: component size - 1 (the \
         component renders as N separate dispatches today; fused it would be 1).\n\n",
    );
    out.push_str("| Family | Refusals | Dispatches saved if lifted |\n|---|---:|---:|\n");
    let mut rows: Vec<(RefusalFamily, FamilyStats)> = stats.into_iter().collect();
    rows.sort_by_key(|(f, _)| *f);
    for (family, s) in &rows {
        out.push_str(&format!("| {} | {} | {} |\n", family.label(), s.refusals, s.dispatches_saved));
    }
    out.push('\n');

    let get = |f: RefusalFamily| {
        rows.iter().find(|(rf, _)| *rf == f).map(|(_, s)| *s).unwrap_or_default()
    };
    let fan_out = get(RefusalFamily::BufferFanOut);
    let resample = get(RefusalFamily::Resample);
    let vec3 = get(RefusalFamily::ParamType);
    let multi = get(RefusalFamily::MultiOutput);

    out.push_str("## Reading the numbers against D4's defaults\n\n");
    out.push_str(&format!(
        "**Vec3 params — LIFT (default stands).** `param-type` bucket also includes Table/String \
         (boundary by nature per D4, not debt) so this count is an upper bound on Vec3-specifically; \
         {} refusal(s) / {} dispatch(es) potentially saved support building the lift as scoped for P5.\n\n",
        vec3.refusals, vec3.dispatches_saved
    ));
    out.push_str(&format!(
        "**Multi-output texture atoms — LIFT, census-gated (default stands).** {} refusal(s) / {} \
         dispatch(es). {}\n\n",
        multi.refusals,
        multi.dispatches_saved,
        if multi.refusals == 0 {
            "Zero shipped-preset cuts from this family — per D4, it still lands as capability \
             (voronoi's cell+distance vocabulary) but ordered LAST of the lifts, exactly the \
             documented fallback."
        } else {
            "Non-zero — P6 has shipped-preset cuts to point at, strengthening (not just \
             capability-justifying) the case to build it."
        }
    ));
    out.push_str(&format!(
        "**Buffer fan-out — DEFER (trigger: >=3 refusals across shipped content). Measured: {} \
         refusal(s) / {} dispatch(es) potentially saved.** {}\n\n",
        fan_out.refusals,
        fan_out.dispatches_saved,
        if fan_out.refusals >= 3 {
            "**TRIGGER CROSSED — see ESCALATION in the P3 phase report; this default is NOT \
             changed here, only flagged.**"
        } else {
            "Below the trigger — DEFER stands as-is."
        }
    ));
    out.push_str(&format!(
        "**Resample-into-region — DEFER (trigger: evidence of hot resample-sandwich chains). \
         Measured: {} refusal(s) / {} dispatch(es) potentially saved.** {}\n\n",
        resample.refusals,
        resample.dispatches_saved,
        "This census counts STRUCTURAL refusal COUNT, not runtime hotness (dispatch frequency \
         isn't visible to a static def walk) — a nonzero count is not itself the trigger; the \
         trigger is evidence of a HOT sandwich, which needs a runtime profile, not this census. \
         DEFER stands as-is from this data alone.",
    ));
    out.push_str(
        "**Nested stencils (`MAX_VIRTUAL_CHAIN=1`) — CORRECT AS-IS, not a D4-revisable family; \
         the `stencil-depth` row above is diagnostic only (D4 explicitly rejects raising the cap \
         without a measured per-region cost comparison this census cannot do).**\n",
    );
    out
}
