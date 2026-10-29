//! Screen-space subsurface scattering for opaque PBR surfaces. Before the
//! main pass, the scattering draws shade their diffuse light, before their
//! colour and with light from behind through their thin parts, into a light
//! target of their own (`fs_pbr` under `SUBSURFACE_LIGHT_SPECIALIZATIONS`).
//! A compute blur spreads it by each draw's Burley profile
//! (`shaders/subsurface_blur.wgsl`), and the main pass takes the spread light
//! times the subsurface colour in place of its diffuse. The cost follows the
//! pixels the scattering draws cover, never their triangle count, and a
//! frame without them runs none of it.
use super::*;
use manifold_gpu::GpuEncoder;
use manifold_node_engine::scene::material::Subsurface;

const BLUR_WGSL: &str = include_str!("../shaders/subsurface_blur.wgsl");
/// Blur samples per pixel: the shader's `SAMPLES`.
const SAMPLES: usize = 32;
/// The most scattering draws one frame holds: the shader's profile table.
pub(super) const MAX_SCATTERING: u32 = 64;

/// A draw's subsurface scattering as the shader reads it (binding 51).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct SubsurfaceLook {
    /// (weight, slot): the slot is the draw's place among the scattering
    /// draws plus one, 0 for a draw that does not scatter.
    weight_slot: [f32; 4],
    color: [f32; 4],
    /// Burley's profile distance for red, green and blue, the largest in w.
    distance: [f32; 4],
}
const _: () = assert!(std::mem::size_of::<SubsurfaceLook>() == 48);

impl SubsurfaceLook {
    pub(super) fn of(subsurface: &Subsurface, slot: u32) -> Self {
        let [r, g, b] = profile_distance(subsurface.radius, subsurface.color);
        let [cr, cg, cb] = subsurface.color;
        Self {
            weight_slot: [subsurface.weight, slot as f32, 0.0, 0.0],
            color: [cr, cg, cb, 0.0],
            distance: [r, g, b, r.max(g).max(b)],
        }
    }

    pub(super) fn scatters(&self) -> bool {
        self.weight_slot[0] > 0.0
    }
}

/// Burley's profile distance d for each colour: the radius over the shape
/// factor Christensen and Burley fit for diffuse light entering a surface of
/// albedo A, s = 1.9 - A + 3.5 (A - 0.8)². Darker colours spread less far.
fn profile_distance(radius: [f32; 3], albedo: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|c| {
        let a = albedo[c].clamp(0.0, 1.0);
        let s = 1.9 - a + 3.5 * (a - 0.8) * (a - 0.8);
        (radius[c] / s).max(1e-5)
    })
}

/// The blur's sample radii in units of d, stratified over Burley's profile:
/// radius i is where the share of the light within it,
/// 1 - e^(-r) / 4 - 3 e^(-r/3) / 4, reaches (i + 1/2) / N. With x = e^(-r/3)
/// that is the cubic x³ + 3x = 4 (1 - share), which Cardano's formula solves.
fn sample_radii() -> [f32; SAMPLES] {
    std::array::from_fn(|i| {
        let rest = 1.0 - (i as f64 + 0.5) / SAMPLES as f64;
        let root = (4.0 * rest * rest + 1.0).sqrt();
        let x = (2.0 * rest + root).cbrt() + (2.0 * rest - root).cbrt();
        (-3.0 * x.ln()) as f32
    })
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurParams {
    inv_view_proj: [[f32; 4]; 4],
    view_proj: [[f32; 4]; 4],
    size: [u32; 2],
    /// Pixels per metre at view depth 1.
    focal: f32,
    _pad0: u32,
    radii: [[f32; 4]; SAMPLES / 4],
}
const _: () = assert!(std::mem::size_of::<BlurParams>() == 272);

#[derive(Default)]
pub(super) struct SubsurfacePass {
    /// The scattering draws' diffuse light before their colour, slot in
    /// alpha.
    light: Option<manifold_gpu::GpuTexture>,
    /// The light pass's own depth: where the blur finds each surface.
    depth: Option<manifold_gpu::GpuTexture>,
    /// The spread light the main pass reads (binding 52).
    pub(super) output: Option<manifold_gpu::GpuTexture>,
    size: [u32; 2],
    depth_state: Option<manifold_gpu::GpuDepthStencilState>,
    /// `fs_pbr`'s light pass: with its cutout tests, and solid.
    pipelines: Option<[manifold_gpu::GpuRenderPipeline; 2]>,
    blur: Option<manifold_gpu::GpuComputePipeline>,
    radii: [[f32; 4]; SAMPLES / 4],
    allocation_error: Option<String>,
}

impl SubsurfacePass {
    /// Targets at the render size and the pipelines, on a frame that
    /// scatters.
    pub(super) fn ensure(&mut self, device: &manifold_gpu::GpuDevice, w: u32, h: u32) {
        if self.pipelines.is_none() {
            self.pipelines = Some([false, true].map(|solid| light_pipeline(device, solid)));
            self.blur = Some(device.create_compute_pipeline(
                BLUR_WGSL,
                "blur_main",
                "node.render_scene subsurface blur",
            ));
            self.depth_state = Some(device.create_depth_stencil_state(
                &manifold_gpu::GpuDepthStencilDesc {
                    compare: manifold_gpu::GpuCompareFunction::Greater,
                    write_enabled: true,
                },
            ));
            let radii = sample_radii();
            self.radii = std::array::from_fn(|v| std::array::from_fn(|k| radii[v * 4 + k]));
        }
        if self.output.is_some() && self.size == [w, h] {
            return;
        }
        self.allocation_error = self.try_targets(device, w, h).err();
    }

    fn try_targets(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
    ) -> Result<(), String> {
        // Two Rgba16Float targets (8 B/px each) and a Depth32Float (4 B/px).
        manifold_node_engine::load::expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            u64::from(w) * u64::from(h) * 20,
        )
        .map_err(|error| format!("Subsurface scattering has no room for its targets: {error}"))?;
        let make = |format, usage, label| {
            device.try_create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage,
                label,
                mip_levels: 1,
            })
        };
        use manifold_gpu::{GpuTextureFormat as F, GpuTextureUsage as U};
        let light = make(
            F::Rgba16Float,
            U::RENDER_TARGET | U::SHADER_READ,
            "node.render_scene subsurface light",
        )?;
        let depth = make(
            F::Depth32Float,
            U::RENDER_TARGET | U::SHADER_READ,
            "node.render_scene subsurface depth",
        )?;
        let output = make(
            F::Rgba16Float,
            U::SHADER_READ | U::SHADER_WRITE,
            "node.render_scene subsurface scattered",
        )?;
        self.light = Some(light);
        self.depth = Some(depth);
        self.output = Some(output);
        self.size = [w, h];
        Ok(())
    }

    /// Draw the scattering draws' light, then spread it into `output`. Runs
    /// before the main pass, with that pass's binding sets.
    pub(super) fn encode(
        &self,
        enc: &mut GpuEncoder,
        pre: &FramePrelude<'_>,
        draws: &[ObjectDraw<'_>],
        binding_sets: &[Option<[GpuBinding<'_>; 65]>],
    ) -> Result<(), String> {
        if let Some(error) = &self.allocation_error {
            return Err(error.clone());
        }
        let Some(inv_view_proj) = mat4_inverse(pre.view_proj) else {
            return Err(
                "Subsurface scattering needs a camera projection that can be inverted.".into(),
            );
        };
        let (Some(light), Some(depth), Some(output), Some(state), Some(pipelines), Some(blur)) = (
            self.light.as_ref(),
            self.depth.as_ref(),
            self.output.as_ref(),
            self.depth_state.as_ref(),
            self.pipelines.as_ref(),
            self.blur.as_ref(),
        ) else {
            return Err("Subsurface scattering ran before its targets were made.".into());
        };
        let mut profiles = [[0.0f32; 4]; MAX_SCATTERING as usize];
        let calls: Vec<manifold_gpu::Draw> = draws
            .iter()
            .zip(binding_sets)
            .filter(|(draw, _)| draw.subsurface_look.scatters())
            .filter_map(|(draw, bindings)| {
                let slot = draw.subsurface_look.weight_slot[1] as usize;
                profiles[slot - 1] = draw.subsurface_look.distance;
                let pipeline = &pipelines[usize::from(solid_surface(&draw.uniforms))];
                bindings.as_ref().map(|bindings| {
                    draw.camera_live(manifold_gpu::Draw::new(
                        pipeline,
                        bindings,
                        draw.vertex_count,
                        draw.instance_count,
                    ))
                })
            })
            .collect();
        enc.draw(
            &manifold_gpu::DrawPass::color(light, manifold_gpu::GpuLoadAction::Clear).depth(
                depth,
                state,
                manifold_gpu::GpuLoadAction::Clear,
                manifold_gpu::DepthStore::Store,
            ),
            &calls,
            "node.render_scene subsurface light",
        );
        let params = BlurParams {
            inv_view_proj,
            view_proj: pre.view_proj,
            size: [pre.width, pre.height],
            focal: 0.5 * pre.height as f32 * pre.cam.proj(pre.aspect)[1][1],
            _pad0: 0,
            radii: self.radii,
        };
        enc.dispatch_compute(
            blur,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Bytes {
                    binding: 1,
                    data: bytemuck::cast_slice(&profiles),
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: light,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: depth,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: output,
                },
            ],
            [pre.width.div_ceil(8), pre.height.div_ceil(8), 1],
            "node.render_scene subsurface blur",
        );
        Ok(())
    }
}

/// The light pass's `fs_pbr`: one colour target, its own depth, one sample.
pub(super) fn light_pipeline(
    device: &manifold_gpu::GpuDevice,
    solid: bool,
) -> manifold_gpu::GpuRenderPipeline {
    let mut specs = RenderScene::SUBSURFACE_LIGHT_SPECIALIZATIONS.to_vec();
    if solid {
        specs.extend_from_slice(RenderScene::OPAQUE_FAST_SPECIALIZATIONS);
    }
    device.create_render_pipeline(
        &manifold_gpu::RenderPipelineDesc::new(
            RENDER_SCENE_WGSL.as_str(),
            "vs_main",
            "fs_pbr",
            "node.render_scene subsurface light",
        )
        .color(manifold_gpu::GpuTextureFormat::Rgba16Float, None)
        .depth(manifold_gpu::GpuTextureFormat::Depth32Float)
        .specialize(&specs),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Burley's profile at `r` (units of d) as a share of the light within r.
    fn share_within(r: f64) -> f64 {
        1.0 - 0.25 * (-r).exp() - 0.75 * (-r / 3.0).exp()
    }

    #[test]
    fn sample_radii_split_the_profile_into_equal_shares() {
        for (i, r) in sample_radii().into_iter().enumerate() {
            let share = (i as f64 + 0.5) / SAMPLES as f64;
            assert!(
                (share_within(f64::from(r)) - share).abs() < 1e-5,
                "{i}: {r}"
            );
        }
    }

    #[test]
    fn darker_colours_spread_less_far() {
        let d = profile_distance([0.02, 0.02, 0.02], [0.9, 0.5, 0.2]);
        assert!(d[0] > d[1] && d[1] > d[2], "{d:?}");
        // s(0.9) = 1.035: a bright surface spreads about its radius.
        assert!((d[0] - 0.02 / 1.035).abs() < 1e-6);
    }
}
