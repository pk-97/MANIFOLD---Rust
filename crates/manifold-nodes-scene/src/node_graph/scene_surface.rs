//! The scene table: where every param of every scene-vocabulary node lives on
//! the scene surfaces (SCENE_SURFACES_DESIGN.md section 3.3 (the scene
//! table)). Pure data, read by the node param source so the live lookup and
//! the stamp see it; no reader goes around them.
//!
//! The table never carries a label. A label is declared once, on the
//! primitive's `ParamDef` (D9 (one word list)); `CardLabel` only says when
//! the card shows an item or group name instead.
//!
//! Hidden params are never stamped. Limits are declared here and enforced
//! from P8 (D15 (slider range and hard limit)); a limit is in the param's
//! storage unit (radians for angles).

use manifold_core::fluid_domain::DOMAIN_EXTENT_METRES;
use manifold_core::liquid_domain::{
    GPU_FLIP_DOMAIN_TYPE_ID, is_liquid_domain, liquid_dial_params,
};
use std::borrow::Cow;

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{
    CardBand, CardLabel, ControlGroup, Feature, Gate, Modes, ParamTriple, RgbChannel, SceneParamDesc,
    SceneShape, SceneShapeDecl, SurfaceRole, Unit,
};

use crate::node_graph::material_inspector::material_param_role;

/// What the scene table declares about one node param.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneParamFacts {
    pub desc: SceneParamDesc,
    pub unit: Option<Unit>,
    pub limit: HardLimit,
}

const fn role(role: SurfaceRole, group: ControlGroup) -> SceneParamFacts {
    SceneParamFacts {
        desc: SceneParamDesc {
            role,
            group,
            band: None,
            card_order: 0,
            card_label: CardLabel::Param,
            gate: None,
            switch: None,
            colour: None,
        },
        unit: None,
        limit: HardLimit::Slider,
    }
}

const fn setup(group: ControlGroup) -> SceneParamFacts {
    role(SurfaceRole::Setup, group)
}

const fn hidden(group: ControlGroup) -> SceneParamFacts {
    role(SurfaceRole::Hidden, group)
}

/// A performance row in `band`, at `order` within its item.
const fn card(group: ControlGroup, band: CardBand, order: u8) -> SceneParamFacts {
    let mut facts = role(SurfaceRole::Performance, group);
    facts.desc.band = Some(band);
    facts.desc.card_order = order;
    facts
}

impl SceneParamFacts {
    const fn unit(mut self, unit: Unit) -> Self {
        self.unit = Some(unit);
        self
    }

    const fn limit(mut self, limit: HardLimit) -> Self {
        self.limit = limit;
        self
    }

    const fn label(mut self, label: CardLabel) -> Self {
        self.desc.card_label = label;
        self
    }

    const fn gate(mut self, gate: Gate) -> Self {
        self.desc.gate = Some(gate);
        self
    }

    const fn switch(mut self, feature: Feature) -> Self {
        self.desc.switch = Some(feature);
        self
    }

    const fn colour(mut self, channel: RgbChannel) -> Self {
        self.desc.colour = Some(channel);
        self
    }
}

/// `node.light`'s `mode` values (`LIGHT_MODES`): Sun 0, Point 1, Spot 2.
const POINT_AND_SPOT: Gate = Gate::Only(Feature::LightType, Modes(0b110));
const SPOT: Gate = Gate::Only(Feature::LightType, Modes(0b100));
/// `node.bake_environment`'s `mode` values: Gradient 0, Softbox 1.
const GRADIENT: Gate = Gate::Only(Feature::EnvironmentMode, Modes(0b01));
const SOFTBOX: Gate = Gate::Only(Feature::EnvironmentMode, Modes(0b10));

const AT_LEAST_ZERO: HardLimit = HardLimit::AtLeast(0.0);
/// Field of view, 1° to 179°, in radians.
const FIELD_OF_VIEW: HardLimit = HardLimit::Between(0.017_453_292, 3.124_139_4);

/// The scene table's answer for one param of `type_id`. `None` for a type
/// outside the scene vocabulary or a param the table does not classify
/// (INV-S1 fails on the latter).
pub fn scene_param_facts(type_id: &str, param: &str) -> Option<SceneParamFacts> {
    match type_id {
        "node.rigid_body" => rigid_body(param),
        "node.physics_world" => physics_world(param),
        "node.fluid_role_source" => fluid_role_source(param),
        "node.transform_3d" => transform(param),
        "node.pbr_material" => material_param_role(type_id, param).map(|_| match param {
            "iridescence_thickness_min" | "iridescence_thickness_max" => {
                setup(ControlGroup::Material).unit(Unit::Nanometres)
            }
            _ => setup(ControlGroup::Material),
        }),
        "node.unlit_material" => unlit_material(param),
        "node.cel_material" => cel_material(param),
        "node.light" => light(param),
        "node.orbit_camera" => orbit_camera(param),
        "node.free_camera" => free_camera(param),
        "node.look_at_camera" => look_at_camera(param),
        "node.camera_lens" => camera_lens(param),
        "node.bokeh_gather" => bokeh_gather(param),
        "node.motion_blur" => motion_blur(param),
        "node.atmosphere" => atmosphere(param),
        "node.bake_environment" => bake_environment(param),
        "node.scene_object" => scene_object(param),
        "node.bend_mesh" | "node.twist_mesh" | "node.taper_mesh" | "node.push_along_normals"
        | "node.push_mesh" | "node.morph_mesh" | "node.rotate_3d" => mesh_modifier(type_id, param),
        "node.render_scene" => render_scene(param),
        "node.whitewater_step" => whitewater(param),
        domain if is_liquid_domain(domain) => liquid_domain(domain, param),
        _ => None,
    }
}

const fn id(name: &'static str) -> Cow<'static, str> {
    Cow::Borrowed(name)
}

const POS: ParamTriple = [id("pos_x"), id("pos_y"), id("pos_z")];
const ROT: ParamTriple = [id("rot_x"), id("rot_y"), id("rot_z")];
const SCALE: ParamTriple = [id("scale_x"), id("scale_y"), id("scale_z")];

/// A `node.transform_3d` box behind `port`: placed, turned and sized by
/// the transform's own params.
const fn transform_box(port: &'static str, rotates: bool) -> SceneShapeDecl {
    SceneShapeDecl {
        shape: SceneShape::Box { center: POS, rotation: if rotates { Some(ROT) } else { None }, size: SCALE },
        via: Some(id(port)),
        extent_limit: None,
    }
}

static SCENE_OBJECT_SHAPES: [SceneShapeDecl; 1] = [transform_box("transform", true)];
/// The liquid domain box: each side stays inside what the solvers accept.
const DOMAIN_BOX: SceneShapeDecl = {
    let mut decl = transform_box("domain", false);
    decl.extent_limit = Some(HardLimit::Between(DOMAIN_EXTENT_METRES.0, DOMAIN_EXTENT_METRES.1));
    decl
};
/// The solvers take axis-aligned boxes only, so neither box rotates. The
/// Initial Fill height is a level above the domain floor, not a world
/// value, so it has no handle yet.
static LIQUID_DOMAIN_SHAPES: [SceneShapeDecl; 2] = [DOMAIN_BOX, transform_box("initial_volume", false)];
/// The `transform` input places the role's volume.
static FLUID_ROLE_SOURCE_SHAPES: [SceneShapeDecl; 1] = [transform_box("transform", true)];

/// Every shape `type_id` declares, in order (PHYSICS_ENGINE_BOUNDARY_DESIGN.md
/// section 5.2 (One scene-object model)). A gizmo target is one entry of
/// this list on one node.
pub fn scene_shape(type_id: &str) -> &'static [SceneShapeDecl] {
    match type_id {
        "node.scene_object" => &SCENE_OBJECT_SHAPES,
        "node.fluid_role_source" => &FLUID_ROLE_SOURCE_SHAPES,
        domain if is_liquid_domain(domain) => &LIQUID_DOMAIN_SHAPES,
        _ => &[],
    }
}

/// The placement an author gives a control that drives several nodes: the
/// one its first target would have alone. A fan-out keeps its own scene
/// facts (SCENE_SURFACES_DESIGN.md D23 (scene facts follow the bound node)),
/// so the importer writes this into the control when it builds it.
pub(crate) fn fan_out_scene(type_id: &str, param: &str) -> Option<SceneParamDesc> {
    scene_param_facts(type_id, param).map(|facts| facts.desc)
}

fn rigid_body(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::Body;
    Some(match param {
        "shape" | "motion" | "friction" | "bounce" | "collider_parts" => setup(Body),
        "density" => setup(Body).unit(Unit::KilogramsPerCubicMetre),
        "enabled" | "release_count" | "path" | "mesh_index" | "primitive_index"
        | "material_index" | "fit" | "recenter" | "translate_x" | "translate_y" | "translate_z"
        | "fragment_count" | "fragment_index" | "fragment_parent" | "compound_materials" => {
            hidden(Body)
        }
        // The box spawner. Only a copies body stamps these
        // (`copies_body::is_copies_body_param`), and the copies body belongs
        // to its physics world's item, so they sit with the world's controls.
        "copy_count" => card(ControlGroup::PhysicsWorld, CardBand::Scene, 0),
        "copy_spacing" => setup(ControlGroup::PhysicsWorld).unit(Unit::Metres).limit(AT_LEAST_ZERO),
        "copy_columns" | "copy_layout" => setup(ControlGroup::PhysicsWorld),
        _ => return None,
    })
}

fn physics_world(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::PhysicsWorld;
    Some(match param {
        "reset" => card(PhysicsWorld, CardBand::Scene, 1),
        "gravity_x" | "gravity_y" | "gravity_z" => {
            setup(PhysicsWorld).unit(Unit::MetresPerSecondSquared).limit(HardLimit::Unlimited)
        }
        "speed" => setup(PhysicsWorld).limit(AT_LEAST_ZERO),
        _ => return None,
    })
}

fn fluid_role_source(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::Source;
    Some(match param {
        "enabled" => card(Source, CardBand::Scene, 0),
        "velocity_x" => card(Source, CardBand::Scene, 1).unit(Unit::MetresPerSecond),
        "velocity_y" => card(Source, CardBand::Scene, 2).unit(Unit::MetresPerSecond),
        "velocity_z" => card(Source, CardBand::Scene, 3).unit(Unit::MetresPerSecond),
        "role" | "geometry" | "shape" | "inherit_motion" | "friction" | "collider_parts" => {
            setup(Source)
        }
        "radius" => setup(Source).unit(Unit::Metres).limit(AT_LEAST_ZERO),
        "path" | "compound_materials" | "mesh_index" | "primitive_index" | "material_index"
        | "fit" | "recenter" | "translate_x" | "translate_y" | "translate_z" | "fragment_count"
        | "fragment_index" => hidden(Source),
        _ => return None,
    })
}

fn transform(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::Transform;
    Some(match param {
        "pos_x" | "pos_y" | "pos_z" => {
            setup(Transform).unit(Unit::Metres).limit(HardLimit::Unlimited)
        }
        "rot_x" | "rot_y" | "rot_z" => setup(Transform).limit(HardLimit::Unlimited),
        "scale_x" | "scale_y" | "scale_z" | "billboard" => setup(Transform),
        _ => return None,
    })
}

fn unlit_material(param: &str) -> Option<SceneParamFacts> {
    matches!(
        param,
        "color_r" | "color_g" | "color_b" | "color_a" | "emission_r" | "emission_g"
            | "emission_b" | "emission_intensity" | "alpha_mode" | "alpha_cutoff" | "uv_m00"
            | "uv_m01" | "uv_m10" | "uv_m11" | "uv_tx" | "uv_ty" | "uv_set" | "wrap_u"
            | "wrap_v" | "mag_filter" | "min_filter" | "mip_filter" | "em_uv_m00" | "em_uv_m01"
            | "em_uv_m10" | "em_uv_m11" | "em_uv_tx" | "em_uv_ty" | "em_uv_set" | "em_wrap_u"
            | "em_wrap_v" | "em_mag_filter" | "em_min_filter" | "em_mip_filter"
    )
    .then_some(setup(ControlGroup::Material))
}

fn cel_material(param: &str) -> Option<SceneParamFacts> {
    matches!(
        param,
        "color_r" | "color_g" | "color_b" | "color_a" | "cel_bands" | "band_low" | "band_high"
            | "emission_r" | "emission_g" | "emission_b" | "emission_intensity" | "alpha_mode"
            | "alpha_cutoff"
    )
    .then_some(setup(ControlGroup::Material))
}

/// Gates follow what `LightNode::run` passes each type: only Spot gets the
/// cone angles, and only Point and Spot get the falloff. Sun reads `range`
/// as its shadow frustum's half-extent, so `range` is never gated.
fn light(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::{Light, Shadows, ShadowsAdvanced};
    Some(match param {
        // One fader per light, named after the light, after Exposure and
        // Environment (D4).
        "intensity" => {
            card(Light, CardBand::Lighting, 2).label(CardLabel::ItemName).limit(AT_LEAST_ZERO)
        }
        "pos_x" | "pos_y" | "pos_z" | "aim_x" | "aim_y" | "aim_z" => {
            setup(Light).unit(Unit::Metres).limit(HardLimit::Unlimited)
        }
        "range" | "light_size" => setup(Light).unit(Unit::Metres).limit(AT_LEAST_ZERO),
        "color_r" => setup(Light).colour(RgbChannel::R),
        "color_g" => setup(Light).colour(RgbChannel::G),
        "color_b" => setup(Light).colour(RgbChannel::B),
        "mode" => setup(Light).switch(Feature::LightType),
        "falloff" => setup(Light).gate(POINT_AND_SPOT),
        "inner_cone_angle" | "outer_cone_angle" => setup(Light).gate(SPOT),
        "cast_shadows" | "shadow_softness" => setup(Shadows),
        "shadow_bias" | "shadow_resolution" => setup(ShadowsAdvanced),
        _ => return None,
    })
}

fn orbit_camera(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::Camera;
    Some(match param {
        "orbit" => card(Camera, CardBand::Camera, 0).limit(HardLimit::Unlimited),
        "tilt" => card(Camera, CardBand::Camera, 1),
        "distance" => card(Camera, CardBand::Camera, 2).unit(Unit::Metres).limit(AT_LEAST_ZERO),
        "look_y" => card(Camera, CardBand::Camera, 3).unit(Unit::Metres).limit(HardLimit::Unlimited),
        "fov_y" => card(Camera, CardBand::Camera, 4).limit(FIELD_OF_VIEW),
        "roll" => setup(Camera).limit(HardLimit::Unlimited),
        "near" | "far" => setup(Camera).unit(Unit::Metres).limit(AT_LEAST_ZERO),
        _ => return None,
    })
}

fn free_camera(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::Camera;
    let position = |order| card(Camera, CardBand::Camera, order).unit(Unit::Metres).limit(HardLimit::Unlimited);
    Some(match param {
        "yaw" => card(Camera, CardBand::Camera, 0).limit(HardLimit::Unlimited),
        "pitch" => card(Camera, CardBand::Camera, 1),
        "pos_x" => position(2),
        "pos_y" => position(3),
        "pos_z" => position(4),
        "fov_y" => card(Camera, CardBand::Camera, 5).limit(FIELD_OF_VIEW),
        "roll" => setup(Camera).limit(HardLimit::Unlimited),
        "near" | "far" => setup(Camera).unit(Unit::Metres).limit(AT_LEAST_ZERO),
        _ => return None,
    })
}

fn look_at_camera(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::Camera;
    Some(match param {
        "fov_y" => card(Camera, CardBand::Camera, 0).limit(FIELD_OF_VIEW),
        "pos_x" | "pos_y" | "pos_z" | "target_x" | "target_y" | "target_z" => {
            setup(Camera).unit(Unit::Metres).limit(HardLimit::Unlimited)
        }
        "near" | "far" => setup(Camera).unit(Unit::Metres).limit(AT_LEAST_ZERO),
        _ => return None,
    })
}

/// Look band order: depth of field rows 0..10, motion blur rows from 10.
/// Each switch leads the rows it controls.
fn camera_lens(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::{DepthOfField, Lens, MotionBlur};
    Some(match param {
        "focus_distance" => card(DepthOfField, CardBand::Look, 1)
            .gate(Gate::Dim(Feature::DepthOfField))
            .unit(Unit::Metres)
            .limit(AT_LEAST_ZERO),
        "f_stop" => card(DepthOfField, CardBand::Look, 2)
            .gate(Gate::Fold(Feature::DepthOfField))
            .limit(HardLimit::AtLeast(0.5)),
        "shutter_angle" => card(MotionBlur, CardBand::Look, 11)
            .gate(Gate::Fold(Feature::MotionBlur))
            .limit(HardLimit::Between(0.0, 360.0)),
        "exposure_ev" => card(Lens, CardBand::Lighting, 0).unit(Unit::Ev).limit(HardLimit::Unlimited),
        _ => return None,
    })
}

fn bokeh_gather(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::DepthOfField;
    Some(match param {
        "enabled" => card(DepthOfField, CardBand::Look, 0).switch(Feature::DepthOfField),
        "aperture" | "quality" => setup(DepthOfField),
        // D4's depth of field Max Blur, shown while depth of field is on.
        "max_radius" => card(DepthOfField, CardBand::Look, 3)
            .gate(Gate::Fold(Feature::DepthOfField))
            .unit(Unit::Pixels)
            .limit(AT_LEAST_ZERO),
        "blur_alpha" => hidden(DepthOfField),
        _ => return None,
    })
}

fn motion_blur(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::MotionBlur;
    Some(match param {
        "enabled" => card(MotionBlur, CardBand::Look, 10).switch(Feature::MotionBlur),
        "max_blur_px" => card(MotionBlur, CardBand::Look, 12)
            .gate(Gate::Fold(Feature::MotionBlur))
            .unit(Unit::Pixels)
            .limit(AT_LEAST_ZERO),
        _ => return None,
    })
}

fn atmosphere(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::Fog;
    Some(match param {
        "fog_density" | "height_falloff" | "shaft_intensity" => setup(Fog).limit(AT_LEAST_ZERO),
        "fog_color_r" | "fog_color_g" | "fog_color_b" | "ambient_tint_r" | "ambient_tint_g"
        | "ambient_tint_b" | "shaft_anisotropy" | "shaft_quality" => setup(Fog),
        _ => return None,
    })
}

/// Gates follow `bake_equirect_envmap.wgsl`: the gradient branch reads only
/// the horizon and variation, the softbox branch only the strips, sun disc
/// and fill. Size and intensity apply in both.
fn bake_environment(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::Environment;
    Some(match param {
        "intensity" => card(Environment, CardBand::Lighting, 1)
            .label(CardLabel::GroupName)
            .limit(AT_LEAST_ZERO),
        "mode" => setup(Environment).switch(Feature::EnvironmentMode),
        "width" | "height" => setup(Environment),
        "horizon_strength" | "azimuth_variation" => setup(Environment).gate(GRADIENT),
        // The disc's direction and hue come from the Sun light itself.
        "emitter_count" | "emitter_intensity" | "emitter_elevation" | "emitter_width"
        | "sun_disc_intensity" | "sun_disc_size" | "fill" => setup(Environment).gate(SOFTBOX),
        // White Furnace is a lighting test, not a look.
        "uniform" => hidden(Environment),
        _ => return None,
    })
}

fn scene_object(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::Look;
    Some(match param {
        "visible" | "cast_shadows" | "emission_strength" | "gain" => setup(Look),
        "parent_visible" => hidden(Look),
        _ => return None,
    })
}

fn mesh_modifier(type_id: &str, param: &str) -> Option<SceneParamFacts> {
    let known = match type_id {
        "node.bend_mesh" | "node.twist_mesh" => matches!(param, "axis" | "angle" | "center"),
        "node.taper_mesh" => matches!(param, "axis" | "taper" | "center" | "length"),
        "node.push_along_normals" => matches!(param, "amount" | "field_bias"),
        "node.push_mesh" => matches!(param, "cols" | "rows" | "displacement" | "height_bias"),
        "node.morph_mesh" => matches!(param, "t" | "blend_frames"),
        "node.rotate_3d" => matches!(param, "angle_x" | "angle_y" | "angle_z"),
        _ => false,
    };
    known.then_some(setup(ControlGroup::Modifier))
}

fn render_scene(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::RayTracing;
    Some(match param {
        "rt_enabled" | "temporal_upscale" | "rt_reflections" | "rt_shadows" | "rt_ao"
        | "rt_gi" => setup(RayTracing),
        "objects" | "lights" | "rt_firefly_clamp" | "rt_denoise_feed" => hidden(RayTracing),
        _ => return None,
    })
}

fn whitewater(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::{Whitewater, WhitewaterAdvanced};
    Some(match param {
        "enabled" | "amount" | "inside_emission" | "dust_emission" | "boundary_dust" => {
            setup(Whitewater)
        }
        "wavecrest_emission" | "turbulence_emission" | "min_turbulence" | "max_turbulence"
        | "dust_rate" | "spray_speed" | "generation_rate" | "influence_base"
        | "influence_decay" => setup(WhitewaterAdvanced),
        "capacity" | "min_energy" | "max_energy" | "preserve_foam" => hidden(Whitewater),
        _ => return None,
    })
}

/// A liquid domain's water-panel dials (`liquid_dial_params`) are the params
/// it shows; every other param it declares is Hidden.
fn liquid_domain(type_id: &str, param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::Simulation;
    let dial = liquid_dial_params(type_id).is_some_and(|dials| dials.contains(&param));
    // The GPU FLIP solver controls the water preset authors on its card are
    // set up once per scene: they show in the Scene tab, never on the card.
    let flip_solver = type_id == GPU_FLIP_DOMAIN_TYPE_ID
        && matches!(param, "solve_level" | "max_iterations" | "sheet_fill_rate" | "particle_capacity");
    if flip_solver {
        return Some(setup(Simulation));
    }
    if !dial {
        return None;
    }
    Some(match param {
        "speed" => card(Simulation, CardBand::Scene, 2).limit(AT_LEAST_ZERO),
        "reset" => card(Simulation, CardBand::Scene, 3),
        "cohesion" => card(Simulation, CardBand::Scene, 4).limit(AT_LEAST_ZERO),
        "stiffness" => card(Simulation, CardBand::Scene, 5).limit(AT_LEAST_ZERO),
        "liveliness" => card(Simulation, CardBand::Scene, 6),
        "domain_size" | "fill_height" => setup(Simulation).unit(Unit::Metres).limit(AT_LEAST_ZERO),
        "gravity_x" | "gravity" | "gravity_z" => {
            setup(Simulation).unit(Unit::MetresPerSecondSquared).limit(HardLimit::Unlimited)
        }
        "grid_budget_mcells" => setup(Simulation).unit(Unit::MillionCells),
        _ => setup(Simulation),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_outside_the_vocabulary_have_no_facts() {
        assert_eq!(scene_param_facts("node.platonic_solid_mesh", "radius"), None);
        assert_eq!(scene_param_facts("node.light", "not_a_param"), None);
    }

    /// The card rows D4 names come out of the table in its order.
    #[test]
    fn card_rows_follow_d4() {
        let order = |t: &str, p: &str| {
            let f = scene_param_facts(t, p).unwrap();
            assert_eq!(f.desc.role, SurfaceRole::Performance, "{t}.{p}");
            (f.desc.band.unwrap(), f.desc.card_order)
        };
        let orbit: Vec<_> = ["orbit", "tilt", "distance", "look_y", "fov_y"]
            .iter()
            .map(|p| order("node.orbit_camera", p))
            .collect();
        assert!(orbit.iter().all(|(band, _)| *band == CardBand::Camera));
        assert!(orbit.windows(2).all(|w| w[0].1 < w[1].1));
        // Each Look switch leads the rows it controls.
        let dof = ["node.bokeh_gather:enabled", "node.camera_lens:focus_distance", "node.camera_lens:f_stop", "node.bokeh_gather:max_radius"];
        let blur = ["node.motion_blur:enabled", "node.camera_lens:shutter_angle", "node.motion_blur:max_blur_px"];
        for run in [&dof[..], &blur[..]] {
            let look: Vec<_> = run.iter().map(|key| key.split_once(':').unwrap()).map(|(t, p)| order(t, p)).collect();
            assert!(look.iter().all(|(band, _)| *band == CardBand::Look), "{run:?}");
            assert!(look.windows(2).all(|w| w[0].1 < w[1].1), "{run:?}");
        }
        assert_eq!(order("node.light", "intensity").0, CardBand::Lighting);
        assert_eq!(
            scene_param_facts("node.light", "intensity").unwrap().desc.card_label,
            CardLabel::ItemName
        );
        assert_eq!(
            scene_param_facts("node.bokeh_gather", "enabled").unwrap().desc.switch,
            Some(Feature::DepthOfField)
        );
        assert_eq!(
            scene_param_facts("node.camera_lens", "f_stop").unwrap().desc.gate,
            Some(Gate::Fold(Feature::DepthOfField))
        );
    }

    /// Which modes read each gated light and environment param.
    #[test]
    fn mode_gates_follow_what_each_mode_reads() {
        let reads = |t: &str, p: &str, feature: Feature, mode: u32| match scene_param_facts(t, p).unwrap().desc.gate {
            Some(Gate::Only(gated, modes)) => {
                assert_eq!(gated, feature, "{t}.{p}");
                modes.contains(mode)
            }
            _ => true,
        };
        let light = |p: &str| [0, 1, 2].map(|mode| reads("node.light", p, Feature::LightType, mode));
        assert_eq!(scene_param_facts("node.light", "mode").unwrap().desc.switch, Some(Feature::LightType));
        assert_eq!(light("inner_cone_angle"), [false, false, true]);
        assert_eq!(light("outer_cone_angle"), [false, false, true]);
        assert_eq!(light("falloff"), [false, true, true]);
        for shared in ["range", "intensity", "pos_x", "aim_x", "cast_shadows", "shadow_bias", "light_size"] {
            assert_eq!(light(shared), [true; 3], "{shared}");
        }
        let env = |p: &str| [0, 1].map(|mode| reads("node.bake_environment", p, Feature::EnvironmentMode, mode));
        assert_eq!(
            scene_param_facts("node.bake_environment", "mode").unwrap().desc.switch,
            Some(Feature::EnvironmentMode)
        );
        for gradient in ["horizon_strength", "azimuth_variation"] {
            assert_eq!(env(gradient), [true, false], "{gradient}");
        }
        for softbox in [
            "emitter_count", "emitter_intensity", "emitter_elevation", "emitter_width", "sun_disc_intensity",
            "sun_disc_size", "fill",
        ] {
            assert_eq!(env(softbox), [false, true], "{softbox}");
        }
        for shared in ["width", "height", "intensity"] {
            assert_eq!(env(shared), [true; 2], "{shared}");
        }
    }

    /// Detail settings sit in a group of their own that starts folded.
    #[test]
    fn detail_settings_sit_under_advanced() {
        let group = |t: &str, p: &str| scene_param_facts(t, p).unwrap().desc.group;
        for p in ["shadow_bias", "shadow_resolution"] {
            assert_eq!(group("node.light", p), ControlGroup::ShadowsAdvanced, "{p}");
        }
        assert_eq!(group("node.light", "cast_shadows"), ControlGroup::Shadows);
        for p in ["enabled", "amount", "inside_emission", "dust_emission", "boundary_dust"] {
            assert_eq!(group("node.whitewater_step", p), ControlGroup::Whitewater, "{p}");
        }
        for p in [
            "wavecrest_emission", "turbulence_emission", "min_turbulence", "max_turbulence", "dust_rate",
            "spray_speed", "generation_rate", "influence_base", "influence_decay",
        ] {
            assert_eq!(group("node.whitewater_step", p), ControlGroup::WhitewaterAdvanced, "{p}");
        }
        for advanced in [ControlGroup::ShadowsAdvanced, ControlGroup::WhitewaterAdvanced] {
            assert!(advanced.title().starts_with("Advanced · "), "{advanced:?}");
        }
    }
}
