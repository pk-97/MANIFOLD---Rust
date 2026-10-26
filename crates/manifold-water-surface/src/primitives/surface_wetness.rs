//! `node.surface_wetness` — per field-lattice node, how wet the surface is,
//! 0 to 1. Everything the water touches is wet, and dries at the rate a thin
//! film evaporates. The node's buffer is the state: it persists between
//! frames and starts over when the display time runs backwards.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_water_liquid::float_param;

const SHADER: &str = include_str!("shaders/surface_wetness.wgsl");
const THREADS: u32 = 64;

/// The film a drained smooth surface holds, about 0.1 mm, in metres.
const WET_FILM_METRES: f32 = 1.0e-4;
/// Reference evaporation in a mild climate, 5 mm a day (FAO-56), in metres per second.
const EVAPORATION_METRES_PER_SECOND: f32 = 5.8e-8;
/// The time the film takes to evaporate at still-air rate, about 29 minutes.
const DRY_SECONDS: f32 = WET_FILM_METRES / EVAPORATION_METRES_PER_SECOND;

/// Field order identical to the shader's `Params`; one 16-byte row.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct WetnessUniforms {
    h: f32,
    decay: f32,
    node_count: u32,
    pad: u32,
}

/// The shader's constants from the node's inputs. `nodes` are the field
/// lattice's node counts as wired (whole numbers, at least 2 per axis); a node
/// is wet within one smallest node spacing of the water. `elapsed` is the
/// seconds since the last run, or `None` when the node starts over (nothing
/// carries, so only the current contact is wet).
fn wetness_uniforms(
    center: [f32; 3],
    size: [f32; 3],
    nodes: [f32; 3],
    time: f32,
    drying: f32,
    elapsed: Option<f32>,
) -> Result<(WetnessUniforms, [u32; 3]), String> {
    if nodes.iter().any(|n| !(n.is_finite() && n.round() >= 2.0)) {
        return Err(format!(
            "Surface Wetness: a {}×{}×{} field lattice has fewer than 2 nodes on an axis. Wire nodes_x/y/z from the water field's lattice.",
            nodes[0], nodes[1], nodes[2]
        ));
    }
    let nodes = nodes.map(|n| n.round() as u32);
    if center.iter().any(|v| !v.is_finite()) || size.iter().any(|v| !(v.is_finite() && *v > 0.0)) {
        return Err("Surface Wetness: the lattice box must be finite with a positive size".into());
    }
    if !time.is_finite() {
        return Err("Surface Wetness: time must be finite".into());
    }
    if !(drying.is_finite() && drying >= 0.0) {
        return Err("Surface Wetness: drying must be finite and at least 0".into());
    }
    let node_count = nodes
        .iter()
        .try_fold(1u32, |total, &n| total.checked_mul(n))
        .ok_or_else(|| {
            format!(
                "Surface Wetness: a {}×{}×{} field lattice has more nodes than an index can address",
                nodes[0], nodes[1], nodes[2]
            )
        })?;
    let spacing: [f32; 3] = std::array::from_fn(|a| size[a] / (nodes[a] - 1) as f32);
    let h = spacing.into_iter().fold(f32::INFINITY, f32::min);
    let decay = elapsed.map_or(0.0, |dt| {
        (-f64::from(dt) * f64::from(drying) / f64::from(DRY_SECONDS)).exp() as f32
    });
    Ok((
        WetnessUniforms {
            h,
            decay,
            node_count,
            pad: 0,
        },
        nodes,
    ))
}

manifold_node_engine::primitive! {
    name: SurfaceWetness,
    type_id: "node.surface_wetness",
    purpose: "Per node of the water's field lattice, how wet the surface is, 0 to 1, kept between frames. Where the water's level set at the node is below one smallest node spacing (water at the node or within a cell of it) the node is wet, 1. Elsewhere it dries as a thin film evaporates: wetness times exp(-dt * drying / 1724 s) each run, 1724 s being a 0.1 mm film over 5 mm a day of evaporation (FAO-56). The node starts over (only the current contact is wet) on its first run, when epoch changes, when the lattice's node counts change, and when time goes backwards.",
    inputs: {
        field: Array(f32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        time: ScalarF32 required,
        epoch: ScalarF32 optional,
        drying: ScalarF32 optional,
    },
    outputs: {
        wetness: Array(f32),
    },
    params: [
        float_param!("center_x", "Center X", 0.0, -1000.0, 1000.0),
        float_param!("center_y", "Center Y", 0.0, -1000.0, 1000.0),
        float_param!("center_z", "Center Z", 0.0, -1000.0, 1000.0),
        float_param!("size_x", "Size X", 4.0, 0.001, 1000.0),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1000.0),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1000.0),
        float_param!("nodes_x", "Field Nodes X", 2.0, 2.0, 16384.0),
        float_param!("nodes_y", "Field Nodes Y", 2.0, 2.0, 16384.0),
        float_param!("nodes_z", "Field Nodes Z", 2.0, 2.0, 16384.0),
        // Times still-air evaporation: 1 dries a soaked floor in about half an hour, 100 in about 17 s, 0 never.
        float_param!("drying", "Drying", 1.0, 0.0, 1000.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire field from the water's level set, nodes_x/y/z and the box from the same lattice, time from the domain's display_time and epoch from its epoch; its wetness feeds the water object's field_wetness. The wetness persists between frames, so keep the node running while the water is shown.",
    examples: [],
    picker: { label: "Surface Wetness", category: Atom },
    summary: "Remembers how wet the surface is on the water's lattice: soaked where the water touches, drying over time.",
    category: Particles3D,
    role: Filter,
    aliases: ["surface wetness", "wet surfaces", "wetness"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        wet_pipeline: Option<GpuComputePipeline> = None,
        wetness: Option<GpuBuffer> = None,
        last_time: Option<f32> = None,
        last_epoch: Option<f32> = None,
        last_nodes: Option<[u32; 3]> = None,
    },
}

impl Primitive for SurfaceWetness {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "wetness"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "wetness" => self.wetness.as_ref(),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        if port != "wetness" {
            return None;
        }
        inputs
            .iter()
            .find(|(name, _)| *name == "field")
            .map(|&(_, n)| n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if self.wet_pipeline.is_none() {
            let gpu = ctx.gpu_encoder();
            self.wet_pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER,
                "cs_wetness",
                "node.surface_wetness",
            ));
        }
        let Some(field) = ctx.inputs.array("field") else {
            ctx.error("Surface Wetness: field must be wired");
            return;
        };
        let Some(ParamValue::Float(time)) = ctx.inputs.scalar("time") else {
            ctx.error("Surface Wetness: time must be wired");
            return;
        };
        let epoch = match ctx.inputs.scalar("epoch") {
            Some(ParamValue::Float(epoch)) => Some(epoch),
            _ => None,
        };
        let center =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let size = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0));
        // The wetness carries on only from a run of the same lattice, epoch and
        // an earlier or equal time; anything else starts over.
        let same_lattice = self
            .last_nodes
            .is_some_and(|last| last == nodes.map(|n| n.round() as u32));
        let elapsed = self
            .last_time
            .filter(|&last| same_lattice && epoch == self.last_epoch && time >= last)
            .map(|last| time - last);
        let (uniforms, node_counts) = match wetness_uniforms(
            center,
            size,
            nodes,
            time,
            ctx.scalar_or_param("drying", 1.0),
            elapsed,
        ) {
            Ok(result) => result,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let node_count = u64::from(uniforms.node_count);
        if field.size / 4 < node_count {
            ctx.error(format!(
                "Surface Wetness: field holds {} values; a {}×{}×{} field lattice needs {node_count}",
                field.size / 4,
                node_counts[0],
                node_counts[1],
                node_counts[2]
            ));
            return;
        }

        let bytes = (node_count * 4).max(16);
        let reallocated = self
            .wetness
            .as_ref()
            .is_none_or(|buffer| buffer.size != bytes);
        if reallocated {
            let gpu = ctx.gpu_encoder();
            match gpu.device.try_create_buffer_shared(bytes) {
                Ok(buffer) => {
                    // A new buffer holds no state; a restart multiplies it by 0.
                    buffer.zero_fill();
                    self.wetness = Some(buffer);
                }
                Err(error) => {
                    ctx.error(format!(
                        "Surface Wetness: output allocation failed: {error}"
                    ));
                    return;
                }
            }
        }
        let wetness = self
            .wetness
            .as_ref()
            .expect("wetness buffer was just allocated")
            .clone();

        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: field,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &wetness,
                offset: 0,
            },
        ];
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            self.wet_pipeline.as_ref().expect("pipeline prepared"),
            &bindings,
            [uniforms.node_count.div_ceil(THREADS), 1, 1],
            "node.surface_wetness",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
        self.last_time = Some(time);
        self.last_epoch = epoch;
        self.last_nodes = Some(node_counts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("surface wetness shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("surface wetness shader validates");
        assert!(
            module
                .entry_points
                .iter()
                .any(|point| point.name == "cs_wetness")
        );
        assert_eq!(std::mem::size_of::<WetnessUniforms>(), 16);
    }

    #[test]
    fn drying_time_is_the_film_over_the_evaporation_rate() {
        assert!((DRY_SECONDS - 1724.1).abs() < 0.1, "{DRY_SECONDS}");
    }

    #[test]
    fn the_wet_distance_is_the_smallest_spacing_of_a_non_cubic_lattice() {
        let (u, nodes) = wetness_uniforms(
            [0.5, -1.0, 2.0],
            [2.0, 1.5, 2.5],
            [33.0, 25.0, 41.0],
            10.0,
            4.0,
            Some(30.0),
        )
        .unwrap();
        assert_eq!(u.h, 0.0625);
        assert_eq!(u.node_count, 33 * 25 * 41);
        assert_eq!(nodes, [33, 25, 41]);
        let want = (-30.0_f64 * 4.0 / f64::from(DRY_SECONDS)).exp() as f32;
        assert_eq!(u.decay, want);
        let (start, _) = wetness_uniforms([0.0; 3], [2.0; 3], [33.0; 3], 10.0, 4.0, None).unwrap();
        assert_eq!(start.decay, 0.0);
    }

    #[test]
    fn a_bad_lattice_time_or_drying_is_an_error() {
        let ok = |nodes: [f32; 3], size: [f32; 3], time: f32, drying: f32| {
            wetness_uniforms([0.0; 3], size, nodes, time, drying, None)
        };
        assert!(ok([33.0, 1.0, 41.0], [2.0; 3], 1.0, 1.0).is_err());
        assert!(ok([33.0; 3], [2.0, 0.0, 2.0], 1.0, 1.0).is_err());
        assert!(ok([33.0; 3], [2.0; 3], f32::NAN, 1.0).is_err());
        assert!(ok([33.0; 3], [2.0; 3], 1.0, -1.0).is_err());
        assert!(ok([33.0; 3], [2.0; 3], 1.0, f32::NAN).is_err());
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
