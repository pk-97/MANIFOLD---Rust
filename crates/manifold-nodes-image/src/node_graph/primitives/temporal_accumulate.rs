//! `node.temporal_accumulate` — averages a screen-space signal over past
//! frames, the reprojected temporal accumulation of Jimenez et al. 2016
//! (GTAO, section 5) with Salvi's variance clipping. Its first user is
//! ambient occlusion, whose noise moves every frame for this node to average.
//!
//! Each frame: fetch last frame's result where the render's `velocity` says
//! this pixel was; keep each bilinear history tap only where its stored view
//! depth matches the depth this surface had last frame (computed from both
//! cameras, so a moving camera keeps its history) and its stored world normal
//! is within about 26 degrees of this pixel's (the ray-tracing accumulator's
//! test; the normal comes from depth by XeGTAO's edge-aware reconstruction).
//! The render's velocity is per vertex for a mesh that deforms with the same
//! triangles and per object otherwise, so copies, particles and meshes whose
//! triangles change reproject from the same pixel; the normal test drops
//! history a surface that turned or changed shape no longer matches, where its
//! depth alone still would. Clip the kept history to
//! the current 3x3 neighbourhood; blend with a 1/n running mean capped at
//! `frames`.
//!
//! History starts over on the first frame, a camera cut (`Camera.shot`
//! changes), a size change and `clear_state`, as a game engine resets its
//! temporal history on a camera cut only. A seek, a clip's loop point and a
//! geometry change never reset it: the per-pixel depth and normal tests
//! already drop history a surface no longer matches, so a looping clip
//! carries no burst of raw noise at its loop point.
//!
//! In export the frames arrive in order from a cleared state, so the same
//! export gives the same frames.

use std::borrow::Cow;

use manifold_gpu::{
    GpuBinding, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::camera::Camera;

use super::ssao_gtao::derive_view_scalars;

const DEPTH_COMMON: &str = include_str!("../../generators/shaders/depth_common.wgsl");
const VIEW_NORMAL_COMMON: &str = include_str!("shaders/view_normal_common.wgsl");
const SHADER: &str = include_str!("shaders/temporal_accumulate.wgsl");

fn shader_source() -> String {
    format!("{DEPTH_COMMON}\n{VIEW_NORMAL_COMMON}\n{SHADER}")
}

manifold_node_engine::primitive! {
    name: TemporalAccumulate,
    type_id: "node.temporal_accumulate",
    purpose: "Reprojected temporal accumulation (Jimenez et al. 2016, GTAO section 5). Fetches last frame's result at uv + (-velocity.x, velocity.y)/2 (`velocity` is the render's NDC motion, now minus previous) with four bilinear taps; a tap is kept only when its stored view depth is within 2% plus the local depth slope of the depth this pixel's surface had from last frame's camera, and its stored world normal (reconstructed from depth, XeGTAO's edge-aware normal) is within cos 0.9 of this pixel's. Kept history is clipped to the current 3x3 neighbourhood's mean ± 1.25 standard deviations, then out = mix(history, in, 1/n) with n = min(history n + 1, `frames`). No usable history gives out = in. Resets on the first frame, a camera cut (Camera.shot), a size change and clear_state; never on a seek, a loop point or a geometry change, where the depth and normal tests drop what no longer matches. `depth` is the render's raw reversed-Z depth.",
    inputs: {
        in: Texture2D required,
        depth: Texture2D required,
        velocity: Texture2D required,
        camera: Camera required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("frames"),
            label: "Frames",
            ty: ParamType::Int,
            default: ParamValue::Float(8.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Warp,
    composition_notes: "Put it right after a noisy screen-space pass whose noise moves every frame (node.ssao_gtao with noise_per_frame on) and before any spatial blur, as Unreal orders its GTAO filters. Wire `depth`, `velocity` and `camera` from the same node.render_scene and its camera. `frames` trades smoothness for how fast a change settles: 8 settles in about 8 frames.",
    examples: [],
    picker: { label: "Temporal Accumulate", category: Atom },
    summary: "Smooths a noisy 3D effect by averaging it with the last few frames, following the scene's motion so nothing smears.",
    category: BlurAndSharpen,
    role: Filter,
    aliases: ["temporal accumulate", "temporal filter", "temporal denoise", "reproject", "history"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        history: History = History::default(),
        textures: Option<HistoryTextures> = None,
    },
}

/// The uniform block of `temporal_accumulate.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    prev_depth: [f32; 4],
    cam_right: [f32; 4],
    cam_up: [f32; 4],
    cam_fwd: [f32; 4],
    tan_half_fov: f32,
    aspect: f32,
    near: f32,
    far: f32,
    frames: f32,
    cold: u32,
    _pad0: f32,
    _pad1: f32,
}

/// Two ping-pong pairs: colour, and (view depth, frames accumulated,
/// octahedral world normal).
struct HistoryTextures {
    width: u32,
    height: u32,
    color: [GpuTexture; 2],
    meta: [GpuTexture; 2],
}

/// When the history is usable, and how last frame's camera saw this frame's
/// points. CPU-only, so the reset rules are tested without a GPU.
#[derive(Default)]
struct History {
    prev_camera: Option<Camera>,
    dims: (u32, u32),
    cleared: bool,
    /// The texture pair last frame wrote; this frame reads it.
    written: usize,
}

impl History {
    /// Advances one frame. Returns whether the history is unusable this
    /// frame and the `prev_depth` row for the shader.
    fn begin_frame(&mut self, camera: &Camera, dims: (u32, u32)) -> (bool, [f32; 4]) {
        // Every check updates its own record, so all of them run every frame.
        let cleared = std::mem::take(&mut self.cleared);
        let resized = std::mem::replace(&mut self.dims, dims) != dims;
        let prev = self.prev_camera.replace(*camera);
        let cold = cleared || resized || prev.is_none_or(|prev| prev.shot != camera.shot);
        let row = match prev {
            Some(prev) if !cold => prev_depth_row(&prev, camera),
            _ => [0.0, 0.0, 1.0, 0.0],
        };
        self.written ^= 1;
        (cold, row)
    }
}

/// Last frame's view depth of a point at view-space `v` in this frame's
/// camera is `dot(row.xyz, v) + row.w` (x right, y up, z forward depth).
/// A periodic camera compares against the copy of the world it last saw.
fn prev_depth_row(prev: &Camera, camera: &Camera) -> [f32; 4] {
    let offset = camera.previous_world_offset(prev.pos, prev.world_period);
    let prev_pos = [
        prev.pos[0] - offset[0],
        prev.pos[1] - offset[1],
        prev.pos[2] - offset[2],
    ];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let f = prev.fwd;
    let shift = [
        camera.pos[0] - prev_pos[0],
        camera.pos[1] - prev_pos[1],
        camera.pos[2] - prev_pos[2],
    ];
    [
        dot(camera.right, f),
        dot(camera.up, f),
        dot(camera.fwd, f),
        dot(shift, f),
    ]
}

fn history_texture(
    device: &manifold_gpu::GpuDevice,
    width: u32,
    height: u32,
    label: &'static str,
) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width,
        height,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET_FULL,
        label,
        mip_levels: 1,
    })
}

impl Primitive for TemporalAccumulate {
    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        self.pipeline.get_or_insert_with(|| {
            device.create_compute_pipeline(&shader_source(), "cs_main", "node.temporal_accumulate")
        });
    }

    fn clear_state(&mut self) {
        self.history.cleared = true;
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let frames = match ctx.params.get("frames") {
            Some(ParamValue::Float(f)) => *f,
            _ => 8.0,
        };
        let (Some(in_tex), Some(depth_tex), Some(velocity_tex), Some(out_tex)) = (
            ctx.inputs.texture_2d("in"),
            ctx.inputs.texture_2d("depth"),
            ctx.inputs.texture_2d("velocity"),
            ctx.outputs.texture_2d("out"),
        ) else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }
        let camera = ctx
            .inputs
            .camera("camera")
            .unwrap_or_else(Camera::default_perspective);
        let [fov_y, near, far] = derive_view_scalars(&camera);
        let (cold, prev_depth) = self.history.begin_frame(&camera, (w, h));
        let basis = |v: [f32; 3]| [v[0], v[1], v[2], 0.0];
        let uniforms = Uniforms {
            prev_depth,
            cam_right: basis(camera.right),
            cam_up: basis(camera.up),
            cam_fwd: basis(camera.fwd),
            tan_half_fov: (fov_y * 0.5).tan(),
            aspect: w as f32 / h as f32,
            near,
            far,
            frames,
            cold: u32::from(cold),
            _pad0: 0.0,
            _pad1: 0.0,
        };

        let gpu = ctx.gpu_encoder();
        let device = gpu.device;
        self.prepare_pipelines(device);
        if self
            .textures
            .as_ref()
            .is_none_or(|t| (t.width, t.height) != (w, h))
        {
            self.textures = Some(HistoryTextures {
                width: w,
                height: h,
                color: [0, 1]
                    .map(|_| history_texture(device, w, h, "node.temporal_accumulate.color")),
                meta: [0, 1]
                    .map(|_| history_texture(device, w, h, "node.temporal_accumulate.meta")),
            });
        }
        let (Some(pipeline), Some(textures)) = (self.pipeline.as_ref(), self.textures.as_ref())
        else {
            return;
        };
        let (write, read) = (self.history.written, self.history.written ^ 1);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: in_tex,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: depth_tex,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: velocity_tex,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: &textures.color[read],
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: &textures.meta[read],
                },
                GpuBinding::Texture {
                    binding: 6,
                    texture: out_tex,
                },
                GpuBinding::Texture {
                    binding: 7,
                    texture: &textures.color[write],
                },
                GpuBinding::Texture {
                    binding: 8,
                    texture: &textures.meta[write],
                },
            ],
            [w.div_ceil(8), h.div_ceil(8), 1],
            "node.temporal_accumulate",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn orbit(orbit: f32, distance: f32) -> Camera {
        Camera::orbit_perspective(orbit, 0.3, distance, 0.9, 0.0, 0.0, 0.05, 200.0)
    }

    /// Steady frames keep history; a camera cut, a resize and clear_state
    /// each start it over for exactly one frame. Time is no input, so a seek
    /// or a clip's loop point, where the camera jumps back to where the clip
    /// began on the same shot, keeps it.
    #[test]
    fn history_resets_on_cut_resize_and_clear_only() {
        let mut history = History::default();
        let start = orbit(0.0, 10.0);
        let dims = (64, 32);
        let step = |history: &mut History, cam: &Camera, dims| history.begin_frame(cam, dims).0;
        assert!(
            step(&mut history, &start, dims),
            "first frame has no history"
        );
        assert!(!step(&mut history, &start, dims));
        let moved = orbit(0.2, 9.0);
        assert!(
            !step(&mut history, &moved, dims),
            "camera motion keeps history"
        );
        assert!(
            !step(&mut history, &start, dims),
            "a loop point keeps history"
        );

        let mut cut = moved;
        cut.shot = 7;
        assert!(step(&mut history, &cut, dims), "a camera cut starts over");
        assert!(!step(&mut history, &cut, dims));
        assert!(step(&mut history, &cut, (128, 64)), "a resize starts over");
        assert!(!step(&mut history, &cut, (128, 64)));
        history.cleared = true;
        assert!(
            step(&mut history, &cut, (128, 64)),
            "clear_state starts over"
        );
        assert!(!step(&mut history, &cut, (128, 64)));
    }

    /// The row maps a point seen by this frame's camera to the view depth
    /// last frame's camera saw it at.
    #[test]
    fn prev_depth_row_matches_the_previous_camera() {
        let prev = orbit(0.0, 10.0);
        let cam = orbit(0.15, 8.5);
        let row = prev_depth_row(&prev, &cam);
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        for world in [[0.0, 0.0, 0.0], [1.0, 0.5, -2.0], [-3.0, 0.2, 1.5]] {
            let rel = [
                world[0] - cam.pos[0],
                world[1] - cam.pos[1],
                world[2] - cam.pos[2],
            ];
            let v = [dot(rel, cam.right), dot(rel, cam.up), dot(rel, cam.fwd)];
            let expected = dot(
                [
                    world[0] - prev.pos[0],
                    world[1] - prev.pos[1],
                    world[2] - prev.pos[2],
                ],
                prev.fwd,
            );
            let got = dot([row[0], row[1], row[2]], v) + row[3];
            assert!(
                (got - expected).abs() < 1e-4,
                "{world:?}: {got} vs {expected}"
            );
        }
        assert_eq!(
            prev_depth_row(&cam, &cam).map(|x| (x * 1e5).round() / 1e5),
            [0.0, 0.0, 1.0, 0.0]
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! The shipped kernel on a still camera: a pattern that flips every
    //! frame averages out, and history a surface no longer matches is
    //! dropped at once.
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
        GpuTextureUsage,
    };
    use manifold_node_engine::gpu::render_target::RenderTarget;

    use super::{Uniforms, history_texture, shader_source};

    const W: u32 = 8;
    const H: u32 = 8;

    fn upload(device: &GpuDevice, value: impl Fn(u32, u32) -> [f32; 4]) -> GpuTexture {
        let px: Vec<f16> = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .flat_map(|(x, y)| value(x, y).map(f16::from_f32))
            .collect();
        let tex = device.create_texture(&GpuTextureDesc {
            width: W,
            height: H,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "temporal-accumulate-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                px.as_ptr().cast::<u8>(),
                std::mem::size_of_val(px.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn read_red(device: &GpuDevice, tex: &GpuTexture) -> Vec<f32> {
        let bytes_per_row = W * 8;
        let readback = device.create_buffer_shared(u64::from(H * bytes_per_row));
        let mut enc = device.create_encoder("temporal-accumulate-readback");
        enc.copy_texture_to_buffer(tex, &readback, W, H, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (W * H * 4) as usize) };
        (0..(W * H) as usize)
            .map(|i| f16::from_bits(halves[i * 4]).to_f32())
            .collect()
    }

    struct Rig {
        device: manifold_gpu::testkit::TestDevice,
        pipeline: manifold_gpu::GpuComputePipeline,
        velocity: GpuTexture,
        color: [GpuTexture; 2],
        meta: [GpuTexture; 2],
        out: RenderTarget,
        frame: usize,
    }

    impl Rig {
        fn new() -> Self {
            let device = manifold_gpu::testkit::test_device();
            let pipeline =
                device.create_compute_pipeline(&shader_source(), "cs_main", "temporal-accumulate");
            let velocity = upload(&device, |_, _| [0.0; 4]);
            let color = [0, 1].map(|_| history_texture(&device, W, H, "history-color"));
            let meta = [0, 1].map(|_| history_texture(&device, W, H, "history-meta"));
            let out = RenderTarget::new(
                &device,
                W,
                H,
                GpuTextureFormat::Rgba16Float,
                "temporal-accumulate-out",
            );
            Self {
                device,
                pipeline,
                velocity,
                color,
                meta,
                out,
                frame: 0,
            }
        }

        /// One frame on a still camera; returns `out.r` per texel.
        fn step(&mut self, input: &GpuTexture, depth: &GpuTexture) -> Vec<f32> {
            let uniforms = Uniforms {
                prev_depth: [0.0, 0.0, 1.0, 0.0],
                cam_right: [1.0, 0.0, 0.0, 0.0],
                cam_up: [0.0, 1.0, 0.0, 0.0],
                cam_fwd: [0.0, 0.0, 1.0, 0.0],
                tan_half_fov: 0.5,
                aspect: 1.0,
                near: 0.05,
                far: 200.0,
                frames: 8.0,
                cold: u32::from(self.frame == 0),
                _pad0: 0.0,
                _pad1: 0.0,
            };
            let (write, read) = (self.frame % 2, (self.frame + 1) % 2);
            let mut enc = self.device.create_encoder("temporal-accumulate");
            enc.dispatch_compute(
                &self.pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Texture {
                        binding: 1,
                        texture: input,
                    },
                    GpuBinding::Texture {
                        binding: 2,
                        texture: depth,
                    },
                    GpuBinding::Texture {
                        binding: 3,
                        texture: &self.velocity,
                    },
                    GpuBinding::Texture {
                        binding: 4,
                        texture: &self.color[read],
                    },
                    GpuBinding::Texture {
                        binding: 5,
                        texture: &self.meta[read],
                    },
                    GpuBinding::Texture {
                        binding: 6,
                        texture: &self.out.texture,
                    },
                    GpuBinding::Texture {
                        binding: 7,
                        texture: &self.color[write],
                    },
                    GpuBinding::Texture {
                        binding: 8,
                        texture: &self.meta[write],
                    },
                ],
                [W.div_ceil(8), H.div_ceil(8), 1],
                "temporal-accumulate",
            );
            enc.commit_and_wait_completed();
            self.frame += 1;
            read_red(&self.device, &self.out.texture)
        }
    }

    /// A checkerboard of 0.2 and 0.8 that flips every frame stands for AO
    /// noise that moves every frame.
    fn checker(device: &GpuDevice, phase: u32) -> GpuTexture {
        upload(device, move |x, y| {
            let v = if (x + y + phase).is_multiple_of(2) {
                0.2
            } else {
                0.8
            };
            [v, v, v, 1.0]
        })
    }

    #[test]
    fn moving_noise_averages_and_a_changed_surface_drops_its_history() {
        let mut rig = Rig::new();
        let near_wall = upload(&rig.device, |_, _| [0.01; 4]);
        let frames = [checker(&rig.device, 0), checker(&rig.device, 1)];
        let first = rig.step(&frames[0], &near_wall);
        assert!(
            (first[0] - 0.2).abs() < 1e-3 && (first[1] - 0.8).abs() < 1e-3,
            "no history: out is in, got {:?}",
            &first[..2]
        );
        let mut out = first;
        for i in 1..40 {
            out = rig.step(&frames[i % 2], &near_wall);
        }
        for (i, v) in out.iter().enumerate() {
            assert!(
                (v - 0.5).abs() < 0.06,
                "texel {i}: flipping noise must average to about 0.5, got {v}"
            );
        }

        // The left half is now a surface ten times farther away: its history
        // belongs to something else and is dropped; the right half keeps it.
        let split = upload(&rig.device, |x, _| {
            [if x < W / 2 { 0.001 } else { 0.01 }; 4]
        });
        let out = rig.step(&frames[0], &split);
        for y in 0..H {
            for x in 0..W {
                let v = out[(y * W + x) as usize];
                let input = if (x + y) % 2 == 0 { 0.2 } else { 0.8 };
                if x < W / 2 - 1 {
                    assert!(
                        (v - input).abs() < 1e-3,
                        "({x},{y}) moved surface must restart from this frame: {v} vs {input}"
                    );
                } else if x > W / 2 {
                    assert!(
                        (v - 0.5).abs() < 0.12,
                        "({x},{y}) unchanged surface keeps its average: {v}"
                    );
                }
            }
        }
    }

    /// A wall that turns about 45 degrees around the middle of the picture:
    /// the two middle columns keep their depth within the slope tolerance,
    /// so only the normal test can tell the history belongs to a surface
    /// facing another way. A spinning copy or a mesh whose triangles change
    /// does this under per-object velocity.
    #[test]
    fn a_surface_that_turns_in_place_drops_its_history() {
        let mut rig = Rig::new();
        let flat = upload(&rig.device, |_, _| [0.01; 4]);
        let frames = [checker(&rig.device, 0), checker(&rig.device, 1)];
        for i in 0..40 {
            rig.step(&frames[i % 2], &flat);
        }
        let turned = upload(&rig.device, |x, _| [0.01 - 0.00114 * (x as f32 - 3.5); 4]);
        let out = rig.step(&frames[0], &turned);
        for y in 1..H - 1 {
            for x in [W / 2 - 1, W / 2] {
                let v = out[(y * W + x) as usize];
                let input = if (x + y) % 2 == 0 { 0.2 } else { 0.8 };
                assert!(
                    (v - input).abs() < 1e-3,
                    "({x},{y}) a turned surface must restart from this frame: {v} vs {input}"
                );
            }
        }
    }
}
