//! `node.bake_environment` — procedurally bake an HDR studio
//! environment map at a configurable resolution. Outputs an
//! equirectangular Rgba16Float Texture2D suitable for wiring into
//! `node.render_mesh`'s `envmap` input for PBR-IBL rendering.
//!
//! The studio aesthetic — ambient floor + bright horizon band + overhead
//! softbox + floor fill + two strip lights + azimuthal modulation — is
//! the default look; defaults match the legacy MetallicGlass envmap
//! bit-for-bit at 512×256 (the canonical reference). Width / height /
//! brightness parameters are exposed for future generators that want a
//! different aesthetic without authoring a new primitive.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EnvmapUniforms {
    width: u32,
    height: u32,
    horizon_strength: f32,
    azimuth_variation: f32,
    // Master brightness applied to the WHOLE baked map (every studio term),
    // unlike `horizon_strength` which only scales the horizon band. 0 = a
    // fully black environment (no IBL contribution at all — the "lit only by
    // scene lights" case).
    intensity: f32,
    // D7 — bake mode: 0 = gradient (legacy studio, byte-identical), 1 = softbox
    // (pure-black base + N emitter strips + optional sun disc).
    mode: u32,
    emitter_count: u32,
    emitter_intensity: f32,
    emitter_elevation: f32,
    emitter_width: f32,
    sun_x: f32,
    sun_y: f32,
    sun_z: f32,
    sun_disc_intensity: f32,
    sun_disc_size: f32,
    // Softbox dome fill (IMPORT_FIDELITY F-P7) — occupies the former pad
    // slot, so the uniform stays 64 bytes (16 × 4-byte fields, naga
    // uniform-size rule). 0.0 = pure-black void, byte-identical to D7.
    fill_intensity: f32,
}

crate::primitive! {
    name: BakeEquirectEnvmap,
    type_id: "node.bake_environment",
    purpose: "Procedurally bake an HDR studio environment map at the given resolution. Equirectangular layout (longitude × latitude). `mode = gradient` (default) matches the legacy MetallicGlass envmap at 512×256: ambient floor + bright horizon band + overhead softbox + floor fill + two strip lights + azimuthal modulation. `mode = softbox` bakes an exact-zero black base lit only by `emitter_count` bright horizontal emitter strips (soft falloff at strip edges only), plus one optional directional sun disc at `sun_x/sun_y/sun_z` sized by `sun_disc_size` and `sun_disc_intensity` (0 = no disc). Output is HDR — wire into `node.render_mesh`'s `envmap` input (PBR material) for IBL reflections, or `node.tone_map` if displaying directly.",
    inputs: {},
    outputs: {
        envmap: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("width"),
            label: "Width",
            ty: ParamType::Int,
            default: ParamValue::Float(512.0),
            range: Some((64.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("height"),
            label: "Height",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((32.0, 2048.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("horizon_strength"),
            label: "Horizon Strength",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("azimuth_variation"),
            label: "Azimuth Variation",
            ty: ParamType::Float,
            default: ParamValue::Float(0.12),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("intensity"),
            label: "Environment Intensity",
            ty: ParamType::Float,
            // 1.0 = the legacy studio look unchanged (every existing preset
            // wiring this node is unaffected). 0 = a black environment, so PBR
            // objects receive no image-based lighting and are lit purely by
            // their scene lights.
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Mode",
            ty: ParamType::Enum,
            // 0 = gradient (legacy studio, byte-identical for all existing
            // presets), 1 = softbox (D7 black-void studio).
            default: ParamValue::Enum(0),
            range: Some((0.0, 1.0)),
            enum_values: &["Gradient", "Softbox"],
        },
        ParamDef {
            name: Cow::Borrowed("emitter_count"),
            label: "Emitter Count",
            ty: ParamType::Int,
            default: ParamValue::Float(3.0),
            range: Some((1.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emitter_intensity"),
            label: "Emitter Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(6.0),
            range: Some((0.0, 50.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emitter_elevation"),
            label: "Emitter Elevation",
            ty: ParamType::Float,
            // Centre of the strip stack in "up" units (sin(elevation)),
            // matching the shader's own `up` convention: -1 = nadir, 0 =
            // horizon, 1 = zenith.
            default: ParamValue::Float(0.15),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emitter_width"),
            label: "Emitter Width",
            ty: ParamType::Float,
            // Half-width of each strip's falloff band, in "up" units.
            default: ParamValue::Float(0.05),
            range: Some((0.001, 0.5)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("sun_x"),
            label: "Sun X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("sun_y"),
            label: "Sun Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("sun_z"),
            label: "Sun Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("sun_disc_intensity"),
            label: "Sun Disc Intensity",
            ty: ParamType::Float,
            // 0 = no disc (byte-identical to the disc mechanism never
            // running at all — F-P3 gate).
            default: ParamValue::Float(0.0),
            range: Some((0.0, 50.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("sun_disc_size"),
            label: "Sun Disc Size",
            ty: ParamType::Float,
            // Angular radius in radians.
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fill"),
            label: "Fill Light",
            ty: ParamType::Float,
            // Softbox dome fill (IMPORT_FIDELITY F-P7): broad neutral
            // studio radiance so metals have a world to reflect. 0 keeps
            // D7's pure-black void byte-identical (existing saved graphs
            // are untouched); the glTF importer sets a non-zero default.
            // Gradient mode ignores it.
            default: ParamValue::Float(0.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "One-shot per chain rebuild — the runtime allocates a persistent slot for this output; the shader writes once on the first frame and downstream samplers read across frames. Width:Height = 2:1 is the standard equirect ratio (matches asin(y/r) / atan2(z,x) mapping). For non-studio aesthetics author a sibling primitive (sky-gradient, file-loaded HDRI) — this one specifically reproduces the legacy MetallicGlass studio (mode=gradient) or D7's black-void softbox (mode=softbox). Softbox strips are compact-support (smoothstep-clamped falloff, EXACTLY 0.0 outside the band) so the base stays pure black — never a Gaussian tail. Sun disc direction (`sun_x/y/z`) is consumed as-is (no conversion math here); F-P4 is responsible for binding it to the scene sun.",
    examples: [],
    picker: { label: "Bake Environment (equirect)", category: Atom },
    summary: "Builds a studio environment map for reflections, laid out as an equirectangular panorama. Feed it into a PBR material for image-based lighting.",
    category: MaterialsAndLighting,
    role: Source,
    aliases: ["environment map", "bake equirect envmap", "equirect", "ibl", "reflection map"],
    boundary_reason: CrossFrameState,
}

impl Primitive for BakeEquirectEnvmap {
    fn output_dims(
        &self,
        port: &str,
        _canvas_dims: (u32, u32),
        _input_dims: &[(&str, (u32, u32))],
        params: &ParamValues,
    ) -> Option<(u32, u32)> {
        if port != "envmap" {
            return None;
        }
        let w = match params.get("width") {
            Some(ParamValue::Float(f)) => f.round().max(64.0) as u32,
            _ => 512,
        };
        let h = match params.get("height") {
            Some(ParamValue::Float(f)) => f.round().max(32.0) as u32,
            _ => 256,
        };
        Some((w, h))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read_int = |name: &str, default: f32| -> f32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => *f,
                _ => default,
            }
        };
        let read_float = |name: &str, default: f32| -> f32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => *f,
                _ => default,
            }
        };
        let width = read_int("width", 512.0).round().max(64.0) as u32;
        let height = read_int("height", 256.0).round().max(32.0) as u32;
        let horizon_strength = read_float("horizon_strength", 1.0);
        let azimuth_variation = read_float("azimuth_variation", 0.12);
        let intensity = read_float("intensity", 1.0);
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
            _ => 0,
        };
        let emitter_count = match ctx.params.get("emitter_count") {
            Some(ParamValue::Float(f)) => f.round().clamp(1.0, 8.0) as u32,
            _ => 3,
        };
        let emitter_intensity = read_float("emitter_intensity", 6.0);
        let emitter_elevation = read_float("emitter_elevation", 0.15);
        let emitter_width = read_float("emitter_width", 0.05);
        let sun_x = read_float("sun_x", 0.0);
        let sun_y = read_float("sun_y", 0.0);
        let sun_z = read_float("sun_z", 0.0);
        let sun_disc_intensity = read_float("sun_disc_intensity", 0.0);
        let sun_disc_size = read_float("sun_disc_size", 0.0);
        let fill_intensity = read_float("fill", 0.0);

        let Some(envmap) = ctx.outputs.texture_2d("envmap") else {
            return;
        };
        let tex_width = envmap.width;
        let tex_height = envmap.height;
        if tex_width == 0 || tex_height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/bake_equirect_envmap.wgsl"),
                "cs_main",
                "node.bake_environment",
            )
        });

        let uniforms = EnvmapUniforms {
            width: tex_width.min(width),
            height: tex_height.min(height),
            horizon_strength,
            azimuth_variation,
            intensity,
            mode,
            emitter_count,
            emitter_intensity,
            emitter_elevation,
            emitter_width,
            sun_x,
            sun_y,
            sun_z,
            sun_disc_intensity,
            sun_disc_size,
            fill_intensity,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: envmap,
                },
            ],
            [tex_width.div_ceil(16), tex_height.div_ceil(16), 1],
            "node.bake_environment",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_zero_inputs_and_envmap_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(BakeEquirectEnvmap::TYPE_ID, "node.bake_environment");
        assert!(BakeEquirectEnvmap::INPUTS.is_empty());
        assert_eq!(BakeEquirectEnvmap::OUTPUTS.len(), 1);
        assert_eq!(BakeEquirectEnvmap::OUTPUTS[0].name, "envmap");
        assert_eq!(BakeEquirectEnvmap::OUTPUTS[0].ty, PortType::Texture2D);
    }

    fn params_at(width: f32, height: f32) -> ParamValues {
        let mut p = ahash::AHashMap::default();
        p.insert(std::borrow::Cow::Borrowed("width"), ParamValue::Float(width));
        p.insert(std::borrow::Cow::Borrowed("height"), ParamValue::Float(height));
        p
    }

    #[test]
    fn output_dims_default_to_512x256() {
        let prim = BakeEquirectEnvmap::new();
        let node: &dyn EffectNode = &prim;
        let params = params_at(512.0, 256.0);
        let dims = node.output_dims("envmap", (1920, 1080), &[], &params);
        assert_eq!(dims, Some((512, 256)));
    }

    #[test]
    fn output_dims_honor_custom_resolution() {
        let prim = BakeEquirectEnvmap::new();
        let node: &dyn EffectNode = &prim;
        let params = params_at(1024.0, 512.0);
        let dims = node.output_dims("envmap", (1920, 1080), &[], &params);
        assert_eq!(dims, Some((1024, 512)));
    }

    #[test]
    fn registers_as_atom() {
        let prim = BakeEquirectEnvmap::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.bake_environment");
    }

    #[test]
    fn uniforms_are_64_bytes() {
        assert_eq!(std::mem::size_of::<EnvmapUniforms>(), 64);
    }

    #[test]
    fn mode_param_defaults_to_gradient() {
        let defaults = BakeEquirectEnvmap::PARAMS;
        let mode_def = defaults.iter().find(|p| p.name == "mode").expect("mode param declared");
        assert_eq!(mode_def.default, ParamValue::Enum(0));
        assert_eq!(mode_def.enum_values, &["Gradient", "Softbox"]);
    }

    #[test]
    fn softbox_params_declared_with_documented_defaults() {
        let defaults = BakeEquirectEnvmap::PARAMS;
        let get = |name: &str| defaults.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("{name} param declared"));
        assert_eq!(get("emitter_count").default, ParamValue::Float(3.0));
        assert_eq!(get("sun_disc_intensity").default, ParamValue::Float(0.0));
        assert_eq!(get("sun_x").default, ParamValue::Float(0.0));
        assert_eq!(get("sun_y").default, ParamValue::Float(0.0));
        assert_eq!(get("sun_z").default, ParamValue::Float(0.0));
        assert_eq!(get("sun_disc_size").default, ParamValue::Float(0.0));
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests for D7 (softbox bake mode,
    //! `docs/IMPORT_FIDELITY_DESIGN.md` F-P3). This atom is a hand-written
    //! WGSL compute shader dispatched directly (`boundary_reason:
    //! CrossFrameState` — a one-shot-per-rebuild state atom, exempt from the
    //! freeze/codegen path), so these tests dispatch the shader the same way
    //! `run()` does: no `EffectNodeContext`/`Graph`/`Executor` needed since
    //! the node has zero inputs and no upstream graph state to exercise.
    use half::f16;
    use manifold_gpu::{GpuBinding, GpuDevice, GpuTexture, GpuTextureFormat};

    use super::*;
    use crate::render_target::RenderTarget;

    fn readback_rgba16f(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("bake-env-readback");
        enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
        let halves: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        (0..(w * h) as usize)
            .map(|i| {
                let o = i * 4;
                [
                    f16::from_bits(halves[o]).to_f32(),
                    f16::from_bits(halves[o + 1]).to_f32(),
                    f16::from_bits(halves[o + 2]).to_f32(),
                    f16::from_bits(halves[o + 3]).to_f32(),
                ]
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn bake(
        device: &GpuDevice,
        w: u32,
        h: u32,
        mode: u32,
        horizon_strength: f32,
        azimuth_variation: f32,
        intensity: f32,
        emitter_count: u32,
        emitter_intensity: f32,
        emitter_elevation: f32,
        emitter_width: f32,
        sun_x: f32,
        sun_y: f32,
        sun_z: f32,
        sun_disc_intensity: f32,
        sun_disc_size: f32,
        fill_intensity: f32,
    ) -> Vec<[f32; 4]> {
        let pipeline =
            device.create_compute_pipeline(include_str!("shaders/bake_equirect_envmap.wgsl"), "cs_main", "bake-env-test");
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "bake-env-out");
        let uniforms = EnvmapUniforms {
            width: w,
            height: h,
            horizon_strength,
            azimuth_variation,
            intensity,
            mode,
            emitter_count,
            emitter_intensity,
            emitter_elevation,
            emitter_width,
            sun_x,
            sun_y,
            sun_z,
            sun_disc_intensity,
            sun_disc_size,
            fill_intensity,
        };
        let mut enc = device.create_encoder("bake-env-dispatch");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "bake-env-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba16f(device, &out.texture, w, h)
    }

    /// Verbatim copy of the pre-D7 build-of-record shader
    /// (`git show c41acc61:crates/manifold-renderer/src/node_graph/primitives/shaders/bake_equirect_envmap.wgsl`).
    /// The "gradient mode byte-identical to build-of-record" gate is proven
    /// by dispatching THIS exact old shader and the new mode=0 shader on the
    /// same GPU with the same inputs and comparing outputs — not by
    /// re-deriving the formula on the CPU. A CPU re-derivation was tried
    /// first and produced spurious sub-ULP drift at some texels: WGSL/MSL's
    /// `pow(f32, f32)` builtin is a generic transcendental, not bit-identical
    /// to Rust's `powi(2)` (exact squaring) — that mismatch is a property of
    /// comparing a GPU transcendental to a CPU one, not a shader regression
    /// (confirmed: the new shader's `mode == 0u` branch is character-for-
    /// character identical to this old file's body).
    const LEGACY_GRADIENT_WGSL: &str = r#"
struct Uniforms {
    width: u32,
    height: u32,
    horizon_strength: f32,
    azimuth_variation: f32,
    intensity: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var dst_tex: texture_storage_2d<rgba16float, write>;

const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318530;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= uniforms.width || gid.y >= uniforms.height { return; }

    let u_coord = f32(gid.x) / f32(uniforms.width);
    let v_coord = f32(gid.y) / f32(uniforms.height);

    let azimuth = u_coord * TAU - PI;
    let elevation = v_coord * PI - PI * 0.5;
    let up = sin(elevation);

    // Studio ambient floor
    var color = vec3<f32>(0.15, 0.15, 0.17);

    // Large bright horizon band (studio windows / white cyclorama)
    color += vec3<f32>(1.5, 1.45, 1.4) * exp(-15.0 * up * up) * uniforms.horizon_strength;

    // Overhead soft box
    let overhead = smoothstep(0.35, 0.65, up) * smoothstep(0.95, 0.65, up);
    color += vec3<f32>(2.5, 2.4, 2.3) * overhead;

    // Floor fill (bounced light from below)
    let floor_fill = smoothstep(-0.15, -0.45, up) * smoothstep(-0.85, -0.45, up);
    color += vec3<f32>(0.4, 0.42, 0.45) * floor_fill;

    // Two narrow strip lights (create chrome streaks)
    color += vec3<f32>(3.5, 3.2, 2.8) * exp(-300.0 * pow(up - 0.12, 2.0));
    color += vec3<f32>(1.5, 2.0, 3.0) * exp(-300.0 * pow(up + 0.08, 2.0));

    // Azimuthal variation — 1.0 + variation * sin(2 azimuth).
    color *= sin(azimuth * 2.0) * uniforms.azimuth_variation + 1.0;

    // Master brightness over every studio term — 0 bakes a fully black map so
    // PBR objects get no image-based lighting (lit only by their scene lights).
    color *= uniforms.intensity;

    textureStore(dst_tex, vec2<i32>(gid.xy), vec4<f32>(color, 1.0));
}
"#;

    #[test]
    fn gradient_mode_matches_legacy_formula() {
        let device = crate::test_device();
        let (w, h) = (64u32, 32u32);
        let horizon_strength = 1.0;
        let azimuth_variation = 0.12;
        let intensity = 1.0;

        // New shader, mode=0 (gradient) — dispatched via the same `bake()`
        // helper every other test in this module uses.
        let new_px = bake(&device, w, h, 0, horizon_strength, azimuth_variation, intensity, 3, 6.0, 0.15, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);

        // Build-of-record shader, dispatched directly with its own (32-byte)
        // uniform layout.
        #[repr(C)]
        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        struct LegacyUniforms {
            width: u32,
            height: u32,
            horizon_strength: f32,
            azimuth_variation: f32,
            intensity: f32,
            _pad0: f32,
            _pad1: f32,
            _pad2: f32,
        }
        let legacy_pipeline = device.create_compute_pipeline(LEGACY_GRADIENT_WGSL, "cs_main", "bake-env-legacy-test");
        let legacy_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "bake-env-legacy-out");
        let legacy_uniforms =
            LegacyUniforms { width: w, height: h, horizon_strength, azimuth_variation, intensity, _pad0: 0.0, _pad1: 0.0, _pad2: 0.0 };
        let mut enc = device.create_encoder("bake-env-legacy-dispatch");
        enc.dispatch_compute(
            &legacy_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&legacy_uniforms) },
                GpuBinding::Texture { binding: 1, texture: &legacy_out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "bake-env-legacy-dispatch",
        );
        enc.commit_and_wait_completed();
        let legacy_px = readback_rgba16f(&device, &legacy_out.texture, w, h);

        for (i, (got, expected)) in new_px.iter().zip(legacy_px.iter()).enumerate() {
            assert_eq!(got, expected, "texel {i}: new mode=0 output must be byte-identical to build-of-record");
        }
    }

    #[test]
    fn softbox_base_is_exact_zero_outside_strip_bands() {
        let device = crate::test_device();
        let (w, h) = (64u32, 64u32);
        let emitter_elevation = 0.15;
        let emitter_width = 0.03; // half_width; falloff band = ±0.03 in "up"
        let px = bake(&device, w, h, 1, 1.0, 0.12, 1.0, 1u32, 6.0, emitter_elevation, emitter_width, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);

        use std::f32::consts::PI;
        let mut max_luminance_outside_band: f32 = 0.0;
        let mut any_outside = false;
        for y in 0..h {
            let v_coord = y as f32 / h as f32;
            let elevation = v_coord * PI - PI * 0.5;
            let up = elevation.sin();
            let dist = (up - emitter_elevation).abs();
            if dist < emitter_width * 1.5 {
                // Inside or adjacent to the falloff band — skip; a separate
                // test proves the strip itself is bright.
                continue;
            }
            any_outside = true;
            for x in 0..w {
                let px_val = px[(y * w + x) as usize];
                let luminance = px_val[0].max(px_val[1]).max(px_val[2]);
                max_luminance_outside_band = max_luminance_outside_band.max(luminance);
            }
        }
        assert!(any_outside, "test fixture must sample rows outside the strip band");
        // D7: pure-black base — EXACTLY 0.0, not merely small.
        assert_eq!(max_luminance_outside_band, 0.0, "softbox base must be exact zero outside strips");
    }

    /// F-P7 dome fill. Three contracts: (a) fill 0 keeps the D7 pure-black
    /// void (covered byte-exactly by `softbox_base_is_exact_zero_outside_
    /// strip_bands` above, which bakes with fill 0); (b) fill > 0 lights
    /// EVERY texel — no direction reflects pure black, which is the whole
    /// point (metals live off the environment); (c) `emitter_intensity`
    /// scales the strips ONLY — a fill-only bake is byte-identical across
    /// strip intensities (the first-cut bug multiplied the fill by it, so
    /// Strip Lights at 0 blacked out the world).
    #[test]
    fn softbox_fill_lights_every_texel_and_ignores_strip_intensity() {
        let device = crate::test_device();
        let (w, h) = (64u32, 64u32);

        // (b) fill only (strip intensity 0): every texel strictly positive.
        let filled = bake(&device, w, h, 1, 1.0, 0.12, 1.0, 3u32, 0.0, 0.15, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8);
        let min_luminance = filled
            .iter()
            .fold(f32::INFINITY, |m, p| m.min(p[0].max(p[1]).max(p[2])));
        assert!(
            min_luminance > 0.0,
            "fill must light every direction: min luminance = {min_luminance}"
        );

        // (c) same fill, wildly different strip intensity, strips still 0-wide
        // contribution because intensity 0 vs 9 only scales the strip term —
        // compare with strips genuinely disabled (intensity 0) on both sides
        // by masking the strip band out of the comparison instead: cheaper
        // and exact — bake twice with strip intensity 0 and 9 and assert the
        // OUTSIDE-band texels are bit-identical.
        let with_strips = bake(&device, w, h, 1, 1.0, 0.12, 1.0, 1u32, 9.0, 0.15, 0.03, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8);
        let no_strips = bake(&device, w, h, 1, 1.0, 0.12, 1.0, 1u32, 0.0, 0.15, 0.03, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8);
        use std::f32::consts::PI;
        for y in 0..h {
            let v_coord = y as f32 / h as f32;
            let up = (v_coord * PI - PI * 0.5).sin();
            if (up - 0.15).abs() < 0.03 * 1.5 {
                continue; // inside/adjacent to the strip band
            }
            for x in 0..w {
                let i = (y * w + x) as usize;
                assert_eq!(
                    with_strips[i], no_strips[i],
                    "fill texels outside the strip band must not depend on emitter_intensity (texel {x},{y})"
                );
            }
        }
    }

    #[test]
    fn softbox_emitter_rows_exceed_hdr_one() {
        let device = crate::test_device();
        let (w, h) = (32u32, 64u32);
        let px = bake(&device, w, h, 1, 1.0, 0.12, 1.0, 1u32, 6.0, 0.15, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let max_channel = px.iter().fold(0.0f32, |m, p| m.max(p[0]).max(p[1]).max(p[2]));
        assert!(max_channel > 1.0, "emitter strip must exceed 1.0 (HDR): max={max_channel}");
    }

    #[test]
    fn softbox_emitter_count_changes_strip_count() {
        let device = crate::test_device();
        let (w, h) = (16u32, 128u32);

        fn count_bands(px: &[[f32; 4]], w: u32, h: u32) -> u32 {
            // Walk the first column top-to-bottom, count contiguous
            // above-zero runs — one run per strip.
            let mut bands = 0u32;
            let mut was_lit = false;
            for y in 0..h {
                let p = px[(y * w) as usize];
                let lit = p[0].max(p[1]).max(p[2]) > 0.0;
                if lit && !was_lit {
                    bands += 1;
                }
                was_lit = lit;
            }
            bands
        }

        let px1 = bake(&device, w, h, 1, 1.0, 0.12, 1.0, 1u32, 6.0, 0.0, 0.02, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(count_bands(&px1, w, h), 1, "emitter_count=1 must bake exactly one strip");

        let px3 = bake(&device, w, h, 1, 1.0, 0.12, 1.0, 3u32, 6.0, 0.0, 0.02, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(count_bands(&px3, w, h), 3, "emitter_count=3 must bake exactly three strips");
    }

    #[test]
    fn softbox_sun_disc_peaks_at_expected_direction() {
        let device = crate::test_device();
        let (w, h) = (128u32, 64u32);
        // A direction off-axis so its equirect coordinates aren't a
        // trivial edge/pole case.
        let sun_dir = {
            let (x, y, z) = (0.4f32, 0.6, 0.3);
            let len = (x * x + y * y + z * z).sqrt();
            (x / len, y / len, z / len)
        };
        let px = bake(
            &device, w, h, 1, 1.0, 0.12, 1.0, 0u32, // zero strips: isolate the disc
            0.0, 0.0, 0.05, sun_dir.0, sun_dir.1, sun_dir.2, 20.0, 0.08, 0.0,
        );

        use std::f32::consts::PI;
        let expected_u = (sun_dir.2.atan2(sun_dir.0)) / (2.0 * PI) + 0.5;
        let expected_v = sun_dir.1.clamp(-1.0, 1.0).asin() / PI + 0.5;
        let expected_x = (expected_u * w as f32).round() as i32;
        let expected_y = (expected_v * h as f32).round() as i32;

        let mut best_luminance = -1.0f32;
        let mut best_xy = (0i32, 0i32);
        for y in 0..h {
            for x in 0..w {
                let p = px[(y * w + x) as usize];
                let luminance = p[0].max(p[1]).max(p[2]);
                if luminance > best_luminance {
                    best_luminance = luminance;
                    best_xy = (x as i32, y as i32);
                }
            }
        }

        let dx = (best_xy.0 - expected_x).abs();
        let dy = (best_xy.1 - expected_y).abs();
        // Committed pixel radius: 2px, generous enough for the discrete
        // 128x64 grid while still proving the disc is at the right spot,
        // not just "somewhere bright".
        assert!(
            dx <= 2 && dy <= 2,
            "brightest texel {best_xy:?} (luminance {best_luminance}) not within 2px of expected ({expected_x},{expected_y})"
        );
    }

    #[test]
    fn softbox_sun_disc_intensity_zero_is_byte_identical_to_no_disc() {
        let device = crate::test_device();
        let (w, h) = (32u32, 32u32);
        // Direction IS set, but intensity is 0 — must be byte-identical to
        // the direction being unset entirely (D7: "sun_disc_intensity = 0
        // is byte-identical to no-disc").
        let with_direction_zero_intensity = bake(&device, w, h, 1, 1.0, 0.12, 1.0, 2u32, 6.0, 0.1, 0.04, 0.5, 0.5, 0.5, 0.0, 0.2, 0.0);
        let no_direction_zero_intensity = bake(&device, w, h, 1, 1.0, 0.12, 1.0, 2u32, 6.0, 0.1, 0.04, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);

        for (i, (a, b)) in with_direction_zero_intensity.iter().zip(no_direction_zero_intensity.iter()).enumerate() {
            assert_eq!(a, b, "texel {i}: sun_disc_intensity=0 must be byte-identical regardless of direction");
        }
    }
}
