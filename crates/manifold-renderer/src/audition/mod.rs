//! Live audition pool for the preset browser
//! (`docs/PRESET_BROWSER_AUDITION_DESIGN.md` section 3.1, phase P2).
//!
//! While the preset browser is open, every visible cell shows the preset
//! actually rendering, applied to the frame at the browser's invocation
//! point (the "tap": master pre-chain composite, or a layer's pre-chain
//! source). Cells are standalone [`PresetRuntime`] builds — never inserted
//! into the live chain, never touching `Project` or `EditingService` (D4:
//! audition is read-only by construction; the document stays byte-identical
//! across a full session).
//!
//! Scheduling (D6): `ensure_cells` runs once per browser open and builds
//! every item's cell; `set_render_list` is a cheap per-frame reorder with no
//! build or evict (empty = browser closed = zero per-frame work); rendering
//! is round-robin, K cells per frame in render-list order. The skip signal
//! is the last completed frame's wall time against the frame budget — under
//! sustained overload the atlas freezes at last-good pixels and the caller
//! skips the bridge publish, exactly like the node-atlas
//! skip-clear-and-publish pattern.
//!
//! Audio semantics (D16): cells render the preset as committed-with-defaults
//! — no audio-modulation simulation. The tap owner's current `trigger_count`
//! IS forwarded (via [`PresetContext`]) so trigger-driven presets behave.

use std::sync::Arc;

use ahash::AHashMap;
use manifold_core::effects::PresetInstance;
use manifold_core::params::ParamManifest;
use manifold_core::preset_def::PresetKind;
use manifold_core::{LayerId, PresetTypeId};
use manifold_gpu::{
    GpuBinding, GpuDevice, GpuLoadAction, GpuRenderPipeline, GpuSampler, GpuTexture,
    GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::{loaded_preset_view_by_id, PrimitiveRegistry};
use crate::preset_context::PresetContext;
use crate::preset_runtime::{ChainBuildInputs, PresetRuntime};
use crate::render_target::RenderTarget;

/// One atlas cell's render size (16:9, matching the node-atlas cell shape).
pub const CELL_W: u32 = 256;
pub const CELL_H: u32 = 144;
/// Hard cap on audition cells (design §3.1: 128 cells at 16:9 256×144 —
/// ~38 MB Rgba16Float at the cap).
pub const MAX_CELLS: usize = 128;
/// Atlas grid columns. Rows derive from the item count at `ensure_cells`.
pub const GRID_COLS: u32 = 16;
/// Cells rendered per frame (D6, initial K=2).
const CELLS_PER_FRAME: usize = 2;

const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

/// Which invocation context the audition grid renders against (D2). Sent
/// once per browser open; the per-frame texture is resolved by the caller
/// AFTER the compositor render on the same encoder (same-command-buffer
/// ordering is the sync).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditionTapTarget {
    /// Master "+ Add Effect": the pre-tonemap composite (`self.main`).
    Master,
    /// Layer "+ Add Effect": that layer's pre-chain source texture. A
    /// `None` per-frame texture means the layer disappeared mid-browse —
    /// cells render against the pool's once-cleared black fallback (§3.1
    /// lifecycle edges; the UI keeps last-good pixels).
    Layer(LayerId),
}

/// The resolved per-frame tap texture handed to [`AuditionPool::render_tick`].
pub enum AuditionTap<'a> {
    Master(&'a GpuTexture),
    Layer {
        layer_id: LayerId,
        texture: Option<&'a GpuTexture>,
    },
}

/// One audition cell: a standalone preset runtime rendering into its own
/// atlas cell. State lives inside the runtime's own `StateStore`, so a fresh
/// cell is a fresh state — the per-open reset (D7) is rebuilding the pool.
struct AuditionCell {
    runtime: PresetRuntime,
    /// Index into the pool's `layout` — the atlas cell this preset blits into.
    cell: u32,
    /// Effect cells: the standalone instance the chain was built from
    /// (`run` needs the effects slice). Generator cells: `None`.
    instance: Option<PresetInstance>,
    /// Generator cells render into their own target before the cell blit
    /// (the runtime installs whatever texture is passed each frame).
    gen_target: Option<RenderTarget>,
}

/// Content-thread pool of standalone preset runtimes rendering live into one
/// atlas. Owned by `ContentPipeline`; the UI sees pixels only, through the
/// IOSurface bridge (D3: one atlas, one bridge).
pub struct AuditionPool {
    device: Arc<GpuDevice>,
    registry: PrimitiveRegistry,
    cells: AHashMap<PresetTypeId, AuditionCell>,
    /// Cell index → preset, for UV publication.
    layout: Vec<PresetTypeId>,
    /// `(preset, atlas UV rect)` per laid-out cell, for the UI handoff.
    uvs: Vec<(PresetTypeId, [f32; 4])>,
    /// Current filtered render order; empty = browser closed (zero cost).
    render_list: Vec<PresetTypeId>,
    round_robin: usize,
    atlas: Option<GpuTexture>,
    atlas_w: u32,
    atlas_h: u32,
    /// Once-cleared black tap fallback for a disappeared layer.
    black: Option<GpuTexture>,
    blit_pipeline: Option<GpuRenderPipeline>,
    sampler: Option<GpuSampler>,
    /// Instrumentation: cells rendered since the last `ensure_cells`
    /// (gate 8a — the render list drives renders, empty list = zero).
    renders_completed: u64,
}

impl AuditionPool {
    pub fn new(device: Arc<GpuDevice>) -> Self {
        Self {
            device,
            registry: PrimitiveRegistry::with_builtin(),
            cells: AHashMap::new(),
            layout: Vec::new(),
            uvs: Vec::new(),
            render_list: Vec::new(),
            round_robin: 0,
            atlas: None,
            atlas_w: 0,
            atlas_h: 0,
            black: None,
            blit_pipeline: None,
            sampler: None,
            renders_completed: 0,
        }
    }

    /// Build every browser item's cell (D6: once per open, no eviction).
    /// Replaces the whole pool — the per-open state reset (D7). Cells whose
    /// preset can't be resolved or built are skipped (one log line each,
    /// never a panic); the UI falls back to the flat text cell for them.
    pub fn ensure_cells(&mut self, ids: Vec<(PresetTypeId, PresetKind)>) {
        self.cells.clear();
        self.layout.clear();
        self.uvs.clear();
        self.render_list.clear();
        self.round_robin = 0;
        self.renders_completed = 0;

        let mut seen: AHashMap<PresetTypeId, ()> = AHashMap::new();
        let mut unique: Vec<(PresetTypeId, PresetKind)> = Vec::with_capacity(ids.len());
        for (id, kind) in ids {
            if seen.insert(id.clone(), ()).is_none() {
                unique.push((id, kind));
            }
        }
        let n = unique.len().min(MAX_CELLS);
        unique.truncate(n);

        for (cell, (id, kind)) in unique.into_iter().enumerate() {
            match self.build_cell(&id, kind, cell as u32) {
                Some(c) => {
                    self.layout.push(id.clone());
                    self.cells.insert(id, c);
                }
                None => {
                    log::warn!("[audition] cell build failed for {:?} — UI falls back to text cell", id);
                }
            }
        }

        self.rebuild_atlas(n.max(1) as u32);
        self.rebuild_uvs();
    }

    /// Per-frame / per-filter reorder (D6): cheap, no build or evict. Ids
    /// without a built cell are dropped. An empty list at browser close
    /// means the next `render_tick` early-returns before any atlas work.
    pub fn set_render_list(&mut self, ids: Vec<PresetTypeId>) {
        self.render_list = ids
            .into_iter()
            .filter(|id| self.cells.contains_key(id))
            .collect();
        if self.round_robin >= self.render_list.len() {
            self.round_robin = 0;
        }
    }

    /// Render up to K cells from the render list (round-robin, D6) into the
    /// atlas. Returns `true` iff the atlas changed this frame — the caller
    /// copies it across the bridge and publishes ONLY on `true`, so a
    /// budget-skipped or idle frame freezes the UI at last-good pixels
    /// instead of strobing black.
    pub fn render_tick(
        &mut self,
        gpu: &mut GpuEncoder,
        tap: AuditionTap,
        ctx: &PresetContext,
        budget_ok: bool,
    ) -> bool {
        if self.render_list.is_empty() {
            return false;
        }
        if !budget_ok {
            return false;
        }
        if self.atlas.is_none() {
            return false;
        }
        self.ensure_pipelines();

        let tap_tex: &GpuTexture = match tap {
            AuditionTap::Master(t) => t,
            AuditionTap::Layer { texture: Some(t), .. } => t,
            // Layer disappeared mid-browse: render against the once-cleared
            // black fallback; the UI keeps last-good pixels (§3.1 edges).
            AuditionTap::Layer { texture: None, .. } => self.black.as_ref().expect("black tap fallback created at ensure_cells"),
        };

        let k = CELLS_PER_FRAME.min(self.render_list.len());
        for off in 0..k {
            let idx = (self.round_robin + off) % self.render_list.len();
            let id = self.render_list[idx].clone();
            let Some(cell) = self.cells.get_mut(&id) else {
                continue;
            };
            let cell_idx = cell.cell;
            let content: Option<GpuTexture> = match &mut cell.instance {
                Some(instance) => {
                    // Effect cell: bind the tap as the chain input (no GPU
                    // copy — the source slot adopts the borrowed texture,
                    // same as the live chain path) and run the graph.
                    cell.runtime
                        .run(gpu, tap_tex, std::slice::from_ref(instance), &[], ctx)
                        .cloned()
                }
                None => {
                    let target = cell
                        .gen_target
                        .as_ref()
                        .expect("generator cell carries its render target");
                    cell.runtime
                        .render(gpu, &target.texture, ctx, &ParamManifest::default());
                    Some(target.texture.clone())
                }
            };
            if let Some(tex) = content {
                self.blit_cell(gpu, &tex, cell_idx);
                self.renders_completed += 1;
            }
        }
        self.round_robin = (self.round_robin + k) % self.render_list.len();
        true
    }

    /// The pool-owned atlas. The caller copies it onto the IOSurface bridge
    /// texture on the same encoder and publishes on frames it changed.
    pub fn atlas_texture(&self) -> Option<&GpuTexture> {
        self.atlas.as_ref()
    }

    /// `(preset, atlas UV rect)` per laid-out cell — shipped to the UI via
    /// `ContentState` so popup cells sample their per-cell sub-rect.
    pub fn cell_uvs(&self) -> &[(PresetTypeId, [f32; 4])] {
        &self.uvs
    }

    /// Instrumentation counter (gate 8a): cells rendered since the last
    /// `ensure_cells`.
    pub fn renders_completed(&self) -> u64 {
        self.renders_completed
    }

    /// Whether a non-empty render list is installed — the pipeline's zero-cost
    /// gate: a closed browser never reaches the tap resolution or the GPU.
    pub fn has_render_list(&self) -> bool {
        !self.render_list.is_empty()
    }

    fn build_cell(&self, id: &PresetTypeId, kind: PresetKind, cell: u32) -> Option<AuditionCell> {
        let view = loaded_preset_view_by_id(id)?;
        match kind {
            PresetKind::Effect => {
                let instance = PresetInstance::new(id.clone());
                let runtime = PresetRuntime::try_build(
                    ChainBuildInputs {
                        effects: std::slice::from_ref(&instance),
                        groups: &[],
                        primitives: &self.registry,
                        device: &self.device,
                        pool: None,
                        width: CELL_W,
                        height: CELL_H,
                        preview_effect: None,
                    },
                    None,
                )?;
                Some(AuditionCell {
                    runtime,
                    cell,
                    instance: Some(instance),
                    gen_target: None,
                })
            }
            PresetKind::Generator => {
                // Standalone build (pattern: preset_thumbnail.rs) — WITHOUT
                // its blocking warmup: commit_and_wait/sleeps/PNG readback
                // are save-time-only. Cells submit into the main content
                // encoder like any chain; state develops over successive
                // round-robin frames like the live playhead would.
                let runtime = PresetRuntime::from_def_with_device(
                    (*view.canonical_def).clone(),
                    &self.registry,
                    Arc::clone(&self.device),
                    CELL_W,
                    CELL_H,
                    FORMAT,
                    None,
                )
                .ok()?;
                let gen_target = RenderTarget::new(&self.device, CELL_W, CELL_H, FORMAT, "audition-cell");
                Some(AuditionCell {
                    runtime,
                    cell,
                    instance: None,
                    gen_target: Some(gen_target),
                })
            }
        }
    }

    fn rebuild_atlas(&mut self, rows: u32) {
        let w = GRID_COLS * CELL_W;
        let h = rows * CELL_H;
        if self.atlas.is_some() && self.atlas_w == w && self.atlas_h == h {
            return;
        }
        self.atlas_w = w;
        self.atlas_h = h;
        self.atlas = Some(self.device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: FORMAT,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
            label: "audition-atlas",
            mip_levels: 1,
        }));
        self.black = Some(self.device.create_texture(&GpuTextureDesc {
            width: CELL_W,
            height: CELL_H,
            depth: 1,
            format: FORMAT,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::SHADER_READ,
            label: "audition-black-tap",
            mip_levels: 1,
        }));
        // ONE lifetime clear for both (Metal doesn't zero-init): the atlas
        // is never cleared per frame — every later write is LoadAction::Load,
        // which is what freezes skipped frames at last-good pixels.
        let mut enc = self.device.create_encoder("audition-atlas-init");
        enc.clear_texture(self.atlas.as_ref().unwrap(), 0.0, 0.0, 0.0, 0.0);
        enc.clear_texture(self.black.as_ref().unwrap(), 0.0, 0.0, 0.0, 1.0);
        enc.commit_and_wait_completed();
    }

    fn rebuild_uvs(&mut self) {
        self.uvs.clear();
        let aw = self.atlas_w as f32;
        let ah = self.atlas_h as f32;
        for (i, id) in self.layout.iter().enumerate() {
            let gx = (i as u32 % GRID_COLS) as f32 * CELL_W as f32;
            let gy = (i as u32 / GRID_COLS) as f32 * CELL_H as f32;
            // Half-texel inset, same as the node-atlas cell UVs: cells render
            // exactly 16:9, so the full cell (minus the inset) is sampled —
            // no letterboxing.
            let u0 = (gx + 0.5) / aw;
            let v0 = (gy + 0.5) / ah;
            let u1 = (gx + CELL_W as f32 - 0.5) / aw;
            let v1 = (gy + CELL_H as f32 - 0.5) / ah;
            self.uvs.push((id.clone(), [u0, v0, u1, v1]));
        }
    }

    fn ensure_pipelines(&mut self) {
        if self.blit_pipeline.is_none() {
            self.blit_pipeline = Some(self.device.create_render_pipeline(
                CELL_BLIT_WGSL,
                "vs_main",
                "fs_main",
                FORMAT,
                None,
                "Audition Cell Blit",
            ));
        }
        if self.sampler.is_none() {
            self.sampler = Some(self.device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                min_filter: manifold_gpu::GpuFilterMode::Linear,
                mag_filter: manifold_gpu::GpuFilterMode::Linear,
                ..Default::default()
            }));
        }
    }

    fn blit_cell(&self, gpu: &mut GpuEncoder, src: &GpuTexture, cell: u32) {
        let atlas = self.atlas.as_ref().expect("render_tick checked atlas");
        let gx = (cell % GRID_COLS) as f32 * CELL_W as f32;
        let gy = (cell / GRID_COLS) as f32 * CELL_H as f32;
        gpu.native_enc.draw_fullscreen_viewport(
            self.blit_pipeline.as_ref().expect("ensure_pipelines before blit"),
            atlas,
            &[
                GpuBinding::Texture { binding: 0, texture: src },
                GpuBinding::Sampler { binding: 1, sampler: self.sampler.as_ref().expect("ensure_pipelines before blit") },
            ],
            (gx, gy, CELL_W as f32, CELL_H as f32),
            GpuLoadAction::Load,
            "Audition Atlas Cell",
        );
    }
}

/// Fullscreen blit for packing a rendered cell into its atlas viewport —
/// the same WGSL shape as the workspace-preview blit.
const CELL_BLIT_WGSL: &str = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_source, s_source, in.uv);
}
"#;

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    //! P2 gates (PRESET_BROWSER_AUDITION_DESIGN section 5): the pool test
    //! (render list drives renders, empty = zero), the budget-skip freeze
    //! test, and the value tests against CPU-computed expected output (no
    //! PNG oracle).

    use super::*;
    use crate::preset_thumbnail::build_gradient_input;

    const TOL: f32 = 1.0e-2;

    /// Deterministic clock, same shape `preset_thumbnail`'s generator
    /// warm-up uses.
    fn test_ctx() -> PresetContext {
        PresetContext {
            time: 1.234,
            beat: 2.5,
            dt: 1.0 / 60.0,
            width: CELL_W,
            height: CELL_H,
            output_width: CELL_W,
            output_height: CELL_H,
            aspect: CELL_W as f32 / CELL_H as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        }
    }

    fn tick(pool: &mut AuditionPool, device: &GpuDevice, tap: &GpuTexture, budget_ok: bool) -> bool {
        let mut enc = device.create_encoder("audition-test-tick");
        let changed = {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            pool.render_tick(&mut gpu, AuditionTap::Master(tap), &test_ctx(), budget_ok)
        };
        enc.commit_and_wait_completed();
        changed
    }

    fn read_atlas(pool: &AuditionPool, device: &GpuDevice) -> (u32, u32, Vec<u8>) {
        let atlas = pool.atlas_texture().expect("atlas exists after ensure_cells");
        let bytes =
            crate::headless_readback::readback_raw_halves(device, atlas, atlas.width, atlas.height);
        (atlas.width, atlas.height, bytes)
    }

    /// f16 pixel accessor over `readback_raw_halves` bytes.
    fn px(bytes: &[u8], w: u32, x: u32, y: u32) -> [f32; 4] {
        let off = ((y * w + x) * 4 * 2) as usize;
        let h = |i: usize| {
            half::f16::from_le_bytes([bytes[off + i * 2], bytes[off + i * 2 + 1]]).to_f32()
        };
        [h(0), h(1), h(2), h(3)]
    }

    fn pool_with(ids: &[(PresetTypeId, PresetKind)], device: &Arc<GpuDevice>) -> AuditionPool {
        let mut pool = AuditionPool::new(Arc::clone(device));
        pool.ensure_cells(ids.to_vec());
        pool
    }

    /// (a) Gate: the render list drives renders; an empty list renders
    /// nothing and reports no atlas change (the closed-browser zero-cost
    /// path, §4/§6.8).
    #[test]
    fn render_list_drives_renders_empty_is_zero() {
        let device = crate::test_device();
        let tap = build_gradient_input(&device, CELL_W, CELL_H, FORMAT);
        let invert = PresetTypeId::new("Invert");
        let mirror = PresetTypeId::new("Mirror");
        let mut pool = pool_with(
            &[(invert.clone(), PresetKind::Effect), (mirror.clone(), PresetKind::Effect)],
            &device.arc(),
        );
        assert_eq!(pool.cells.len(), 2, "both catalog presets must build");

        pool.set_render_list(vec![invert.clone(), mirror.clone()]);
        assert!(tick(&mut pool, &device, &tap.texture, true), "first tick renders + changes");
        assert_eq!(pool.renders_completed(), 2, "K=2 cells per frame");

        pool.set_render_list(vec![]);
        let done = pool.renders_completed();
        assert!(
            !tick(&mut pool, &device, &tap.texture, true),
            "empty render list = no atlas change"
        );
        assert_eq!(pool.renders_completed(), done, "empty render list = zero renders");
    }

    /// (b) Gate: forced over-budget freezes the atlas at last-good — the
    /// tick reports no change (the pipeline skips the transport copy on
    /// that signal) and the atlas bytes are frame-over-frame identical.
    #[test]
    fn budget_skip_freezes_atlas_at_last_good() {
        let device = crate::test_device();
        let tap = build_gradient_input(&device, CELL_W, CELL_H, FORMAT);
        let invert = PresetTypeId::new("Invert");
        let mirror = PresetTypeId::new("Mirror");
        let mut pool = pool_with(
            &[(invert.clone(), PresetKind::Effect), (mirror.clone(), PresetKind::Effect)],
            &device.arc(),
        );
        pool.set_render_list(vec![invert.clone(), mirror.clone()]);

        assert!(tick(&mut pool, &device, &tap.texture, true));
        let before = read_atlas(&pool, &device);

        let done = pool.renders_completed();
        assert!(
            !tick(&mut pool, &device, &tap.texture, false),
            "over-budget tick must report no change (pipeline skips the copy/publish)"
        );
        assert_eq!(pool.renders_completed(), done, "over-budget tick renders nothing");
        let after = read_atlas(&pool, &device);
        assert_eq!(before, after, "atlas must freeze at last-good pixels");
    }

    /// (c) Gate, value level: an Invert cell (default amount = 1.0) over the
    /// standard gradient readback-compares to `1.0 - tap` pixelwise, against
    /// the CPU-computed gradient — no PNG oracle.
    #[test]
    fn invert_cell_matches_cpu_computed_expected() {
        let device = crate::test_device();
        let tap = build_gradient_input(&device, CELL_W, CELL_H, FORMAT);
        let invert = PresetTypeId::new("Invert");
        let mut pool = pool_with(&[(invert.clone(), PresetKind::Effect)], &device.arc());
        pool.set_render_list(vec![invert.clone()]);
        assert!(tick(&mut pool, &device, &tap.texture, true));

        let (aw, _, bytes) = read_atlas(&pool, &device);
        let wm = (CELL_W.max(1) - 1).max(1) as f32;
        let hm = (CELL_H.max(1) - 1).max(1) as f32;
        // Stride the whole cell (plus a few rows) so a wiring error can't
        // hide in one lucky pixel.
        for y in (0..CELL_H).step_by(17) {
            for x in (0..CELL_W).step_by(23) {
                let got = px(&bytes, aw, x, y);
                let u = x as f32 / wm;
                let v = y as f32 / hm;
                let expected = [1.0 - u, 1.0 - v, 1.0 - (u + v) * 0.5, 1.0];
                for c in 0..4 {
                    assert!(
                        (got[c] - expected[c]).abs() < TOL,
                        "cell({x},{y}) ch{c}: got {got:?}, expected ~{expected:?}"
                    );
                }
            }
        }
    }

    /// (d) Gate, value level: the same Invert cell with its outer `amount`
    /// driven to 0 is a near-passthrough — output ≈ tap within tolerance.
    /// (Invert is in the amount-zero-passthrough audit set, so exact identity
    /// is the documented contract; the tolerance absorbs half-float rounding.)
    #[test]
    fn zero_amount_cell_is_near_passthrough() {
        use manifold_core::effect_graph_def::ParamSpecDef;
        use manifold_core::params::{Param, ParamManifest};

        let device = crate::test_device();
        let tap = build_gradient_input(&device, CELL_W, CELL_H, FORMAT);
        let invert = PresetTypeId::new("Invert");
        let mut pool = pool_with(&[(invert.clone(), PresetKind::Effect)], &device.arc());

        // Drive the cell's outer `amount` to 0 exactly the way a committed
        // card at rest would carry it: a manifest entry at value 0.
        let cell = pool.cells.get_mut(&invert).expect("Invert cell built");
        let instance = cell.instance.as_mut().expect("effect cell");
        let spec = ParamSpecDef {
            id: "amount".to_string(),
            name: "Amount".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
            card_visible: true,
        };
        instance.params = ParamManifest::from_params(vec![Param::bundled(spec)]);

        pool.set_render_list(vec![invert.clone()]);
        assert!(tick(&mut pool, &device, &tap.texture, true));

        let (aw, _, bytes) = read_atlas(&pool, &device);
        let wm = (CELL_W.max(1) - 1).max(1) as f32;
        let hm = (CELL_H.max(1) - 1).max(1) as f32;
        for y in (0..CELL_H).step_by(19) {
            for x in (0..CELL_W).step_by(29) {
                let got = px(&bytes, aw, x, y);
                let expected = [x as f32 / wm, y as f32 / hm, (x as f32 / wm + y as f32 / hm) * 0.5, 1.0];
                for c in 0..4 {
                    assert!(
                        (got[c] - expected[c]).abs() < TOL,
                        "cell({x},{y}) ch{c}: got {got:?}, expected ~{expected:?} (passthrough)"
                    );
                }
            }
        }
    }

    /// (a-c helper) Generator cells build standalone and render: StarField
    /// over several ticks produces non-uniform content in its cell (the
    /// dispatch actually ran, not a black/flat cell). (Lissajous is NOT used
    /// here: line-based generators render black through the standalone
    /// runtime path on main today — pre-existing, logged in beads, unrelated
    /// to the audition pool.)
    #[test]
    fn generator_cell_renders_non_uniform_content() {
        let device = crate::test_device();
        let tap = build_gradient_input(&device, CELL_W, CELL_H, FORMAT);
        let starfield = PresetTypeId::new("StarField");
        let mut pool = pool_with(&[(starfield.clone(), PresetKind::Generator)], &device.arc());
        pool.set_render_list(vec![starfield.clone()]);
        for _ in 0..4 {
            assert!(tick(&mut pool, &device, &tap.texture, true));
        }
        let (aw, ah, bytes) = read_atlas(&pool, &device);
        let mut distinct = std::collections::HashSet::new();
        for y in (0..CELL_H).step_by(12) {
            for x in (0..CELL_W).step_by(12) {
                distinct.insert(px(&bytes, aw, x, y).map(|c| (c * 64.0) as u32));
                if distinct.len() > 6 {
                    break;
                }
            }
        }
        assert!(
            distinct.len() > 4,
            "StarField cell must be non-uniform (dispatch actually ran), got {distinct:?} on {aw}x{ah}"
        );
    }
}
