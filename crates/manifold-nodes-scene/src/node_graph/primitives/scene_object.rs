//! `node.scene_object` — binds one scene object's mesh, transform, material,
//! maps, instances, and optional live instance count into a single
//! [`SceneObject`] wire.
//!
//! An object today is
//! "whatever named group happens to wrap the wires feeding `mesh_k`" — a
//! convention `SceneVm` reverse-engineers. This primitive makes the object a
//! typed graph fact instead: it consumes the same wires `render_scene`'s
//! per-object port family consumed (mesh vertices, transform, material, nineteen
//! maps, instances) and emits ONE [`Object`](crate::node_graph::ports::PortType::Object)
//! wire carrying a [`SceneObject`]. `render_scene` takes `object_k: Object`
//! (P2 of the design) instead of nine parallel per-object wires that had to
//! stay index-coherent by luck.
//!
//! `visible` is port-shadowed exactly like every `node.light` scalar —
//! "mute the statue on the drop" is a MIDI binding, not a feature request.
//! `false` means no draw AND no shadow cast.
//!
//! CPU-only — no GPU dispatch, no `wgsl_body`/`fusion_kind` (same codegen-
//! mandate exemption class as `node.light` / `node.transform_3d`: an IO/CPU
//! bridge, not a per-element GPU atom). Resources referenced through the
//! emitted struct (mesh/map/instance `Slot`s) escape the planner's normal
//! wire-based lifetime view — `carries_resources()` on the `EffectNode`
//! trait is the seam that fixes this (`effect_node.rs`, planner extension in
//! `execution_plan.rs`), the same rule `variadic_skip_passthrough_out`
//! already implements for muxes.

use std::borrow::Cow;

use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::{InstanceTransform, MeshVertex, Vec4Vertex};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::scene_object::{
    Cover, FieldSurface, GasLight, GasLook, GasVolume, InstanceSpace, SceneObject,
};

/// Each Cover's material port and its base colour, normal and
/// metallic-roughness map ports, Cover 1 then Cover 2.
const COVER_PORTS: [(&str, [&str; 3]); 2] = [
    (
        "cover1",
        ["cover1_base_color_map", "cover1_normal_map", "cover1_mr_map"],
    ),
    (
        "cover2",
        ["cover2_base_color_map", "cover2_normal_map", "cover2_mr_map"],
    ),
];

manifold_node_engine::primitive! {
    name: SceneObjectNode,
    type_id: "node.scene_object",
    purpose: "Binds one scene object's mesh vertices, transform, material, nineteen maps (base colour / normal / metallic-roughness / occlusion / emissive / sheen colour / sheen roughness / iridescence / iridescence thickness / anisotropy / clearcoat / clearcoat roughness / clearcoat normal / specular / specular colour / transmission / diffuse transmission / diffuse transmission colour / volume thickness), instances, and an optional live instance count into a single Object wire consumed by render_scene's object_k ports. Object wires never chain — this is the sole producer, and it takes no Object input. `visible` is port-shadowed so muting the object is a MIDI/LFO binding, not a graph edit; false means no draw AND no shadow cast. CPU-only bridge: no GPU dispatch of its own — mesh/map/instance resources are forwarded as Slots, resolved by the consumer exactly as render_scene resolves them today.",
    inputs: {
        vertices: Array(MeshVertex) optional,
        indices: Array(u32) optional,
        lods: Array(u32) optional,
        clusters: Array(u32) optional,
        weights: Array(f32) optional,
        topology: Array(Vec4Vertex) optional,
        prev_positions: Array(Vec4Vertex) optional,
        field: Array(f32) optional,
        field_velocity: Array(f32) optional,
        field_foam: Array(FluidParticle) optional,
        field_mist: Array(f32) optional,
        wet_map: Array(u32) optional,
        field_tiles: Array(u32) optional,
        field_extent: Array(f32) optional,
        field_pool_tiles: ScalarF32 optional,
        field_cell_size: ScalarF32 optional,
        field_ripple_detail: ScalarF32 optional,
        gas_state: Array(u32) optional,
        gas_velocity: Array(u32) optional,
        gas_cell_size: ScalarF32 optional,
        gas_origin_x: ScalarF32 optional,
        gas_origin_y: ScalarF32 optional,
        gas_origin_z: ScalarF32 optional,
        gas_dims_x: ScalarF32 optional,
        gas_dims_y: ScalarF32 optional,
        gas_dims_z: ScalarF32 optional,
        gas_light_power: ScalarF32 optional,
        gas_light_x: ScalarF32 optional,
        gas_light_y: ScalarF32 optional,
        gas_light_z: ScalarF32 optional,
        gas_light_temperature: ScalarF32 optional,
        transform: Transform optional,
        parent_transform: Transform optional,
        parent_visible: ScalarF32 optional,
        material: Material optional,
        base_color_map: Texture2D optional,
        normal_map: Texture2D optional,
        mr_map: Texture2D optional,
        occlusion_map: Texture2D optional,
        emissive_map: Texture2D optional,
        sheen_color_map: Texture2D optional,
        sheen_roughness_map: Texture2D optional,
        iridescence_map: Texture2D optional,
        iridescence_thickness_map: Texture2D optional,
        anisotropy_map: Texture2D optional,
        clearcoat_map: Texture2D optional,
        clearcoat_roughness_map: Texture2D optional,
        clearcoat_normal_map: Texture2D optional,
        specular_map: Texture2D optional,
        specular_color_map: Texture2D optional,
        transmission_map: Texture2D optional,
        diffuse_transmission_map: Texture2D optional,
        diffuse_transmission_color_map: Texture2D optional,
        volume_thickness_map: Texture2D optional,
        cover1: Material optional,
        cover1_base_color_map: Texture2D optional,
        cover1_normal_map: Texture2D optional,
        cover1_mr_map: Texture2D optional,
        cover2: Material optional,
        cover2_base_color_map: Texture2D optional,
        cover2_normal_map: Texture2D optional,
        cover2_mr_map: Texture2D optional,
        instances: Array(InstanceTransform) optional,
        instance_count: ScalarF32 optional,
        copy_scale: Transform optional,
        visible: ScalarF32 optional,
        cast_shadows: ScalarF32 optional,
        gain: ScalarF32 optional,
    },
    outputs: {
        object: Object,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("visible"),
            label: "Visible",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("parent_visible"),
            label: "Parent Visible",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cast_shadows"),
            label: "Cast Shadows",
            // Stays Float/0..1 (not Bool), same rationale as
            // `node.light`'s `cast_shadows` (`light.rs`): modulatable by
            // an LFO/trigger, which the `Bool` `ParamValue` shape doesn't
            // support. `enum_values` supplies display-only labels for the
            // card slider text; the primitive's own threshold read
            // (`> 0.5`, below) is unaffected.
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &["Off", "On"],
        },
        // A water field's optional looks, each a switch on its card: off, the
        // renderer leaves the look out and runs none of its passes. Mist
        // starts off because its passes cost 2 to 4 ms a frame.
        ParamDef {
            name: Cow::Borrowed("foam"),
            label: "Foam",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &["Off", "On"],
        },
        // Times the foam particles' area: 1 is the foam they make, 0 none.
        ParamDef {
            name: Cow::Borrowed("foam_coverage"),
            label: "Foam Coverage",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mist"),
            label: "Spray Mist",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &["Off", "On"],
        },
        ParamDef {
            name: Cow::Borrowed("wet_surfaces"),
            label: "Wet Surfaces",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &["Off", "On"],
        },
        ParamDef {
            name: Cow::Borrowed("ripples"),
            label: "Surface Ripples",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &["Off", "On"],
        },
        ParamDef {
            name: Cow::Borrowed("caustics"),
            label: "Caustics",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &["Off", "On"],
        },
        ParamDef {
            name: Cow::Borrowed("caustic_intensity"),
            label: "Caustic Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emission_strength"),
            label: "Emission Strength",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("gain"),
            label: "Level",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("instances_space"),
            label: "Instances Space",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 2.0)),
            enum_values: &["Object", "Group", "World"],
        },
        ParamDef {
            name: Cow::Borrowed("closed"),
            label: "Closed Mesh",
            // On only for meshes known closed and outward-wound (the
            // built-in solids): an opaque draw then skips back faces.
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
        // A Smoke's or Fire's look (`node.gas_volume` wired to gas_state):
        // the renderer's volume pass reads these.
        // The smoke's colour (its scattering tint), red.
        ParamDef {
            name: Cow::Borrowed("gas_smoke_r"),
            label: "Smoke Red",
            ty: ParamType::Float,
            default: ParamValue::Float(0.85),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("gas_smoke_g"),
            label: "Smoke Green",
            ty: ParamType::Float,
            default: ParamValue::Float(0.85),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("gas_smoke_b"),
            label: "Smoke Blue",
            ty: ParamType::Float,
            default: ParamValue::Float(0.85),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        // Extinction per metre at density 1.
        ParamDef {
            name: Cow::Borrowed("gas_density"),
            label: "Smoke Density",
            ty: ParamType::Float,
            default: ParamValue::Float(30.0),
            range: Some((0.0, 200.0)),
            enum_values: &[],
        },
        // The share of the light the smoke stops that it scatters on, not absorbs.
        ParamDef {
            name: Cow::Borrowed("gas_albedo"),
            label: "Smoke Brightness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.9),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        // Fine swirls the march adds below the cells, carried by the flow.
        ParamDef {
            name: Cow::Borrowed("gas_detail"),
            label: "Detail",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        // Flame radiance at full flame and 1500 K, in the scene's light units.
        ParamDef {
            name: Cow::Borrowed("gas_flame_brightness"),
            label: "Flame Brightness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        // The flame's colour temperature in kelvin where it is coolest.
        ParamDef {
            name: Cow::Borrowed("gas_kelvin_cool"),
            label: "Flame Cool",
            ty: ParamType::Float,
            default: ParamValue::Float(900.0),
            range: Some((500.0, 4000.0)),
            enum_values: &[],
        },
        // The flame's colour temperature in kelvin where it is hottest.
        ParamDef {
            name: Cow::Borrowed("gas_kelvin_hot"),
            label: "Flame Hot",
            ty: ParamType::Float,
            default: ParamValue::Float(1800.0),
            range: Some((500.0, 4000.0)),
            enum_values: &[],
        },
        // The flames' light on their own smoke, times Flame Brightness.
        ParamDef {
            name: Cow::Borrowed("gas_glow"),
            label: "Glow",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        // The fire's light on the scene, times what its flames give off; 0 adds no light.
        ParamDef {
            name: Cow::Borrowed("gas_light_brightness"),
            label: "Fire Light",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 20.0)),
            enum_values: &[],
        },
        // The gas render preset: march samples, light volume, detail and
        // scattering octaves (render_scene/gas_volume.rs `render_preset`).
        ParamDef {
            name: Cow::Borrowed("gas_quality"),
            label: "Gas Quality",
            ty: ParamType::Enum,
            default: ParamValue::Enum(2),
            range: Some((0.0, 3.0)),
            enum_values: &["Low", "Medium", "High", "Extra High"],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Optional indices are u32 triangle indices into vertices; their live extent controls indexed draws. Optional lods are the indexed mesh's detail levels (node.gltf_mesh_source's lods); an instanced draw draws each copy at the coarsest level under a pixel of error. Optional clusters are the indexed mesh's crack-free cluster hierarchy (node.gltf_mesh_source's clusters); a single draw draws each part of it at the coarsest level under a pixel. Optional prev_positions is each vertex's position a frame ago (one vec4 per vertex, as many as the draw reads), for a mesh whose triangles are rebuilt every frame (node.tile_surface_mesh with a velocity): the velocity pass reads it instead of keeping a history, so the object blurs with its motion. An unindexed draw stops at the vertices' live extent, which a fragment cut publishes and the steps after it forward. Wire `object` into render_scene's object_k port (replacing the legacy mesh_k/material_k/…/instances_k nine-wire family). Unwired inputs read as the same unresolved/identity defaults their legacy per-object ports did: vertices unwired = no draw (consumer skip, matching render_scene.rs's existing tolerance), transform unwired = identity TRS, material unwired = the consumer's existing structured-error path, maps unwired = no map. `instance_count` is optional and has no param fallback: with `instances` wired, an unwired count preserves buffer capacity while a wired value is floored and clamped to capacity; zero, negative, NaN, and infinity produce zero draws. `visible` is a [0, 1] threshold (> 0.5 = on) so it can be modulated by an LFO or MIDI, or bound to an eye-toggle in the panel. Optional `field` is a level set (signed distance per node, negative inside) on the water's display world tiles: `field_tiles` is the world-tile table every field array is laid out under, `field_extent` its eight-float extent (min xyz, 0, max xyz, 0, object space), `field_pool_tiles` the tile slots in the pool and `field_cell_size` the lattice cell in metres, with node p at (p - 1/2) cells; plus optional `field_velocity` (three f32 per node), `field_foam` and `field_mist` (one f32 per node), all pooled the same way, and `wet_map`, node.wet_map's map, which every mesh in the scene reads to look wet: the object then draws the field instead of a mesh. A field has no box of its own, only its extent. With `field` wired, `vertices` is the field's own surface mesh (node.tile_surface_mesh over the same level set): the renderer draws its depth over the opaque scene and marches each ray only round it, so a pixel with no mesh in front draws no water. With `field` wired the tiles, extent, pool and cell size are required; a missing one, a pool under one tile or a cell size at or below zero is a node error and the object carries no field.",
    examples: [],
    picker: { label: "Scene Object", category: Driver },
    summary: "Binds one object's mesh, transform, material, maps, instances, and optional live count into a single wire. Wire it into a render_scene object slot.",
    category: Geometry3D,
    role: Source,
    aliases: ["scene object", "object", "object bundle"],
    boundary_reason: NonGpu,
}

impl Primitive for SceneObjectNode {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let visible = ctx.scalar_or_param("visible", 1.0) > 0.5;
        let cast_shadows = ctx.scalar_or_param("cast_shadows", 1.0) > 0.5;
        let closed = matches!(ctx.params.get("closed"), Some(ParamValue::Bool(true)));
        let emission_strength = ctx.scalar_or_param("emission_strength", 1.0);
        let gain = ctx.scalar_or_param("gain", 1.0);
        let mut transform = ctx.inputs.transform("transform").unwrap_or_default();
        // A copy group's instances carry one uniform scale; `copy_scale` is the
        // rest of the prototype's per-axis scale, applied inside each copy
        // under the object's own chain (Group space).
        if let Some(copy) = ctx.inputs.transform("copy_scale") {
            let [x, y, z] = copy.scale;
            let scale = [
                [x, 0.0, 0.0, 0.0],
                [0.0, y, 0.0, 0.0],
                [0.0, 0.0, z, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ];
            transform.parent =
                manifold_node_engine::mesh::pipeline::mat4_mul(scale, transform.parent);
        }
        let parent_transform = ctx.inputs.transform("parent_transform");
        let parent_visible = ctx.scalar_or_param("parent_visible", 1.0) > 0.5;
        let material = ctx.inputs.material("material");
        let mesh = ctx.inputs.slot_of("vertices");
        let weights = ctx.inputs.slot_of("weights");
        let topology = ctx.inputs.slot_of("topology");
        // A hidden object reads no field, so Physics View can switch the
        // hidden water's field off without a node error.
        let field = (visible && parent_visible)
            .then(|| ctx.inputs.slot_of("field"))
            .flatten()
            .and_then(|levelset| field_surface(ctx, levelset));
        let gas = (visible && parent_visible)
            .then(|| ctx.inputs.slot_of("gas_state"))
            .flatten()
            .and_then(|state| gas_volume(ctx, state));
        let base_color_map = ctx.inputs.slot_of("base_color_map");
        let normal_map = ctx.inputs.slot_of("normal_map");
        let mr_map = ctx.inputs.slot_of("mr_map");
        let occlusion_map = ctx.inputs.slot_of("occlusion_map");
        let emissive_map = ctx.inputs.slot_of("emissive_map");
        let sheen_color_map = ctx.inputs.slot_of("sheen_color_map");
        let sheen_roughness_map = ctx.inputs.slot_of("sheen_roughness_map");
        let iridescence_map = ctx.inputs.slot_of("iridescence_map");
        let iridescence_thickness_map = ctx.inputs.slot_of("iridescence_thickness_map");
        let anisotropy_map = ctx.inputs.slot_of("anisotropy_map");
        let clearcoat_map = ctx.inputs.slot_of("clearcoat_map");
        let clearcoat_roughness_map = ctx.inputs.slot_of("clearcoat_roughness_map");
        let clearcoat_normal_map = ctx.inputs.slot_of("clearcoat_normal_map");
        let specular_map = ctx.inputs.slot_of("specular_map");
        let specular_color_map = ctx.inputs.slot_of("specular_color_map");
        let transmission_map = ctx.inputs.slot_of("transmission_map");
        let diffuse_transmission_map = ctx.inputs.slot_of("diffuse_transmission_map");
        let diffuse_transmission_color_map = ctx.inputs.slot_of("diffuse_transmission_color_map");
        let volume_thickness_map = ctx.inputs.slot_of("volume_thickness_map");
        let covers = COVER_PORTS.map(|(material, maps)| {
            ctx.inputs.material(material).map(|material| Cover {
                material,
                maps: maps.map(|port| ctx.inputs.slot_of(port)),
            })
        });
        let instances = ctx.inputs.slot_of("instances");
        let instance_count = ctx
            .inputs
            .scalar("instance_count")
            .and_then(|value| value.as_scalar());
        let instances_space = match ctx.params.get("instances_space") {
            Some(ParamValue::Enum(index)) => InstanceSpace::from_index(*index),
            _ => InstanceSpace::Object,
        };

        let object = SceneObject {
            visible: visible && parent_visible,
            cast_shadows,
            closed,
            transform,
            parent_transform,
            material,
            mesh,
            indices: ctx.inputs.slot_of("indices"),
            lods: ctx.inputs.slot_of("lods"),
            clusters: ctx.inputs.slot_of("clusters"),
            prev_positions: ctx.inputs.slot_of("prev_positions"),
            weights,
            topology,
            field,
            gas,
            covers,
            base_color_map,
            normal_map,
            mr_map,
            occlusion_map,
            emissive_map,
            sheen_color_map,
            sheen_roughness_map,
            iridescence_map,
            iridescence_thickness_map,
            anisotropy_map,
            clearcoat_map,
            clearcoat_roughness_map,
            clearcoat_normal_map,
            specular_map,
            specular_color_map,
            transmission_map,
            diffuse_transmission_map,
            diffuse_transmission_color_map,
            volume_thickness_map,
            instances,
            instance_count,
            instances_space,
            emission_strength,
            gain,
        };

        ctx.outputs.set_object("object", object);
    }

    /// This node forwards mesh/map/
    /// instance `Slot`s inside the emitted `SceneObject` struct — resources
    /// referenced through a struct field are invisible to the planner's
    /// normal wire-based lifetime tracking. `carries_resources` is the seam
    /// that fixes this (see `EffectNode::carries_resources` doc for the
    /// planner-side rule).
    fn carries_resources(&self) -> bool {
        true
    }
}

fn scalar_input(ctx: &EffectNodeContext<'_, '_>, name: &str) -> Option<f32> {
    ctx.inputs.scalar(name).and_then(|value| value.as_scalar())
}

/// The wired field and its display tiles. The table, the extent, the pool size
/// and the cell size are required with a field: a missing one, a pool under one
/// tile or a cell size at or below zero is a node error, and the object draws
/// no field.
fn field_surface(ctx: &mut EffectNodeContext<'_, '_>, levelset: Slot) -> Option<FieldSurface> {
    let (Some(tiles), Some(extent), Some(pool_tiles), Some(cell_size)) = (
        ctx.inputs.slot_of("field_tiles"),
        ctx.inputs.slot_of("field_extent"),
        scalar_input(ctx, "field_pool_tiles"),
        scalar_input(ctx, "field_cell_size"),
    ) else {
        ctx.error(
            "scene_object: `field` is wired but its world-tile table, extent, pool size or cell size is not",
        );
        return None;
    };
    if !(pool_tiles >= 1.0 && pool_tiles.is_finite() && cell_size > 0.0 && cell_size.is_finite()) {
        ctx.error(format!(
            "scene_object: a field needs a pool of at least 1 tile and a cell size above zero (pool {pool_tiles}, cell {cell_size})"
        ));
        return None;
    }
    // Unwired reads 1.0, the automatic strength.
    let ripple_detail = scalar_input(ctx, "field_ripple_detail").unwrap_or(1.0);
    if ripple_detail.is_nan() || ripple_detail < 0.0 {
        ctx.error(format!(
            "scene_object: a field's ripple detail must be zero or above (got {ripple_detail})"
        ));
        return None;
    }
    let on = |ctx: &EffectNodeContext<'_, '_>, look: &str| ctx.scalar_or_param(look, 1.0) > 0.5;
    let foam_coverage = ctx.scalar_or_param("foam_coverage", 1.0);
    let caustics = if on(ctx, "caustics") {
        ctx.scalar_or_param("caustic_intensity", 1.0).max(0.0)
    } else {
        0.0
    };
    Some(FieldSurface {
        levelset,
        velocity: ctx.inputs.slot_of("field_velocity"),
        foam: ctx
            .inputs
            .slot_of("field_foam")
            .filter(|_| on(ctx, "foam") && foam_coverage > 0.0),
        foam_coverage,
        mist: ctx.inputs.slot_of("field_mist").filter(|_| on(ctx, "mist")),
        wet_map: ctx
            .inputs
            .slot_of("wet_map")
            .filter(|_| on(ctx, "wet_surfaces")),
        tiles,
        extent,
        pool_tiles: pool_tiles.round() as u32,
        cell_size,
        ripple_detail: if on(ctx, "ripples") {
            ripple_detail
        } else {
            0.0
        },
        caustics,
    })
}

/// The wired gas and its box: the velocity, cell size, origin and dims are
/// required with a gas state; a missing one, a cell size at or below zero or
/// an empty box is a node error, and the object draws no gas.
fn gas_volume(ctx: &mut EffectNodeContext<'_, '_>, state: Slot) -> Option<GasVolume> {
    let scalar = |name: &str| scalar_input(ctx, name);
    let (Some(velocity), Some(cell_size), Some(ox), Some(oy), Some(oz), Some(nx), Some(ny), Some(nz)) = (
        ctx.inputs.slot_of("gas_velocity"),
        scalar("gas_cell_size"),
        scalar("gas_origin_x"),
        scalar("gas_origin_y"),
        scalar("gas_origin_z"),
        scalar("gas_dims_x"),
        scalar("gas_dims_y"),
        scalar("gas_dims_z"),
    ) else {
        ctx.error(
            "scene_object: `gas_state` is wired but its velocity, cell size, origin or dims is not",
        );
        return None;
    };
    let dims = [nx, ny, nz].map(|n| n.round().max(0.0) as u32);
    if !(cell_size > 0.0 && cell_size.is_finite()) || dims.contains(&0) {
        ctx.error(format!(
            "scene_object: a gas needs a cell size above zero and at least one cell per axis (cell {cell_size}, dims {dims:?})"
        ));
        return None;
    }
    let param = |ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32| {
        ctx.scalar_or_param(name, default)
    };
    let quality = match ctx.params.get("gas_quality") {
        Some(ParamValue::Enum(index)) => (*index).min(3),
        _ => 2,
    };
    Some(GasVolume {
        state,
        velocity,
        origin: [ox, oy, oz],
        cell_size,
        dims,
        look: GasLook {
            smoke_color: [
                param(ctx, "gas_smoke_r", 0.85),
                param(ctx, "gas_smoke_g", 0.85),
                param(ctx, "gas_smoke_b", 0.85),
            ],
            density: param(ctx, "gas_density", 30.0).max(0.0),
            albedo: param(ctx, "gas_albedo", 0.9).clamp(0.0, 1.0),
            detail: param(ctx, "gas_detail", 0.0).clamp(0.0, 1.0),
            flame_brightness: param(ctx, "gas_flame_brightness", 0.0).max(0.0),
            kelvin_cool: param(ctx, "gas_kelvin_cool", 900.0),
            kelvin_hot: param(ctx, "gas_kelvin_hot", 1800.0),
            glow: param(ctx, "gas_glow", 0.0).max(0.0),
            light_brightness: param(ctx, "gas_light_brightness", 0.0).max(0.0),
            quality,
        },
        light: GasLight {
            position: [
                scalar_input(ctx, "gas_light_x").unwrap_or(0.0),
                scalar_input(ctx, "gas_light_y").unwrap_or(0.0),
                scalar_input(ctx, "gas_light_z").unwrap_or(0.0),
            ],
            power: scalar_input(ctx, "gas_light_power").unwrap_or(0.0).max(0.0),
            temperature: scalar_input(ctx, "gas_light_temperature").unwrap_or(0.0),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{Beats, Seconds};
    use manifold_node_engine::bindings::{NodeInputs, NodeOutputs, Slot};
    use manifold_node_engine::exec::backend::Backend;
    use manifold_node_engine::exec::backend::MockBackend;
    use manifold_node_engine::exec::effect_node::{FrameTime, ParamValues};
    use manifold_node_engine::exec::execution_plan::ResourceId;
    use manifold_node_engine::ports::{ArrayType, PortType};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    #[test]
    fn unwired_object_is_invisible_by_default_flag_but_visible_param_defaults_on() {
        // Fully unwired: visible param defaults to 1.0 (on), transform
        // defaults identity, everything else None.
        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Object, None, (0, 0));

        let mut params = ParamValues::default();
        params.insert(
            std::borrow::Cow::Borrowed("visible"),
            ParamValue::Float(1.0),
        );

        let mut prim = SceneObjectNode::new();
        let inputs_bindings: &[(&'static str, Slot)] = &[];
        let outputs_bindings: &[(&'static str, Slot)] = &[("object", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut render_mode_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let inputs = NodeInputs::new(inputs_bindings, &backend, &[]);
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut render_mode_scratch,
            &mut object_scratch,
        );
        let time = frame_time();
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);

        for (slot, value) in object_scratch.drain(..) {
            backend.set_object(slot, value);
        }

        let object = backend.object(out_slot).expect("object should be set");
        assert!(object.visible);
        assert_eq!(
            object.transform,
            manifold_node_engine::scene::transform::Transform::default()
        );
        assert!(object.material.is_none());
        assert!(object.mesh.is_none());
        assert!(object.instances.is_none());
        assert!(object.instance_count.is_none());
        assert!(object.field.is_none());
    }

    #[test]
    fn wired_mesh_and_maps_forward_their_slots_onto_the_object() {
        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Object, None, (0, 0));
        let mesh_slot = backend.acquire(
            ResourceId(1),
            PortType::Array(ArrayType::of_known::<MeshVertex>()),
            None,
            (0, 0),
        );
        let color_slot = backend.acquire(ResourceId(2), PortType::Texture2D, None, (0, 0));
        let instances_slot = backend.acquire(
            ResourceId(3),
            PortType::Array(ArrayType::of_known::<InstanceTransform>()),
            None,
            (0, 0),
        );
        let topology_slot = backend.acquire(
            ResourceId(4),
            PortType::Array(ArrayType::of_known::<manifold_node_engine::mesh::Vec4Vertex>()),
            None,
            (0, 0),
        );
        let count_slot = backend.acquire(
            ResourceId(5),
            PortType::Scalar(manifold_node_engine::ports::ScalarType::F32),
            None,
            (0, 0),
        );
        backend.set_scalar(count_slot, ParamValue::Float(17.0));
        let mut generations = vec![0; topology_slot.0 as usize + 1];
        generations[topology_slot.0 as usize] = 7;

        let mut params = ParamValues::default();
        params.insert(
            std::borrow::Cow::Borrowed("visible"),
            ParamValue::Float(0.0),
        );

        let mut prim = SceneObjectNode::new();
        let inputs_bindings: &[(&'static str, Slot)] = &[
            ("vertices", mesh_slot),
            ("base_color_map", color_slot),
            ("instances", instances_slot),
            ("instance_count", count_slot),
            ("topology", topology_slot),
        ];
        let outputs_bindings: &[(&'static str, Slot)] = &[("object", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut render_mode_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let inputs = NodeInputs::new(inputs_bindings, &backend, &generations);
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut render_mode_scratch,
            &mut object_scratch,
        );
        let time = frame_time();
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);

        for (slot, value) in object_scratch.drain(..) {
            backend.set_object(slot, value);
        }

        let object = backend.object(out_slot).expect("object should be set");
        assert!(!object.visible, "visible param was set to 0.0");
        assert_eq!(object.mesh, Some(mesh_slot));
        assert_eq!(object.base_color_map, Some(color_slot));
        assert_eq!(object.instances, Some(instances_slot));
        assert_eq!(object.instance_count, Some(17.0));
        assert_eq!(object.topology, Some(topology_slot));
        assert_eq!(object.normal_map, None);
    }

    /// Runs the node once with `inputs_bindings` wired and returns the object
    /// it emitted. Node errors land in `errors`.
    fn run_object(
        backend: &mut MockBackend,
        out_slot: Slot,
        inputs_bindings: &[(&'static str, Slot)],
        errors: &mut Vec<String>,
    ) -> SceneObject {
        let mut params = ParamValues::default();
        params.insert(
            std::borrow::Cow::Borrowed("visible"),
            ParamValue::Float(1.0),
        );
        let mut prim = SceneObjectNode::new();
        let generations = vec![0; 32];
        let outputs_bindings: &[(&'static str, Slot)] = &[("object", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut render_mode_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        {
            let inputs = NodeInputs::new(inputs_bindings, &*backend, &generations);
            let outputs = NodeOutputs::new(
                outputs_bindings,
                &*backend,
                &mut scalar_scratch,
                &mut camera_scratch,
                &mut light_scratch,
                &mut material_scratch,
                &mut transform_scratch,
                &mut atmosphere_scratch,
                &mut render_mode_scratch,
                &mut object_scratch,
            );
            let mut ctx = EffectNodeContext::new(frame_time(), &params, inputs, outputs, None)
                .with_errors(errors);
            Primitive::run(&mut prim, &mut ctx);
        }
        for (slot, value) in object_scratch.drain(..) {
            backend.set_object(slot, value);
        }
        backend.object(out_slot).expect("object should be set")
    }

    /// Acquires one `f32` scalar slot per `(port, value)` and returns the
    /// port bindings for them.
    fn wire_scalars(
        backend: &mut MockBackend,
        first_resource: u32,
        scalars: &[(&'static str, f32)],
    ) -> Vec<(&'static str, Slot)> {
        scalars
            .iter()
            .enumerate()
            .map(|(i, &(name, value))| {
                let slot = backend.acquire(
                    ResourceId(first_resource + i as u32),
                    PortType::Scalar(manifold_node_engine::ports::ScalarType::F32),
                    None,
                    (0, 0),
                );
                backend.set_scalar(slot, ParamValue::Float(value));
                (name, slot)
            })
            .collect()
    }

    /// The table, extent, pool size and cell size of a field, wired from
    /// resources `first_resource` on. Returns the bindings and the slots of
    /// the table and the extent.
    fn wire_table(
        backend: &mut MockBackend,
        first_resource: u32,
        pool_tiles: f32,
        cell_size: f32,
    ) -> (Vec<(&'static str, Slot)>, Slot, Slot) {
        let tiles = backend.acquire(
            ResourceId(first_resource),
            PortType::Array(ArrayType::of_known::<u32>()),
            None,
            (0, 0),
        );
        let extent = backend.acquire(
            ResourceId(first_resource + 1),
            PortType::Array(ArrayType::of_known::<f32>()),
            None,
            (0, 0),
        );
        let mut bindings = vec![("field_tiles", tiles), ("field_extent", extent)];
        bindings.extend(wire_scalars(
            backend,
            first_resource + 2,
            &[
                ("field_pool_tiles", pool_tiles),
                ("field_cell_size", cell_size),
            ],
        ));
        (bindings, tiles, extent)
    }

    fn levelset_slot(backend: &mut MockBackend) -> Slot {
        backend.acquire(
            ResourceId(1),
            PortType::Array(ArrayType::of_known::<f32>()),
            None,
            (0, 0),
        )
    }

    #[test]
    fn wired_field_forwards_its_slots_table_and_cell() {
        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Object, None, (0, 0));
        let levelset = levelset_slot(&mut backend);
        let [velocity, foam, mist, wet_map] = [2, 3, 4, 5].map(|id| {
            backend.acquire(
                ResourceId(id),
                PortType::Array(ArrayType::of_known::<f32>()),
                None,
                (0, 0),
            )
        });
        let mut bindings = vec![
            ("field", levelset),
            ("field_velocity", velocity),
            ("field_foam", foam),
            ("field_mist", mist),
            ("wet_map", wet_map),
        ];
        let (table, tiles, extent) = wire_table(&mut backend, 6, 37.0, 0.045);
        bindings.extend(table);
        bindings.extend(wire_scalars(
            &mut backend,
            10,
            &[("field_ripple_detail", 0.4)],
        ));

        let mut errors = Vec::new();
        let object = run_object(&mut backend, out_slot, &bindings, &mut errors);

        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            object.field,
            Some(FieldSurface {
                levelset,
                velocity: Some(velocity),
                foam: Some(foam),
                foam_coverage: 1.0,
                mist: Some(mist),
                wet_map: Some(wet_map),
                tiles,
                extent,
                pool_tiles: 37,
                cell_size: 0.045,
                ripple_detail: 0.4,
                caustics: 1.0,
            })
        );
        assert!(object.mesh.is_none());
    }

    #[test]
    fn unwired_ripple_detail_reads_automatic_and_a_bad_one_is_a_node_error() {
        for (ripple, want) in [
            (None, Some(1.0)),
            (Some(0.0), Some(0.0)),
            (Some(-0.5), None),
            (Some(f32::NAN), None),
        ] {
            let mut backend = MockBackend::new();
            let out_slot = backend.acquire(ResourceId(0), PortType::Object, None, (0, 0));
            let levelset = levelset_slot(&mut backend);
            let mut bindings = vec![("field", levelset)];
            let (table, _, _) = wire_table(&mut backend, 2, 3.0, 0.05);
            bindings.extend(table);
            if let Some(value) = ripple {
                bindings.extend(wire_scalars(
                    &mut backend,
                    6,
                    &[("field_ripple_detail", value)],
                ));
            }

            let mut errors = Vec::new();
            let object = run_object(&mut backend, out_slot, &bindings, &mut errors);

            match want {
                Some(strength) => {
                    assert!(errors.is_empty(), "{errors:?}");
                    assert_eq!(object.field.map(|f| f.ripple_detail), Some(strength));
                }
                None => {
                    assert!(object.field.is_none(), "ripple {ripple:?}");
                    assert_eq!(errors.len(), 1);
                }
            }
        }
    }

    #[test]
    fn field_without_its_table_is_a_node_error() {
        // Each of the four required inputs missing in turn.
        for missing in [
            "field_tiles",
            "field_extent",
            "field_pool_tiles",
            "field_cell_size",
        ] {
            let mut backend = MockBackend::new();
            let out_slot = backend.acquire(ResourceId(0), PortType::Object, None, (0, 0));
            let levelset = levelset_slot(&mut backend);
            let mut bindings = vec![("field", levelset)];
            let (table, _, _) = wire_table(&mut backend, 2, 3.0, 0.05);
            bindings.extend(table.into_iter().filter(|(port, _)| *port != missing));

            let mut errors = Vec::new();
            let object = run_object(&mut backend, out_slot, &bindings, &mut errors);

            assert!(object.field.is_none(), "{missing} missing");
            assert_eq!(errors.len(), 1, "{missing} missing");
        }
    }

    #[test]
    fn a_hidden_object_reads_no_field() {
        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Object, None, (0, 0));
        let levelset = backend.acquire(
            ResourceId(1),
            PortType::Array(ArrayType::of_known::<f32>()),
            None,
            (0, 0),
        );
        let mut bindings = vec![("field", levelset)];
        // No table at all: a visible object would report it.
        bindings.extend(wire_scalars(&mut backend, 2, &[("visible", 0.0)]));

        let mut errors = Vec::new();
        let object = run_object(&mut backend, out_slot, &bindings, &mut errors);

        assert!(errors.is_empty(), "{errors:?}");
        assert!(object.field.is_none());
        assert!(!object.visible);
    }

    #[test]
    fn a_pool_under_one_tile_or_a_cell_without_size_is_a_node_error() {
        for (pool_tiles, cell_size) in [
            (0.0, 0.05),
            (0.4, 0.05),
            (f32::NAN, 0.05),
            (3.0, 0.0),
            (3.0, -0.05),
            (3.0, f32::NAN),
            (3.0, f32::INFINITY),
        ] {
            let mut backend = MockBackend::new();
            let out_slot = backend.acquire(ResourceId(0), PortType::Object, None, (0, 0));
            let levelset = levelset_slot(&mut backend);
            let mut bindings = vec![("field", levelset)];
            let (table, _, _) = wire_table(&mut backend, 2, pool_tiles, cell_size);
            bindings.extend(table);

            let mut errors = Vec::new();
            let object = run_object(&mut backend, out_slot, &bindings, &mut errors);

            assert!(
                object.field.is_none(),
                "pool {pool_tiles}, cell {cell_size}"
            );
            assert_eq!(errors.len(), 1, "pool {pool_tiles}, cell {cell_size}");
        }
    }
}

/// Real-GPU end-to-end wiring proof: `mesh_src → node.scene_object →
/// consumer` through the real [`Executor`](crate::node_graph::execution::Executor) +
/// [`MetalBackend`] path, per the P1 gate ("synthetic def wiring
/// mesh→scene_object→(test consumer via `inputs.object`), asserts the
/// struct arrives with correct slots and the mesh resolves"). No `wgsl_body`
/// exists to parity-test here (CPU-only bridge, per the module doc) — the
/// thing under test is the wiring itself: does a real `Array<MeshVertex>`
/// buffer, pre-bound to `mesh_src`'s output resource, actually arrive at the
/// consumer through the `SceneObject`'s forwarded `Slot`.
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use std::sync::{Arc, Mutex};

    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;
    use manifold_node_engine::exec::effect_node::EffectNode;
    use manifold_node_engine::exec::effect_node::{EffectNodeContext, EffectNodeType, FrameTime};
    use manifold_node_engine::exec::execution::Executor;
    use manifold_node_engine::exec::execution_plan::compile;
    use manifold_node_engine::exec::metal_backend::MetalBackend;
    use manifold_node_engine::graph::Graph;
    use manifold_node_engine::parameters::ParamDef;
    use manifold_node_engine::ports::{
        ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType,
    };

    use super::*;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn mk_vertex(pos: [f32; 3]) -> MeshVertex {
        MeshVertex {
            position: pos,
            _pad0: 0.0,
            normal: [0.0, 1.0, 0.0],
            _pad1: 0.0,
            uv: [0.0, 0.0],
            _pad2: [0.0, 0.0],
            tangent: [0.0; 4],
            color: [1.0; 4],
        }
    }

    /// Test-only mesh producer: its `out` resource is pre-bound directly to
    /// a real GPU buffer before the frame runs (mirroring
    /// `bindings.rs::array_accessor_tests`), so `evaluate` itself is a
    /// no-op — the test is about the WIRING downstream of the pre-bound
    /// resource, not about dispatching a mesh-generation kernel.
    struct MeshSourceNode {
        type_id: EffectNodeType,
        /// Published on `out` when set, as a GPU-counted producer does.
        live: Option<manifold_node_engine::scene::live_extent::LiveExtent>,
    }

    impl EffectNode for MeshSourceNode {
        fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
            manifold_node_engine::scene::depth_rule::DepthRule::Terminal
        } // test fixture
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &[]
        }
        fn outputs(&self) -> &[NodeOutput] {
            static OUTPUTS: [NodePort; 1] = [NodePort {
                name: Cow::Borrowed("out"),
                ty: PortType::Array(ArrayType::of_known::<MeshVertex>()),
                kind: PortKind::Output,
                required: false,
            }];
            &OUTPUTS
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            if let Some(live) = &self.live {
                ctx.outputs.set_live_extent("out", live.clone());
            }
        }
    }

    /// Test consumer: declares an `Object` input (standing in for
    /// `render_scene`'s future `object_k` port), records the resolved
    /// [`SceneObject`] plus the mesh vertices read back through its
    /// forwarded `Slot`.
    struct ObjectConsumerNode {
        type_id: EffectNodeType,
        seen: Arc<Mutex<Option<SceneObject>>>,
        resolved_mesh: Arc<Mutex<Option<Vec<[f32; 3]>>>>,
        /// (count word offset, per item, bound) of the mesh slot's live extent.
        live: Arc<Mutex<Option<(u64, u32, u32)>>>,
    }

    impl EffectNode for ObjectConsumerNode {
        fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
            manifold_node_engine::scene::depth_rule::DepthRule::Terminal
        } // test fixture
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodePort; 1] = [NodePort {
                name: Cow::Borrowed("in"),
                ty: PortType::Object,
                kind: PortKind::Input,
                required: false,
            }];
            &INPUTS
        }
        fn outputs(&self) -> &[NodeOutput] {
            &[]
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            let Some(object) = ctx.inputs.object("in") else {
                return;
            };
            *self.seen.lock().unwrap() = Some(object);
            *self.live.lock().unwrap() = object
                .mesh
                .and_then(|slot| ctx.inputs.live_extent_slot(slot))
                .map(|extent| (extent.offset, extent.per_item, extent.bound));
            if let Some(mesh_slot) = object.mesh
                && let Some(buf) = ctx.inputs.array_slot(mesh_slot)
            {
                let ptr = buf.mapped_ptr().expect("shared mesh buffer") as *const MeshVertex;
                let count = buf.size as usize / std::mem::size_of::<MeshVertex>();
                let verts = unsafe { std::slice::from_raw_parts(ptr, count) };
                *self.resolved_mesh.lock().unwrap() =
                    Some(verts.iter().map(|v| v.position).collect());
            }
        }
    }

    #[test]
    fn object_wire_carries_real_mesh_slot_end_to_end() {
        let device = manifold_gpu::testkit::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let mesh_src = g.add_node(Box::new(MeshSourceNode {
            type_id: EffectNodeType::new("mesh_src"),
            live: None,
        }));
        let scene_object = g.add_node(Box::new(SceneObjectNode::new()));
        let seen = Arc::new(Mutex::new(None));
        let resolved_mesh = Arc::new(Mutex::new(None));
        let consumer = g.add_node(Box::new(ObjectConsumerNode {
            type_id: EffectNodeType::new("object_consumer"),
            seen: seen.clone(),
            resolved_mesh: resolved_mesh.clone(),
            live: Arc::new(Mutex::new(None)),
        }));
        g.connect((mesh_src, "out"), (scene_object, "vertices"))
            .unwrap();
        g.connect((scene_object, "object"), (consumer, "in"))
            .unwrap();

        let plan = compile(&g).unwrap();
        let r_mesh_out = plan
            .steps()
            .iter()
            .find(|s| s.node == mesh_src)
            .and_then(|s| s.outputs.iter().find(|(n, _)| *n == "out"))
            .map(|&(_, r)| r)
            .expect("mesh_src's out resource is bound (scene_object reads it)");

        let expected = vec![
            mk_vertex([1.0, 2.0, 3.0]),
            mk_vertex([-1.0, 0.5, 4.0]),
            mk_vertex([0.0, 0.0, 0.0]),
        ];
        let buffer = device.create_buffer(
            std::mem::size_of_val(expected.as_slice()) as u64,
            manifold_gpu::BufferUse::Upload,
        );
        unsafe {
            buffer.write(0, bytemuck::cast_slice(&expected));
        }

        let mut backend = MetalBackend::new(device.arc(), 16, 16, format);
        backend.pre_bind_array(r_mesh_out, buffer);

        let mut exec = Executor::new(Box::new(backend));
        exec.execute_frame(&mut g, &plan, frame_time());

        let object = seen
            .lock()
            .unwrap()
            .expect("consumer should have seen a SceneObject");
        assert!(
            object.mesh.is_some(),
            "SceneObject must carry the forwarded mesh Slot"
        );
        assert!(object.visible, "visible defaults to on (param default 1.0)");

        let resolved = resolved_mesh
            .lock()
            .unwrap()
            .clone()
            .expect("mesh must resolve through the forwarded slot");
        let expected_positions: Vec<[f32; 3]> = expected.iter().map(|v| v.position).collect();
        assert_eq!(
            resolved, expected_positions,
            "mesh vertices must round-trip through the Object wire's forwarded Slot"
        );
    }

    /// A producer's published live extent
    /// reaches the object's consumer through the forwarded mesh slot, and a
    /// producer that publishes none leaves the whole array live.
    #[test]
    fn object_wire_carries_the_mesh_live_extent() {
        let device = manifold_gpu::testkit::test_device();
        for publish in [true, false] {
            let mut g = Graph::new();
            let live = publish.then(|| manifold_node_engine::scene::live_extent::LiveExtent {
                counts: device.create_buffer(16, manifold_gpu::BufferUse::Upload),
                offset: 8,
                per_item: 3,
                bound: 6,
            });
            let mesh_src = g.add_node(Box::new(MeshSourceNode {
                type_id: EffectNodeType::new("mesh_src"),
                live,
            }));
            let scene_object = g.add_node(Box::new(SceneObjectNode::new()));
            let seen_live = Arc::new(Mutex::new(None));
            let consumer = g.add_node(Box::new(ObjectConsumerNode {
                type_id: EffectNodeType::new("object_consumer"),
                seen: Arc::new(Mutex::new(None)),
                resolved_mesh: Arc::new(Mutex::new(None)),
                live: seen_live.clone(),
            }));
            g.connect((mesh_src, "out"), (scene_object, "vertices"))
                .unwrap();
            g.connect((scene_object, "object"), (consumer, "in"))
                .unwrap();
            let plan = compile(&g).unwrap();
            let r_mesh_out = plan
                .steps()
                .iter()
                .find(|s| s.node == mesh_src)
                .and_then(|s| s.outputs.iter().find(|(n, _)| *n == "out"))
                .map(|&(_, r)| r)
                .expect("mesh_src's out resource is bound");
            let mut backend =
                MetalBackend::new(device.arc(), 16, 16, GpuTextureFormat::Rgba16Float);
            backend.pre_bind_array(
                r_mesh_out,
                device.create_buffer(
                    9 * std::mem::size_of::<MeshVertex>() as u64,
                    manifold_gpu::BufferUse::Upload,
                ),
            );
            let mut exec = Executor::new(Box::new(backend));
            exec.execute_frame(&mut g, &plan, frame_time());
            let expected = publish.then_some((8, 3, 6));
            assert_eq!(*seen_live.lock().unwrap(), expected, "publish={publish}");
        }
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
