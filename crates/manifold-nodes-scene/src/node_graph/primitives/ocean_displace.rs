//! `node.ocean_displace` — moves a mesh by three spectral ocean cascades and
//! paints crest foam where the summed surface folds
//! (docs/OCEAN_SURFACE_DESIGN.md D6, D9).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::scene::camera::Camera;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::{active_elements, standalone_pipeline};

pub(crate) const CASCADES: usize = 3;

/// Each cascade's Size, Tile Size, Fade Start and Fade End param names, so
/// the per-frame read builds no strings.
const CASCADE_PARAMS: [[&str; 4]; CASCADES] = [
    ["size_0", "tile_size_0", "fade_start_0", "fade_end_0"],
    ["size_1", "tile_size_1", "fade_start_1", "fade_end_1"],
    ["size_2", "tile_size_2", "fade_start_2", "fade_end_2"],
];

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Cascade {
    pub size: i32,
    pub tile_size: f32,
    pub fade_start: f32,
    pub fade_end: f32,
}

/// Codegen uniform layout: params in PARAMS order, the derived camera x/z,
/// then `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DisplaceUniforms {
    pub choppiness: f32,
    pub foam_threshold: f32,
    pub foam_width: f32,
    pub size_0: i32,
    pub tile_size_0: f32,
    pub fade_start_0: f32,
    pub fade_end_0: f32,
    pub size_1: i32,
    pub tile_size_1: f32,
    pub fade_start_1: f32,
    pub fade_end_1: f32,
    pub size_2: i32,
    pub tile_size_2: f32,
    pub fade_start_2: f32,
    pub fade_end_2: f32,
    pub cam_x: f32,
    pub cam_z: f32,
    pub dispatch_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
}

impl DisplaceUniforms {
    pub fn new(choppiness: f32, foam_threshold: f32, foam_width: f32, c: [Cascade; CASCADES], cam: [f32; 2], dispatch_count: u32) -> Self {
        Self {
            choppiness,
            foam_threshold,
            foam_width,
            size_0: c[0].size,
            tile_size_0: c[0].tile_size,
            fade_start_0: c[0].fade_start,
            fade_end_0: c[0].fade_end,
            size_1: c[1].size,
            tile_size_1: c[1].tile_size,
            fade_start_1: c[1].fade_start,
            fade_end_1: c[1].fade_end,
            size_2: c[2].size,
            tile_size_2: c[2].tile_size,
            fade_start_2: c[2].fade_start,
            fade_end_2: c[2].fade_end,
            cam_x: cam[0],
            cam_z: cam[1],
            dispatch_count,
            _pad0: 0,
            _pad1: 0,
        }
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub fn cascade(&self, c: usize) -> Cascade {
        match c {
            0 => Cascade { size: self.size_0, tile_size: self.tile_size_0, fade_start: self.fade_start_0, fade_end: self.fade_end_0 },
            1 => Cascade { size: self.size_1, tile_size: self.tile_size_1, fade_start: self.fade_start_1, fade_end: self.fade_end_1 },
            _ => Cascade { size: self.size_2, tile_size: self.tile_size_2, fade_start: self.fade_start_2, fade_end: self.fade_end_2 },
        }
    }
}

manifold_node_engine::primitive! {
    name: OceanDisplace,
    type_id: "node.ocean_displace",
    purpose: "Move each vertex of a water mesh by three ocean cascades and paint foam where the waves fold. Each cascade's fields (from node.inverse_fft_2d: height, sideways x/z, and their slopes) are sampled, wrapping, at the vertex's rest position held in its uv (metres) over that cascade's tile, faded out with distance from the camera: Catmull-Rom for height and sideways shift, so the surface stays smooth between samples however close the camera gets, and bilinear for the slopes that drive foam. Position += Σ fade·(Choppiness·Dx, Dy, Choppiness·Dz). Foam mixes vertex colour toward white as the Jacobian of the summed sideways displacement drops below Foam Threshold.",
    inputs: {
        mesh: Array(MeshVertex) required,
        field_0: Array(f32) required,
        field_1: Array(f32) required,
        field_2: Array(f32) required,
        camera: Camera required,
        choppiness: ScalarF32 optional,
        foam_threshold: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef { name: Cow::Borrowed("choppiness"), label: "Choppiness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 3.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("foam_threshold"), label: "Foam Threshold", ty: ParamType::Float, default: ParamValue::Float(0.3), range: Some((-1.0, 1.5)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("foam_width"), label: "Foam Softness", ty: ParamType::Float, default: ParamValue::Float(0.3), range: Some((0.001, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("size_0"), label: "Swell Size", ty: ParamType::Int, default: ParamValue::Float(256.0), range: Some((16.0, 1024.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tile_size_0"), label: "Swell Tile Size", ty: ParamType::Float, default: ParamValue::Float(1000.0), range: Some((1.0, 10000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fade_start_0"), label: "Swell Fade Start", ty: ParamType::Float, default: ParamValue::Float(5000.0), range: Some((0.0, 100000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fade_end_0"), label: "Swell Fade End", ty: ParamType::Float, default: ParamValue::Float(15000.0), range: Some((0.0, 100000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("size_1"), label: "Chop Size", ty: ParamType::Int, default: ParamValue::Float(256.0), range: Some((16.0, 1024.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tile_size_1"), label: "Chop Tile Size", ty: ParamType::Float, default: ParamValue::Float(167.0), range: Some((1.0, 10000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fade_start_1"), label: "Chop Fade Start", ty: ParamType::Float, default: ParamValue::Float(1000.0), range: Some((0.0, 100000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fade_end_1"), label: "Chop Fade End", ty: ParamType::Float, default: ParamValue::Float(3000.0), range: Some((0.0, 100000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("size_2"), label: "Ripple Size", ty: ParamType::Int, default: ParamValue::Float(256.0), range: Some((16.0, 1024.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tile_size_2"), label: "Ripple Tile Size", ty: ParamType::Float, default: ParamValue::Float(27.0), range: Some((1.0, 10000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fade_start_2"), label: "Ripple Fade Start", ty: ParamType::Float, default: ParamValue::Float(150.0), range: Some((0.0, 100000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fade_end_2"), label: "Ripple Fade End", ty: ParamType::Float, default: ParamValue::Float(400.0), range: Some((0.0, 100000.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Feed `mesh` from node.projected_grid and each field from a node.ocean_spectrum → node.inverse_fft_2d pair, swell (largest tile) to field_0. Each cascade's Size and Tile Size must match its spectrum. Fade a cascade out before its shortest wave is smaller than two grid cells there. Wire the same camera as the grid. Follow with node.make_triangles for normals.",
    examples: ["Ocean"],
    picker: { label: "Ocean Displace", category: Atom },
    summary: "Shapes a flat water grid into ocean waves from three wave fields and whitens the crests where the waves break.",
    category: Geometry3D,
    role: Filter,
    aliases: ["ocean", "waves", "displace ocean", "sea surface", "tessendorf"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/ocean_displace_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather],
    output_capacity: manifold_node_engine::freeze::classify::FusedOutputCapacity::FromInput { input: "mesh" },
    derived_uniforms: ["cam_x", "cam_z"],
}

// A fused region's camera x/z, as `run()` reads them.
inventory::submit! {
    manifold_node_engine::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.ocean_displace",
        array_ports: &[],
        recompute: |ctx| ctx.camera.map(|c| vec![c.pos[0], c.pos[2]]),
    }
}

fn param(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(v)) => *v,
        _ => default,
    }
}

impl Primitive for OceanDisplace {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(p, _)| *p == "mesh").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);
        let defaults = [(1000.0, 5000.0, 15000.0), (167.0, 1000.0, 3000.0), (27.0, 150.0, 400.0)];
        let mut cascades = [Cascade::default(); CASCADES];
        for (c, (tile, start, end)) in defaults.into_iter().enumerate() {
            let [size_name, tile_name, start_name, end_name] = CASCADE_PARAMS[c];
            let size = param(ctx, size_name, 256.0).round();
            if !(16.0..=1024.0).contains(&size) || !(size as u32).is_power_of_two() {
                ctx.error(format!("Ocean Displace: cascade {c} Size must be a power of two in 16..1024 (got {size})"));
                return;
            }
            cascades[c] = Cascade {
                size: size as i32,
                tile_size: param(ctx, tile_name, tile),
                fade_start: param(ctx, start_name, start),
                fade_end: param(ctx, end_name, end),
            };
        }
        let (Some(mesh), Some(f0), Some(f1), Some(f2), Some(out)) = (
            ctx.inputs.array("mesh"),
            ctx.inputs.array("field_0"),
            ctx.inputs.array("field_1"),
            ctx.inputs.array("field_2"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        for (c, field) in [f0, f1, f2].into_iter().enumerate() {
            let n = cascades[c].size as u64;
            if field.size < 6 * n * n * 4 {
                ctx.error(format!("Ocean Displace: field_{c} is smaller than six {n}×{n} fields"));
                return;
            }
        }
        let count = active_elements::<MeshVertex>(mesh.size.min(out.size), u32::MAX);
        if count == 0 {
            return;
        }
        let uniforms = DisplaceUniforms::new(
            ctx.scalar_or_param("choppiness", 1.0),
            ctx.scalar_or_param("foam_threshold", 0.3),
            param(ctx, "foam_width", 0.3),
            cascades,
            [cam.pos[0], cam.pos[2]],
            count,
        );
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: mesh, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: f0, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: f1, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: f2, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.ocean_displace",
        );
    }
}

/// CPU twins of the body's field sampling.
#[cfg(test)]
pub(crate) mod sampling {
    /// Catmull-Rom weights for the taps at base-1 .. base+2 and fraction t.
    pub fn catmull_rom(t: f32) -> [f32; 4] {
        let (t2, t3) = (t * t, t * t * t);
        [
            0.5 * (-t3 + 2.0 * t2 - t),
            0.5 * (3.0 * t3 - 5.0 * t2 + 2.0),
            0.5 * (-3.0 * t3 + 4.0 * t2 + t),
            0.5 * (t3 - t2),
        ]
    }

    fn wrap(i: i32, n: i32) -> usize {
        (((i % n) + n) % n) as usize
    }

    /// One N×N field at lattice coordinate t (cells), wrapping.
    pub fn cubic(field: &[f32], n: i32, t: [f32; 2]) -> f32 {
        let (bx, bz) = (t[0].floor() as i32, t[1].floor() as i32);
        let (wx, wz) = (catmull_rom(t[0] - t[0].floor()), catmull_rom(t[1] - t[1].floor()));
        let mut acc = 0.0;
        for (r, wr) in wz.iter().enumerate() {
            let row = wrap(bz + r as i32 - 1, n) * n as usize;
            let line: f32 = wx.iter().enumerate().map(|(c, wc)| wc * field[row + wrap(bx + c as i32 - 1, n)]).sum();
            acc += wr * line;
        }
        acc
    }

    pub fn bilinear(field: &[f32], n: i32, t: [f32; 2]) -> f32 {
        let (bx, bz) = (t[0].floor() as i32, t[1].floor() as i32);
        let w = [t[0] - t[0].floor(), t[1] - t[1].floor()];
        let at = |z: i32, x: i32| field[wrap(z, n) * n as usize + wrap(x, n)];
        let a = at(bz, bx) + (at(bz, bx + 1) - at(bz, bx)) * w[0];
        let b = at(bz + 1, bx) + (at(bz + 1, bx + 1) - at(bz + 1, bx)) * w[0];
        a + (b - a) * w[1]
    }
}

#[cfg(test)]
mod tests {
    use super::sampling::{bilinear, cubic};

    const N: i32 = 16;

    /// A periodic field holding a few waves well inside the lattice's Nyquist.
    fn waves() -> Vec<f32> {
        let tau = std::f32::consts::TAU / N as f32;
        (0..N * N)
            .map(|i| {
                let (x, z) = ((i % N) as f32, (i / N) as f32);
                (2.0 * tau * x + tau * z).sin() + 0.5 * (3.0 * tau * z - tau * x).cos()
            })
            .collect()
    }

    /// Catmull-Rom passes through every sample, so the waves' heights at the
    /// FFT's own points are exact.
    #[test]
    fn cubic_passes_through_samples() {
        let f = waves();
        for z in 0..N {
            for x in 0..N {
                let got = cubic(&f, N, [x as f32, z as f32]);
                assert!((got - f[(z * N + x) as usize]).abs() < 1e-5, "({x}, {z}): {got}");
            }
        }
    }

    /// The faceting cure: across a sample line the cubic surface's slope is
    /// continuous, where bilinear's jumps. Both sides are measured just off
    /// the line, so a kink shows as a slope difference.
    #[test]
    fn cubic_slope_is_continuous_across_samples() {
        let f = waves();
        let (h, e) = (1e-3f32, 1e-2f32);
        let slope = |s: &dyn Fn([f32; 2]) -> f32, x: f32, z: f32| (s([x + h, z]) - s([x - h, z])) / (2.0 * h);
        let mut cubic_jump = 0.0f32;
        let mut bilinear_jump = 0.0f32;
        for k in 0..N {
            let (x, z) = (k as f32, 0.37 + k as f32 * 0.61);
            let c = |p: [f32; 2]| cubic(&f, N, p);
            let b = |p: [f32; 2]| bilinear(&f, N, p);
            cubic_jump = cubic_jump.max((slope(&c, x + e, z) - slope(&c, x - e, z)).abs());
            bilinear_jump = bilinear_jump.max((slope(&b, x + e, z) - slope(&b, x - e, z)).abs());
        }
        assert!(cubic_jump < 0.05, "cubic slope jumps {cubic_jump} across a sample line");
        assert!(bilinear_jump > 10.0 * cubic_jump, "bilinear should kink ({bilinear_jump} vs {cubic_jump})");
    }

    /// Between samples the cubic surface follows the waves: height and slope
    /// close to the exact field, which a wrong weight would miss by far.
    #[test]
    fn cubic_follows_the_waves_between_samples() {
        let f = waves();
        let tau = std::f32::consts::TAU / N as f32;
        let exact = |x: f32, z: f32| (2.0 * tau * x + tau * z).sin() + 0.5 * (3.0 * tau * z - tau * x).cos();
        let h = 1e-3f32;
        let (mut worst_h, mut worst_s) = (0.0f32, 0.0f32);
        for k in 0..64 {
            let (x, z) = (k as f32 * 0.237 % N as f32, k as f32 * 0.419 % N as f32);
            worst_h = worst_h.max((cubic(&f, N, [x, z]) - exact(x, z)).abs());
            let got = (cubic(&f, N, [x + h, z]) - cubic(&f, N, [x - h, z])) / (2.0 * h);
            let want = (exact(x + h, z) - exact(x - h, z)) / (2.0 * h);
            worst_s = worst_s.max((got - want).abs());
        }
        // Measured 0.025 and 0.078 against a peak slope near 1.
        assert!(worst_h < 0.04, "height error {worst_h}");
        assert!(worst_s < 0.12, "slope error {worst_s}");
    }
}

/// CPU reference of the WGSL body, for the proofs.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) fn reference(v: &MeshVertex, u: &DisplaceUniforms, fields: [&[f32]; CASCADES]) -> MeshVertex {
    let smooth = |e0: f32, e1: f32, x: f32| {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let rest = v.uv;
    let d = ((rest[0] - u.cam_x).powi(2) + (rest[1] - u.cam_z).powi(2)).sqrt();
    let (mut disp, mut jac) = ([0.0f32; 3], [0.0f32; 3]);
    for (c, field) in fields.iter().enumerate() {
        let cs = u.cascade(c);
        let fade = 1.0 - smooth(cs.fade_start, cs.fade_end.max(cs.fade_start + 1e-3), d);
        if fade <= 0.0 {
            continue;
        }
        let n = cs.size;
        let t = [rest[0] / cs.tile_size.max(1e-3) * n as f32, rest[1] / cs.tile_size.max(1e-3) * n as f32];
        let plane = (n * n) as usize;
        let s: [f32; 6] = std::array::from_fn(|f| {
            let one = &field[f * plane..(f + 1) * plane];
            if f < 3 { sampling::cubic(one, n, t) } else { sampling::bilinear(one, n, t) }
        });
        disp[0] += fade * u.choppiness * s[1];
        disp[1] += fade * s[0];
        disp[2] += fade * u.choppiness * s[2];
        for (k, j) in jac.iter_mut().enumerate() {
            *j += fade * u.choppiness * s[3 + k];
        }
    }
    let j = (1.0 + jac[0]) * (1.0 + jac[1]) - jac[2] * jac[2];
    let foam = 1.0 - smooth(u.foam_threshold - u.foam_width.max(1e-3), u.foam_threshold, j);
    let mut out = *v;
    for (p, d) in out.position.iter_mut().zip(disp) {
        *p += d;
    }
    for c in &mut out.color[..3] {
        *c += (1.0 - *c) * foam;
    }
    out
}

#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) mod gpu_tests {
    use super::*;

    pub fn fixture() -> (Vec<MeshVertex>, DisplaceUniforms, [Vec<f32>; CASCADES]) {
        let mut rng = 0x1234_5678u32;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            (rng >> 8) as f32 / 16_777_216.0
        };
        let sizes = [16, 32, 16];
        let fields: [Vec<f32>; CASCADES] = std::array::from_fn(|c| {
            let n = sizes[c];
            (0..6 * n * n).map(|f| (next() - 0.5) * if f < 3 * n * n { 2.0 } else { 0.8 }).collect()
        });
        let vertices: Vec<MeshVertex> = (0..2000)
            .map(|i| {
                let (x, z) = ((next() - 0.5) * 600.0, (next() - 0.5) * 600.0 - (i % 7) as f32 * 40.0);
                MeshVertex {
                    position: [x, 0.25, z],
                    _pad0: 0.0,
                    normal: [0.0, 1.0, 0.0],
                    _pad1: 0.0,
                    uv: [x, z],
                    _pad2: [0.0; 2],
                    tangent: [0.0; 4],
                    color: [0.02, 0.05, 0.07, 1.0],
                }
            })
            .collect();
        let uniforms = DisplaceUniforms::new(
            1.4,
            0.4,
            0.3,
            [
                Cascade { size: 16, tile_size: 400.0, fade_start: 300.0, fade_end: 500.0 },
                Cascade { size: 32, tile_size: 67.0, fade_start: 100.0, fade_end: 250.0 },
                Cascade { size: 16, tile_size: 11.0, fade_start: 40.0, fade_end: 90.0 },
            ],
            [10.0, -20.0],
            vertices.len() as u32,
        );
        (vertices, uniforms, fields)
    }

    /// The generated kernel matches the CPU reference vertex for vertex.
    #[test]
    fn ocean_displace_matches_cpu() {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<OceanDisplace>().expect("ocean_displace codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, manifold_node_engine::freeze::codegen::ENTRY, "ocean-displace-test");
        let (vertices, uniforms, fields) = fixture();
        let upload = |bytes: &[u8]| {
            let b = device.create_buffer_shared(bytes.len() as u64);
            unsafe { b.write(0, bytes) };
            b
        };
        let mesh = upload(bytemuck::cast_slice(&vertices));
        let f: Vec<_> = fields.iter().map(|v| upload(bytemuck::cast_slice(v))).collect();
        let out = device.create_buffer_shared(std::mem::size_of_val(vertices.as_slice()) as u64);
        let mut enc = device.create_encoder("ocean-displace-test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &mesh, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &f[0], offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &f[1], offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: &f[2], offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: &out, offset: 0 },
            ],
            [(vertices.len() as u32).div_ceil(256), 1, 1],
            "ocean-displace-test",
        );
        enc.commit_and_wait_completed();
        let ptr = out.mapped_ptr().expect("shared buffer");
        let got = unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, vertices.len()) };
        let refs = [fields[0].as_slice(), fields[1].as_slice(), fields[2].as_slice()];
        let mut foamy = 0;
        for (i, (v, g)) in vertices.iter().zip(got).enumerate() {
            let want = reference(v, &uniforms, refs);
            for a in 0..3 {
                assert!((g.position[a] - want.position[a]).abs() <= 1e-4, "vertex {i} axis {a}: {} vs {}", g.position[a], want.position[a]);
                assert!((g.color[a] - want.color[a]).abs() <= 1e-4, "vertex {i} colour {a}: {} vs {}", g.color[a], want.color[a]);
            }
            assert_eq!(g.uv, v.uv);
            foamy += usize::from(want.color[0] > 0.5);
        }
        assert!(foamy > 0 && foamy < vertices.len(), "the fixture must exercise both foam and clear water ({foamy})");
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
