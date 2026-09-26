//! `node.render_copies` — bundled instanced 3D mesh
//! renderer. Sibling to [`render_3d_mesh`](super::render_3d_mesh):
//! same per-MaterialKind dispatch, same Material/Light/envmap input
//! shape, but applies a per-instance `pos/scale/Euler-rotation`
//! transform from an `Array<InstanceTransform>` to each instance's
//! vertices.
//!
//! Per-kind conditional requirements + magenta-fallback for missing
//! inputs match `render_3d_mesh`'s contract exactly. See the doc on
//! that primitive for the shared design rationale.

use std::borrow::Cow;

use crate::generators::mesh_common::{InstanceTransform, MeshVertex};
use crate::node_graph::effect_node::{ConditionalRequirement, EffectNodeContext};
use crate::node_graph::material::MaterialKind;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const CONDITIONAL_RULES: &[ConditionalRequirement] = &[
    ConditionalRequirement {
        on_material_kind: MaterialKind::Phong,
        required_inputs: &["light"],
    },
    ConditionalRequirement {
        on_material_kind: MaterialKind::Pbr,
        required_inputs: &["light", "envmap"],
    },
    ConditionalRequirement {
        on_material_kind: MaterialKind::Cel,
        required_inputs: &["light"],
    },
];

crate::primitive! {
    name: RenderInstanced3DMesh,
    type_id: "node.render_copies",
    purpose: "Instanced mesh adapter over the shared scene material evaluator. Draws an Array<MeshVertex> through wired Array<InstanceTransform> entries with complete material maps while retaining legacy normal and red-channel map semantics.",
    inputs: {
        vertices: Array(MeshVertex) required,
        instances: Array(InstanceTransform) required,
        camera: Camera required,
        material: Material required,
        light: Light optional,
        envmap: Texture2D optional,
        normal_map: Texture2D optional,
        roughness_map: Texture2D optional,
        base_color_map: Texture2D optional,
        metallic_map: Texture2D optional,
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
        volume_thickness_map: Texture2D optional,
        diffuse_transmission_map: Texture2D optional,
        diffuse_transmission_color_map: Texture2D optional,
    },
    outputs: {
        color: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("instance_count"),
            label: "Instance Count",
            ty: ParamType::Int,
            default: ParamValue::Float(64.0),
            range: Some((1.0, 1_000_000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Vertex count must be a multiple of 3 (trailing partial triangle truncated). instance_count is clamped to the wired instance buffer's capacity. Color uses the shared scene evaluator, including Blend and complete material map families.",
    examples: [],
    picker: { label: "Render Copies", category: Atom },
    summary: "Draws many copies of one mesh in a single pass, each placed by a list of transforms. The fast way to render a field of repeated objects.",
    category: Geometry3D,
    role: Filter,
    aliases: ["render copies", "render instanced 3d mesh", "instancing", "instances", "Geometry COMP"],
    boundary_reason: DrawCall,
    extra_fields: {
        scene_renderer: super::render_scene::RenderScene =
            super::render_scene::RenderScene::for_single_mesh(),
    },
}

impl Primitive for RenderInstanced3DMesh {
    fn conditional_requirements(&self) -> &'static [ConditionalRequirement] {
        CONDITIONAL_RULES
    }

    /// Rasterizer outputs are screen-space: always canvas-sized. The
    /// texture inputs (envmap, normal/roughness/base-color/metallic maps)
    /// are scene resources — without this declaration the plan's
    /// max-of-input-dims default would size the render target to the
    /// largest wired map instead of the canvas (BUG-140 class).
    fn output_canvas_scale(
        &self,
        _port: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        Some((1, 1))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let instance_count_param = match ctx.params.get("instance_count") {
            Some(ParamValue::Float(n)) => n.round().max(0.0) as u32,
            _ => 64,
        };

        let material = match ctx.inputs.material("material") {
            Some(m) => m,
            None => {
                ctx.error("missing required `material` input; renderer fell back to magenta clear");
                if let Some(target) = ctx.outputs.texture_2d("color") {
                    ctx.gpu_encoder()
                        .native_enc
                        .clear_texture(target, 1.0, 0.0, 1.0, 1.0);
                }
                return;
            }
        };

        let needs_light = material.requires_light();
        let needs_envmap = material.requires_envmap();
        let light_wired = ctx.inputs.light("light");
        let envmap_wired = ctx.inputs.texture_2d("envmap");
        if needs_light && light_wired.is_none() {
            ctx.error(format!(
                "{:?} material requires `light` input but it is unwired; renderer fell back to magenta",
                material.kind
            ));
            if let Some(target) = ctx.outputs.texture_2d("color") {
                ctx.gpu_encoder()
                    .native_enc
                    .clear_texture(target, 1.0, 0.0, 1.0, 1.0);
            }
            return;
        }
        if needs_envmap && envmap_wired.is_none() {
            ctx.error(format!(
                "{:?} material requires `envmap` input but it is unwired; renderer fell back to magenta",
                material.kind
            ));
            if let Some(target) = ctx.outputs.texture_2d("color") {
                ctx.gpu_encoder()
                    .native_enc
                    .clear_texture(target, 1.0, 0.0, 1.0, 1.0);
            }
            return;
        }

        let (width, height, vertex_count, instance_count) = {
            let Some(vertices) = ctx.inputs.array("vertices") else {
                return;
            };
            let Some(instances) = ctx.inputs.array("instances") else {
                return;
            };
            let Some(target) = ctx.outputs.texture_2d("color") else {
                return;
            };
            let vertex_capacity = (vertices.size / std::mem::size_of::<MeshVertex>() as u64) as u32;
            let instance_capacity =
                (instances.size / std::mem::size_of::<InstanceTransform>() as u64) as u32;
            (
                target.width,
                target.height,
                (vertex_capacity / 3) * 3,
                instance_count_param.min(instance_capacity),
            )
        };
        if width == 0 || height == 0 {
            return;
        }
        if vertex_count == 0 || instance_count == 0 {
            if let Some(target) = ctx.outputs.texture_2d("color") {
                ctx.gpu_encoder()
                    .native_enc
                    .clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            }
            return;
        }

        // The shared scene evaluator owns the color pass and consumes the
        // same identity or authored instance buffer through its object adapter.
        self.scene_renderer.render_single_mesh(ctx, Some(instance_count as f32));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::EffectNode;

    #[test]
    fn render_instanced_declares_material_required_inputs() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let instance_layout = ArrayType::of_known::<InstanceTransform>();

        assert_eq!(RenderInstanced3DMesh::TYPE_ID, "node.render_copies");
        let by_name = |n: &str| {
            RenderInstanced3DMesh::INPUTS
                .iter()
                .find(|p| p.name == n)
                .unwrap_or_else(|| panic!("missing input {n}"))
        };
        let vertices = by_name("vertices");
        assert!(vertices.required);
        assert_eq!(vertices.ty, PortType::Array(mesh_layout));
        let instances = by_name("instances");
        assert!(instances.required);
        assert_eq!(instances.ty, PortType::Array(instance_layout));
        let camera = by_name("camera");
        assert!(camera.required);
        assert_eq!(camera.ty, PortType::Camera);
        let material = by_name("material");
        assert!(material.required, "material must be REQUIRED");
        assert_eq!(material.ty, PortType::Material);
        let light = by_name("light");
        assert!(!light.required);
        assert_eq!(light.ty, PortType::Light);
        let envmap = by_name("envmap");
        assert!(!envmap.required);
        assert_eq!(envmap.ty, PortType::Texture2D);
        let normal_map = by_name("normal_map");
        assert!(!normal_map.required);
        assert_eq!(normal_map.ty, PortType::Texture2D);
        let roughness_map = by_name("roughness_map");
        assert!(!roughness_map.required);
        assert_eq!(roughness_map.ty, PortType::Texture2D);
        let base_color_map = by_name("base_color_map");
        assert!(!base_color_map.required);
        assert_eq!(base_color_map.ty, PortType::Texture2D);
        let metallic_map = by_name("metallic_map");
        assert!(!metallic_map.required);
        assert_eq!(metallic_map.ty, PortType::Texture2D);
        for name in [
            "mr_map",
            "occlusion_map",
            "emissive_map",
            "sheen_color_map",
            "sheen_roughness_map",
            "iridescence_map",
            "iridescence_thickness_map",
            "anisotropy_map",
            "clearcoat_map",
            "clearcoat_roughness_map",
            "clearcoat_normal_map",
            "specular_map",
            "specular_color_map",
            "transmission_map",
            "volume_thickness_map",
            "diffuse_transmission_map",
            "diffuse_transmission_color_map",
        ] {
            let input = by_name(name);
            assert!(!input.required, "{name} must remain optional");
            assert_eq!(input.ty, PortType::Texture2D, "{name} type");
        }
        assert_eq!(RenderInstanced3DMesh::OUTPUTS.len(), 1);
        assert_eq!(RenderInstanced3DMesh::OUTPUTS[0].name, "color");
    }

    #[test]
    fn render_instanced_3d_mesh_declares_conditional_requirements() {
        let prim = RenderInstanced3DMesh::new();
        let node: &dyn EffectNode = &prim;
        let rules = node.conditional_requirements();
        assert_eq!(rules.len(), 3);
        let by_kind = |k: MaterialKind| {
            rules
                .iter()
                .find(|r| r.on_material_kind == k)
                .unwrap_or_else(|| panic!("missing rule for {k:?}"))
        };
        assert_eq!(by_kind(MaterialKind::Phong).required_inputs, &["light"]);
        assert_eq!(
            by_kind(MaterialKind::Pbr).required_inputs,
            &["light", "envmap"]
        );
        assert_eq!(by_kind(MaterialKind::Cel).required_inputs, &["light"]);
    }

    #[test]
    fn render_instanced_has_only_instance_count_param() {
        // Scattered light/colour scalars deleted in the Material migration.
        let names: Vec<&str> = RenderInstanced3DMesh::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, vec!["instance_count"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = RenderInstanced3DMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.render_copies");
    }
}
