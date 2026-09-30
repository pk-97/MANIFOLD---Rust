//! `node.fft_3d` and `node.inverse_fft_3d` — the real 3D FFT between a lattice
//! and its half spectrum, one vendor-library call each (MPSGraph through
//! `manifold_gpu::GpuFft`). Boundary atoms: a transform's every output depends
//! on every input across several butterfly passes, so it is the multi-pass
//! class of the codegen exemption list (docs/ADDING_PRIMITIVES.md, exemption
//! class 1; docs/FFT_WATER_SOLVER_DESIGN.md D9).
//!
//! The plan is compiled on the first run and again only when the lattice
//! lengths or the transformed axes change. Axes 2 transforms x and y of every
//! z slice on its own: one batched call, same half-spectrum layout.

use manifold_gpu::{FftKind, GpuFft};

use super::cosine_spectrum::{AXES_PARAM, half_spectrum_len, lattice_nodes, transform_axes};
use super::sort_particles_into_cells::float_param;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use std::borrow::Cow;

/// Lattice lengths and transformed axes, the key a plan is built for.
type PlanKey = ([u32; 3], u32);

/// The plan for `key`, rebuilt when it changes. Lattice (i, j, k) at
/// i + nx·(j + ny·k) is the row-major shape [nz, ny, nx].
fn plan_for<'a>(
    slot: &'a mut Option<(PlanKey, GpuFft)>,
    device: &manifold_gpu::GpuDevice,
    kind: FftKind,
    key: PlanKey,
) -> &'a GpuFft {
    if slot.as_ref().is_none_or(|(built, _)| *built != key) {
        let (nodes, axes) = key;
        let shape = [nodes[2] as usize, nodes[1] as usize, nodes[0] as usize];
        let transformed: &[usize] = if axes == 2 { &[1, 2] } else { &[0, 1, 2] };
        *slot = Some((key, GpuFft::new_nd(device, kind, &shape, transformed)));
    }
    &slot.as_ref().expect("plan built above").1
}

/// Every transformed length must be a power of two, 2 to 1024; a batched z
/// need not be.
fn legal_key(params: &ParamValues) -> Option<PlanKey> {
    let axes = transform_axes(params);
    lattice_nodes(params).filter(|n| n[..axes as usize].iter().all(|v| v.is_power_of_two())).map(|n| (n, axes))
}

const ILLEGAL_LENGTH: &str = "every transformed length must be a power of two, 2 to 1024";

fn refusal(params: &ParamValues, label: &str) -> Option<String> {
    legal_key(params).is_none().then(|| format!("{label}: {ILLEGAL_LENGTH}"))
}

/// The build refuses an illegal lattice (`params_refusal`); a param changed
/// since the build is refused here too.
fn plan_key(ctx: &mut EffectNodeContext<'_, '_>, label: &str) -> Option<PlanKey> {
    let key = legal_key(ctx.params);
    if key.is_none() {
        ctx.error(format!("{label}: {ILLEGAL_LENGTH}"));
    }
    key
}

crate::primitive! {
    name: Fft3d,
    type_id: "node.fft_3d",
    purpose: "Real FFT of a lattice held in an Array<f32> (nodes_x/y/z nodes, node (i, j, k) at i + nx·(j + ny·k), every transformed length a power of two) into its half spectrum: nx/2 + 1 complex entries along x, unscaled, entry (kx, ky, kz) at kx + (nx/2 + 1)·(ky + ny·kz). Axes 3 transforms x, y and z; axes 2 transforms x and y of every z slice on its own. One vendor FFT call.",
    inputs: {
        values: Array(f32) required,
    },
    outputs: {
        spectrum: Array([f32; 2]),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 64.0, 2.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 64.0, 2.0, 1024.0),
        float_param!("nodes_z", "Nodes Z", 64.0, 1.0, 4096.0),
        AXES_PARAM,
    ],
    depth_rule: Terminal,
    composition_notes: "Pair with node.inverse_fft_3d. For a cosine transform (walls instead of wrap-around) wrap it as cosine_reorder → fft_3d → cosine_spectrum.",
    examples: [],
    picker: { label: "FFT 3D", category: Atom },
    summary: "Breaks a 3D grid of numbers into the waves it is made of.",
    category: MathAndConvert,
    role: Map,
    aliases: ["fft", "fourier transform", "spectrum"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        plan: Option<(PlanKey, GpuFft)> = None,
    },
}

impl Primitive for Fft3d {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "spectrum").then(|| lattice_nodes(params).map(half_spectrum_len)).flatten()
    }

    fn params_refusal(&self, params: &ParamValues) -> Option<String> {
        refusal(params, "FFT 3D")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(key) = plan_key(ctx, "FFT 3D") else { return };
        let nodes = key.0;
        let (Some(values), Some(spectrum)) = (ctx.inputs.array("values"), ctx.outputs.array("spectrum")) else {
            return;
        };
        if u64::from(nodes.iter().product::<u32>()) * 4 > values.size || u64::from(half_spectrum_len(nodes)) * 8 > spectrum.size {
            ctx.error(format!("FFT 3D: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let gpu = ctx.gpu_encoder();
        let plan = plan_for(&mut self.plan, gpu.device, FftKind::RealToHermitean, key);
        plan.encode(gpu.native_enc, values, spectrum);
    }
}

crate::primitive! {
    name: InverseFft3d,
    type_id: "node.inverse_fft_3d",
    purpose: "Inverse of node.fft_3d: a half spectrum (nx/2 + 1 complex entries along x) back to the real lattice, scaled by one over the transformed lengths' product (nx·ny·nz with axes 3, nx·ny with axes 2) so the pair round-trips exactly. One vendor FFT call.",
    inputs: {
        spectrum: Array([f32; 2]) required,
    },
    outputs: {
        values: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 64.0, 2.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 64.0, 2.0, 1024.0),
        float_param!("nodes_z", "Nodes Z", 64.0, 1.0, 4096.0),
        AXES_PARAM,
    ],
    depth_rule: Terminal,
    composition_notes: "The half spectrum must be the transform of a real lattice (conjugate-symmetric); cosine_half_spectrum produces one.",
    examples: [],
    picker: { label: "Inverse FFT 3D", category: Atom },
    summary: "Adds a 3D grid's waves back together into the grid.",
    category: MathAndConvert,
    role: Map,
    aliases: ["ifft", "inverse fourier transform"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        plan: Option<(PlanKey, GpuFft)> = None,
    },
}

impl Primitive for InverseFft3d {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "values").then(|| lattice_nodes(params).map(|n| n.iter().product())).flatten()
    }

    fn params_refusal(&self, params: &ParamValues) -> Option<String> {
        refusal(params, "Inverse FFT 3D")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(key) = plan_key(ctx, "Inverse FFT 3D") else { return };
        let nodes = key.0;
        let (Some(spectrum), Some(values)) = (ctx.inputs.array("spectrum"), ctx.outputs.array("values")) else {
            return;
        };
        if u64::from(nodes.iter().product::<u32>()) * 4 > values.size || u64::from(half_spectrum_len(nodes)) * 8 > spectrum.size {
            ctx.error(format!("Inverse FFT 3D: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let gpu = ctx.gpu_encoder();
        let plan = plan_for(&mut self.plan, gpu.device, FftKind::HermiteanToReal, key);
        plan.encode(gpu.native_enc, spectrum, values);
    }
}
