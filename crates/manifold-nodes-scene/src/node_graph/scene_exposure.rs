//! Renderer-side implementation of `docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` P1.
//!
//! - `metadata_for_node_type` reads a primitive's `ParamDef` table through the
//!   registry.
//! - `migrate_scene_exposures` is the load-time idempotent migration that stamps
//!   exposures onto every scene-vocabulary node in an existing graph.
//! - `PrimitiveRegistrySceneExposureProvider` implements the core trait for
//!   creation-site commands that cannot depend on `manifold_nodes` directly.

use manifold_core::effect_graph_def::EffectGraphDef;
mod compound;
mod fluid_objects;
use manifold_core::liquid_domain::{LIQUID_DOMAIN_TYPE_IDS, is_liquid_domain, liquid_dial_params};
use manifold_core::scene_exposure::{SceneExposureMetadataProvider, SceneParamMetadata};

use manifold_node_engine::parameters::ParamType;
use manifold_node_engine::persistence::PrimitiveRegistry;
use crate::node_graph::material_inspector::material_param_role;

static SCENE_EXPOSURE_REGISTRY: std::sync::LazyLock<PrimitiveRegistry> =
    std::sync::LazyLock::new(PrimitiveRegistry::with_builtin);

static SCENE_VOCABULARY: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
    SCENE_VOCABULARY_TYPE_IDS.iter().chain(LIQUID_DOMAIN_TYPE_IDS).copied().collect()
});

/// An unbounded angle has no primitive range to copy into an exposure, but a
/// scene card still needs a useful initial scrub band. Keep the band in the
/// primitive's storage unit (radians); the `is_angle` flag makes the card
/// present and edit it in degrees. The primitive itself remains unbounded.
const UNBOUNDED_ANGLE_EXPOSURE_RANGE: (f32, f32) =
    (-std::f32::consts::TAU, std::f32::consts::TAU);

/// Scene-vocabulary type ids — the nodes whose params the scene panel wants to
/// address. Kept in sync with `scene_vm.rs`. Every liquid domain joins them
/// through `LIQUID_DOMAIN_TYPE_IDS` (`scene_vocabulary`).
const SCENE_VOCABULARY_TYPE_IDS: &[&str] = &[
    "node.rigid_body",
    "node.physics_world",
    "node.fluid_role_source",
    "node.transform_3d",
    "node.pbr_material",
    "node.unlit_material",
    "node.cel_material",
    "node.light",
    "node.orbit_camera",
    "node.free_camera",
    "node.look_at_camera",
    "node.camera_lens",
    "node.bokeh_gather",
    "node.motion_blur",
    "node.atmosphere",
    "node.bake_environment",
    "node.scene_object",
    "node.bend_mesh",
    "node.twist_mesh",
    "node.taper_mesh",
    "node.push_along_normals",
    "node.push_mesh",
    "node.morph_mesh",
    "node.rotate_3d",
    // RAYTRACING_DESIGN.md D14/section 5.2: the scene-level RT toggles live on the
    // `node.render_scene` root. Curated to the RT subset in
    // `metadata_for_node_type` — the root node's other params (sun, env,
    // counts) stay hand-curated exposures, never auto-stamped.
    "node.render_scene",
    // The liquid's whitewater: its on/off and amount join the water's
    // Simulation controls (BUG-ejcb); its rates and budget stay graph-side
    // or hand-curated.
    "node.whitewater_step",
];

/// Whether Scene Setup automatically exposes this node's detailed controls.
/// Performance projection uses the same vocabulary to avoid leaking them.
pub fn is_scene_setup_node(type_id: &str) -> bool {
    SCENE_VOCABULARY.contains(&type_id)
}

/// The curated `node.render_scene` auto-stamp subset (see the vocabulary
/// entry above): the per-scene RT toggle (D14), the MetalFX temporal
/// quality toggle (P4), the per-scene reflection toggle (section 9 RD9),
/// the per-scene ML-denoiser feed toggle (RAYTRACING_DESIGN.md section
/// 17 DN4), and the per-term RT toggles (shadows/AO/GI — the hybrid-split
/// levers). Everything else on the root node is deliberately NOT auto-stamped.
const RENDER_SCENE_STAMPED_PARAMS: &[&str] = &[
    "rt_enabled",
    "temporal_upscale",
    "rt_reflections",
    "rt_shadows",
    "rt_ao",
    "rt_gi",
    "rt_denoise_feed",
];

/// Return the full param manifest for `type_id` from the primitive registry,
/// converting `ParamDef` metadata into the crate-neutral `SceneParamMetadata`
/// shape. Empty when the type is unknown.
pub fn metadata_for_node_type(type_id: &str) -> Vec<SceneParamMetadata> {
    metadata_for_node_type_with_registry(&SCENE_EXPOSURE_REGISTRY, type_id, None)
}

manifold_core::testkit_visible! {
/// Convert one registry node's descriptors to scene metadata. Proofs may pass
/// an explicit dial list for a retired solver whose product controls are no
/// longer exposed by `manifold-core`.
pub(crate) fn metadata_for_node_type_with_registry(
    registry: &PrimitiveRegistry,
    type_id: &str,
    dial_whitelist: Option<&[&str]>,
) -> Vec<SceneParamMetadata> {
    let Some(node) = registry.construct(type_id) else {
        return Vec::new();
    };
    let dials = dial_whitelist.or_else(|| liquid_dial_params(type_id));
    node.parameters()
        .iter()
        .filter(|pd| {
            (type_id != "node.render_scene"
                || RENDER_SCENE_STAMPED_PARAMS.contains(&pd.name.as_ref()))
                && (type_id != "node.bokeh_gather"
                    || matches!(pd.name.as_ref(), "enabled" | "aperture" | "quality"))
        })
        .filter(|pd| type_id != "node.rigid_body" || matches!(pd.name.as_ref(), "shape" | "motion" | "density" | "friction" | "bounce" | "collider_parts"))
        .filter(|pd| type_id != "node.scene_object" || pd.name.as_ref() != "parent_visible")
        .filter(|pd| dials.is_none_or(|dials| dials.contains(&pd.name.as_ref())))
        .filter(|pd| type_id != "node.whitewater_step" || matches!(pd.name.as_ref(), "enabled" | "amount" | "wavecrest_emission" | "turbulence_emission" | "min_turbulence" | "max_turbulence" | "inside_emission" | "dust_emission" | "boundary_dust" | "dust_rate" | "spray_speed" | "generation_rate" | "influence_base" | "influence_decay"))
        .filter(|pd| type_id != "node.fluid_role_source" || matches!(pd.name.as_ref(),
            "role" | "enabled" | "geometry" | "shape" | "radius"
                | "velocity_x" | "velocity_y" | "velocity_z" | "inherit_motion"
                | "friction" | "collider_parts"))
        .map(|pd| {
            let (min, max) = pd.range.unwrap_or({
                if matches!(pd.ty, ParamType::Angle) {
                    UNBOUNDED_ANGLE_EXPOSURE_RANGE
                } else {
                    (0.0, 1.0)
                }
            });
            let default_value: manifold_core::effect_graph_def::SerializedParamValue =
                pd.default.clone().into();
            let is_angle = matches!(pd.ty, ParamType::Angle);
            let whole_numbers = matches!(pd.ty, ParamType::Int | ParamType::Enum);
            let is_toggle = matches!(pd.ty, ParamType::Bool);
            let is_trigger = matches!(pd.ty, ParamType::Trigger);
            // R2 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): display labels
            // aren't Enum-exclusive — a modulatable `Float` threshold param
            // (e.g. `node.light`'s `cast_shadows`, kept `Float` on purpose so
            // it stays LFO/trigger-modulatable) can declare `enum_values` as
            // display-only text for the outer-card slider
            // (`format_param_value` substitutes by index regardless of the
            // param's real type). Read straight off whatever the primitive
            // declared, not gated by `ty`.
            let value_labels = pd.enum_values.iter().map(|s| s.to_string()).collect();
            let convert = match pd.ty {
                ParamType::Bool => manifold_core::effects::ParamConvert::BoolThreshold,
                ParamType::Int => manifold_core::effects::ParamConvert::IntRound,
                ParamType::Enum => manifold_core::effects::ParamConvert::EnumRound,
                ParamType::Trigger => manifold_core::effects::ParamConvert::Trigger,
                _ => manifold_core::effects::ParamConvert::Float,
            };
            SceneParamMetadata {
                name: pd.name.to_string(),
                label: pd.label.to_string(),
                min,
                max,
                default_value,
                is_angle,
                // Periodicity is authored by the exposure kind, not inferred
                // from every angle (FOV and tilt are intentionally bounded).
                wraps: false,
                whole_numbers,
                is_toggle,
                is_trigger,
                value_labels,
                convert,
                material_role: material_param_role(type_id, pd.name.as_ref()),
            }
        })
        .collect()
}
}

/// Explicit Water-look exposure metadata, never part of load-time vocabulary.
pub fn look_metadata() -> Vec<SceneParamMetadata> {
    metadata_for_node_type("node.platonic_solid_mesh").into_iter()
        .filter(|metadata| metadata.name == "radius")
        .map(|mut metadata| { metadata.label = "Size".into(); metadata })
        .collect()
}

/// Idempotent load-time migration: stamp exposures for every scene-vocabulary
/// node in `def`. Returns `true` iff anything changed. Safe to run on any graph
/// (non-scene defs are untouched).
pub fn migrate_scene_exposures(def: &mut EffectGraphDef) -> bool {
    if manifold_core::retired_cpu_flip::graph_contains_retired_cpu_flip_node(def) {
        return false;
    }
    let material_migrated = manifold_core::phong_migration::migrate_phong_to_pbr(def);
    let compound = compound::migrate(def);
    let fluid_objects = fluid_objects::migrate(def);
    let repaired = repair_legacy_lens_f_stop(def);
    let provider = PrimitiveRegistrySceneExposureProvider;
    let migrated = manifold_core::scene_exposure::migrate_scene_exposures(
        def,
        &SCENE_VOCABULARY,
        section_name_for_node,
        &provider,
    );
    let bokeh_source_migrated = migrate_bokeh_source_coc(def);
    compound
        || fluid_objects
        || material_migrated
        || repaired
        || migrated
        || bokeh_source_migrated
}

/// The layered gather consumes the original signed CoC and computes its own
/// conservative tile bounds. Older saved graphs routed that CoC through the
/// standalone `coc_dilate` node first, which inflated radii at silhouettes.
/// Rewrite only the canonical `coc_from_depth → coc_dilate → bokeh_gather`
/// shape at load time. A dilation node shared by another consumer stays in
/// place; noncanonical producers remain untouched.
fn migrate_bokeh_source_coc(def: &mut EffectGraphDef) -> bool {
    fn migrate_scope(
        nodes: &mut Vec<manifold_core::effect_graph_def::EffectGraphNode>,
        wires: &mut Vec<manifold_core::effect_graph_def::EffectGraphWire>,
    ) -> bool {
        let mut changed = false;

        for node in nodes.iter_mut() {
            if let Some(group) = node.group.as_deref_mut() {
                changed |= migrate_scope(&mut group.nodes, &mut group.wires);
            }
        }

        let bokeh_ids: Vec<u32> = nodes
            .iter()
            .filter(|node| node.type_id == "node.bokeh_gather")
            .map(|node| node.id)
            .collect();
        for bokeh_id in bokeh_ids {
            let Some(width_index) = wires.iter().position(|wire| {
                wire.to_node == bokeh_id && wire.to_port == "width" && wire.from_port == "out"
            }) else {
                continue;
            };
            let dilate_id = wires[width_index].from_node;
            let is_dilate = nodes
                .iter()
                .any(|node| node.id == dilate_id && node.type_id == "node.coc_dilate");
            if !is_dilate {
                continue;
            }
            let Some(source_wire) = wires.iter().find(|wire| {
                wire.to_node == dilate_id && wire.to_port == "in" && wire.from_port == "out"
            }) else {
                continue;
            };
            let is_coc_source = nodes.iter().any(|node| {
                node.id == source_wire.from_node && node.type_id == "node.coc_from_depth"
            });
            if !is_coc_source {
                continue;
            }

            let source_id = source_wire.from_node;
            wires[width_index].from_node = source_id;
            wires[width_index].from_port = "out".to_string();
            changed = true;

            let has_other_consumer = wires.iter().any(|wire| wire.from_node == dilate_id);
            if !has_other_consumer {
                wires.retain(|wire| wire.from_node != dilate_id && wire.to_node != dilate_id);
                nodes.retain(|node| node.id != dilate_id);
            }
        }
        // Camera DoF blurs a scene's transparent silhouette too. Keep the
        // primitive's legacy alpha-preserving default for arbitrary textures,
        // and respect an explicitly saved transparency choice.
        for index in 0..nodes.len() {
            if nodes[index].type_id != "node.bokeh_gather"
                || nodes[index].params.contains_key("blur_alpha")
            {
                continue;
            }
            let is_camera_dof = wires.iter().any(|wire| {
                wire.to_node == nodes[index].id && wire.to_port == "width"
                    && wire.from_port == "out"
                    && nodes.iter().any(|node| {
                        node.id == wire.from_node && node.type_id == "node.coc_from_depth"
                    })
            });
            if is_camera_dof {
                nodes[index].params.insert("blur_alpha".to_string(),
                    manifold_core::effect_graph_def::SerializedParamValue::Bool { value: true });
                changed = true;
            }
        }
        changed
    }

    migrate_scope(&mut def.nodes, &mut def.wires)
}

/// Legacy tail repair (2026-08-27): pre-fix projects carry the lens's old
/// neutral `f_stop = 1000` seed. 1000 sat outside the param's 0.5–32 band,
/// and the stamper's widen rule stretched every stamped f-stop slider to
/// fit — the unusable-ranges bug Peter reported on a fresh import. The same
/// fix that brings the value back in band (32) also moves "DoF off" off the
/// f-stop axis entirely: off is bokeh's `enabled` toggle now, seeded false
/// (no f-stop value is off on close-up scenes — f/32 blurs visibly past the
/// focus plane). A stored 1000 is proof the lens was never dialed, so every
/// bokeh in the def is forced to enabled=false alongside the rewrite —
/// otherwise migrated projects would GAIN visible DoF on load, changing
/// their look. Guarded on exactly 1000.0: any other f-stop is a performer's
/// choice (or already repaired) and the whole pass is left alone. Runs
/// BEFORE the core stamp/repair passes so they see the corrected defaults;
/// idempotent — a second run finds no 1000.0 and writes nothing.
fn repair_legacy_lens_f_stop(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::{BindingTarget, SerializedParamValue};

    let mut lens_node_ids: Vec<manifold_core::NodeId> = Vec::new();
    let mut bokeh_node_ids: Vec<manifold_core::NodeId> = Vec::new();
    fn collect_and_repair(
        nodes: &mut [manifold_core::effect_graph_def::EffectGraphNode],
        lenses: &mut Vec<manifold_core::NodeId>,
    ) -> bool {
        let mut changed = false;
        for node in nodes.iter_mut() {
            if node.type_id.as_str() == "node.camera_lens"
                && let Some(SerializedParamValue::Float { value }) = node.params.get_mut("f_stop")
                && *value == 1000.0
            {
                *value = 32.0;
                lenses.push(node.node_id.clone());
                changed = true;
            }
            if let Some(group) = node.group.as_deref_mut() {
                changed |= collect_and_repair(&mut group.nodes, lenses);
            }
        }
        changed
    }
    fn collect_bokehs(
        nodes: &mut [manifold_core::effect_graph_def::EffectGraphNode],
        bokehs: &mut Vec<manifold_core::NodeId>,
    ) {
        for node in nodes.iter_mut() {
            if node.type_id.as_str() == "node.bokeh_gather" {
                node.params.insert(
                    "enabled".to_string(),
                    SerializedParamValue::Bool { value: false },
                );
                bokehs.push(node.node_id.clone());
            }
            if let Some(group) = node.group.as_deref_mut() {
                collect_bokehs(&mut group.nodes, bokehs);
            }
        }
    }
    if !collect_and_repair(&mut def.nodes, &mut lens_node_ids) {
        return false;
    }
    collect_bokehs(&mut def.nodes, &mut bokeh_node_ids);

    // Keep the stamps in step: the default repairs in core key off the node
    // param, but they only widen ranges — the stretched max needs the band
    // re-derived from the current metadata (0.5–32 widened by the new 32
    // default is exactly the band). Bokeh stamps follow the new off default.
    let f_stop_meta = metadata_for_node_type("node.camera_lens")
        .into_iter()
        .find(|m| m.name == "f_stop");
    if let Some(preset) = def.preset_metadata.as_mut() {
        if let Some(meta) = f_stop_meta {
            for node_id in &lens_node_ids {
                let Some(binding) = preset.bindings.iter_mut().find(|b| {
                    !b.user_added
                        && matches!(
                            &b.target,
                            BindingTarget::Node { node_id: nid, param }
                                if nid == node_id && param == "f_stop"
                        )
                }) else {
                    continue;
                };
                if binding.default_value == 1000.0 {
                    binding.default_value = 32.0;
                }
                let binding_id = binding.id.clone();
                if let Some(spec) = preset.params.iter_mut().find(|p| p.id == binding_id) {
                    if spec.default_value == 1000.0 {
                        spec.default_value = 32.0;
                    }
                    spec.min = meta.min.min(spec.default_value);
                    spec.max = meta.max.max(spec.default_value);
                }
            }
        }
        for node_id in &bokeh_node_ids {
            let Some(binding) = preset.bindings.iter_mut().find(|b| {
                !b.user_added
                    && matches!(
                        &b.target,
                        BindingTarget::Node { node_id: nid, param }
                            if nid == node_id && param == "enabled"
                    )
            }) else {
                continue;
            };
            binding.default_value = 0.0;
            let binding_id = binding.id.clone();
            if let Some(spec) = preset.params.iter_mut().find(|p| p.id == binding_id) {
                spec.default_value = 0.0;
            }
        }
    }
    true
}

fn section_name_for_node(node: &manifold_core::effect_graph_def::EffectGraphNode) -> String {
    let display = node
        .title
        .as_deref()
        .or(node.handle.as_deref())
        .unwrap_or("Scene");
    let category = match node.type_id.as_str() {
        "node.rigid_body" | "node.physics_world" => "Physics".to_string(),
        domain if is_liquid_domain(domain) => "Simulation".to_string(),
        "node.fluid_role_source" => "Source".to_string(),
        "node.transform_3d" => "Transform".to_string(),
        "node.pbr_material" | "node.unlit_material" | "node.cel_material" => {
            "Material".to_string()
        }
        "node.light" => return display.to_string(),
        "node.orbit_camera"
        | "node.free_camera"
        | "node.look_at_camera"
        | "node.camera_lens"
        | "node.motion_blur"
        | "node.bokeh_gather" => {
            "Camera".to_string()
        }
        "node.atmosphere" => "Atmosphere".to_string(),
        "node.bake_environment" => "Environment".to_string(),
        "node.render_scene" => return "Rendering".to_string(),
        "node.whitewater_step" => return "Whitewater".to_string(),
        "node.scene_object" => "Object".to_string(),
        _ => {
            // Modifiers and anything else: use the type id suffix.
            node.type_id
                .strip_prefix("node.")
                .map(|s| {
                    let mut s = s.to_string();
                    s.replace_range(0..1, &s[0..1].to_uppercase());
                    s
                })
                .unwrap_or_else(|| "Modifier".to_string())
        }
    };
    format!("{} — {}", display, category)
}

/// Zero-sized provider backed by the lazy static registry. Commands in
/// `manifold_editing` store a `Box<dyn SceneExposureMetadataProvider>` and call
/// this at execute time.
pub struct PrimitiveRegistrySceneExposureProvider;

impl SceneExposureMetadataProvider for PrimitiveRegistrySceneExposureProvider {
    fn metadata_for_type(&self, type_id: &str) -> Vec<SceneParamMetadata> {
        metadata_for_node_type(type_id)
    }
}


inventory::submit! {
    manifold_node_engine::scene::exposure_source::SceneExposureSource {
        metadata: metadata_for_node_type,
        look: look_metadata,
    }
}

#[cfg(test)]
mod exposure_source_tests {
    #[test]
    fn scene_exposure_registration_preserves_metadata() {
        assert_eq!(super::look_metadata(), manifold_node_engine::scene::exposure_source::look_metadata());
        for type_id in ["node.scene_object", "node.pbr_material", "node.camera", "missing"] {
            assert_eq!(super::metadata_for_node_type(type_id),
                manifold_node_engine::scene::exposure_source::metadata_for_node_type(type_id));
        }
    }
}

#[cfg(any(test, feature = "testkit"))]
pub mod testkit {
    pub const SCENE_VOCABULARY_TYPE_IDS: &[&str] = super::SCENE_VOCABULARY_TYPE_IDS;

    pub fn migrate_fluid_objects(def: &mut manifold_core::effect_graph_def::EffectGraphDef) -> bool {
        super::fluid_objects::migrate(def)
    }

    pub fn migrate_bokeh_source_coc(def: &mut manifold_core::effect_graph_def::EffectGraphDef) -> bool {
        super::migrate_bokeh_source_coc(def)
    }

    pub fn section_name_for_node(node: &manifold_core::effect_graph_def::EffectGraphNode) -> String {
        super::section_name_for_node(node)
    }
}
