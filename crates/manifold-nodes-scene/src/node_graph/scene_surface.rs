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

use manifold_core::liquid_domain::{
    GPU_FLIP_DOMAIN_TYPE_ID, is_liquid_domain, liquid_dial_params,
};
use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{
    CardBand, CardLabel, ControlGroup, Feature, Gate, RgbChannel, SceneParamDesc, SurfaceRole, Unit,
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
        _ => return None,
    })
}

fn physics_world(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::PhysicsWorld;
    Some(match param {
        "copy_count" => card(PhysicsWorld, CardBand::Scene, 0),
        "reset" => card(PhysicsWorld, CardBand::Scene, 1),
        "gravity_x" | "gravity_y" | "gravity_z" => {
            setup(PhysicsWorld).unit(Unit::MetresPerSecondSquared).limit(HardLimit::Unlimited)
        }
        "copy_spacing" => setup(PhysicsWorld).unit(Unit::Metres).limit(AT_LEAST_ZERO),
        "speed" => setup(PhysicsWorld).limit(AT_LEAST_ZERO),
        "copy_columns" | "copy_layout" => setup(PhysicsWorld),
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

fn light(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::{Light, Shadows};
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
        "mode" | "falloff" | "inner_cone_angle" | "outer_cone_angle" => setup(Light),
        "cast_shadows" | "shadow_softness" | "shadow_bias" | "shadow_resolution" => {
            setup(Shadows)
        }
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
fn camera_lens(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::{DepthOfField, Lens, MotionBlur};
    Some(match param {
        // Focus sits after every camera type's own rows.
        "focus_distance" => card(DepthOfField, CardBand::Camera, 10)
            .gate(Gate::Dim(Feature::DepthOfField))
            .unit(Unit::Metres)
            .limit(AT_LEAST_ZERO),
        "f_stop" => card(DepthOfField, CardBand::Look, 1)
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
        "max_radius" => card(DepthOfField, CardBand::Look, 2)
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

fn bake_environment(param: &str) -> Option<SceneParamFacts> {
    use ControlGroup::Environment;
    Some(match param {
        "intensity" => card(Environment, CardBand::Lighting, 1)
            .label(CardLabel::GroupName)
            .limit(AT_LEAST_ZERO),
        "width" | "height" | "horizon_strength" | "azimuth_variation" | "mode"
        | "emitter_count" | "emitter_intensity" | "emitter_elevation" | "emitter_width"
        | "sun_x" | "sun_y" | "sun_z" | "sun_disc_intensity" | "sun_disc_size" | "fill" => {
            setup(Environment)
        }
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
    use ControlGroup::Whitewater;
    Some(match param {
        "enabled" | "amount" | "wavecrest_emission" | "turbulence_emission" | "min_turbulence"
        | "max_turbulence" | "inside_emission" | "dust_emission" | "boundary_dust" | "dust_rate"
        | "spray_speed" | "generation_rate" | "influence_base" | "influence_decay" => {
            setup(Whitewater)
        }
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
        assert!(orbit[4].1 < order("node.camera_lens", "focus_distance").1);
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
}
