//! `node.inverse_fft_2d` — a batch of 2D half spectra back to real fields
//! (docs/OCEAN_SURFACE_DESIGN.md D8). One MPSGraph call through
//! manifold-gpu's `GpuFft`; the plan is rebuilt only when the shape changes.

use std::borrow::Cow;

use manifold_gpu::{FftKind, GpuFft};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: InverseFft2d,
    type_id: "node.inverse_fft_2d",
    purpose: "Inverse 2D FFT of a batch of half spectra. `spectrum` holds B fields of N rows by N/2+1 columns of complex (re, im) values, row-major, field-major; `field` returns B real N×N fields, row-major, scaled by 1/N² so it exactly undoes a forward transform. Column is x, row is z. One backend FFT call per frame.",
    inputs: {
        spectrum: Array([f32; 2]) required,
    },
    outputs: {
        field: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("size"), label: "Size", ty: ParamType::Int, default: ParamValue::Float(256.0), range: Some((16.0, 1024.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("batch"), label: "Batch", ty: ParamType::Int, default: ParamValue::Float(6.0), range: Some((1.0, 16.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire node.ocean_spectrum's `spectrum` with the same Size; Batch 6 matches its six fields. Size must be a power of two.",
    examples: ["Ocean"],
    picker: { label: "Inverse FFT 2D", category: Atom },
    summary: "Turns a batch of wave spectra back into height and displacement fields, the step that makes a spectral ocean.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["ifft", "inverse fft", "fourier", "spectrum to field"],
    boundary_reason: IoBridge,
    extra_fields: {
        plan: Option<(u32, u32, GpuFft)> = None,
    },
}

fn int_param(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(v)) => v.round(),
        _ => default,
    }
}

/// `(size, batch)` when both are valid.
fn shape(size: f32, batch: f32) -> Option<(u32, u32)> {
    let n = size as u32;
    let b = batch as u32;
    ((16.0..=1024.0).contains(&size) && n.is_power_of_two() && (1.0..=16.0).contains(&batch)).then_some((n, b))
}

impl Primitive for InverseFft2d {
    fn array_output_capacity(
        &self,
        port: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        let get = |name: &str, default: f32| match params.get(name) {
            Some(ParamValue::Float(v)) => v.round(),
            _ => default,
        };
        (port == "field").then(|| shape(get("size", 256.0), get("batch", 6.0))).flatten().map(|(n, b)| b * n * n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (size, batch) = (int_param(ctx, "size", 256.0), int_param(ctx, "batch", 6.0));
        let Some((n, b)) = shape(size, batch) else {
            ctx.error(format!("Inverse FFT 2D: Size must be a power of two in 16..1024 and Batch 1..16 (got {size}, {batch})"));
            return;
        };
        let (Some(spectrum), Some(field)) = (ctx.inputs.array("spectrum"), ctx.outputs.array("field")) else {
            return;
        };
        let spectrum_bytes = u64::from(b) * u64::from(n) * u64::from(n / 2 + 1) * 8;
        let field_bytes = u64::from(b) * u64::from(n) * u64::from(n) * 4;
        if spectrum.size < spectrum_bytes || field.size < field_bytes {
            ctx.error(format!(
                "Inverse FFT 2D: a {b}×{n}×{n} transform needs a {spectrum_bytes}-byte spectrum and a {field_bytes}-byte field (got {} and {})",
                spectrum.size, field.size
            ));
            return;
        }
        let gpu = ctx.gpu_encoder();
        if !matches!(&self.plan, Some((pn, pb, _)) if (*pn, *pb) == (n, b)) {
            let fft = GpuFft::new_nd(gpu.device, FftKind::HermiteanToReal, &[b as usize, n as usize, n as usize], &[1, 2]);
            self.plan = Some((n, b, fft));
        }
        let Some((_, _, fft)) = &self.plan else { return };
        fft.encode(gpu.native_enc, spectrum, field);
    }
}
