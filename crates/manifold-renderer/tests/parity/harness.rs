//! Pixel-exact parity test harness for Phase 4a primitive migration.
//!
//! For each effect being decomposed into a preset graph, the harness:
//!
//! 1. Renders the legacy `EffectChain` against a fixed input texture +
//!    parameter set.
//! 2. Renders the new graph-decomposed version against the same input +
//!    parameters.
//! 3. Reads both outputs back to CPU and asserts bytewise equality.
//!
//! No tolerance. A single byte mismatch fails the test, and the
//! decomposition is either fixed (typically by collapsing adjacent
//! primitives into a fused composite) or the effect is reclassified as
//! monolithic. See `docs/PRIMITIVE_LIBRARY_DESIGN.md` §5 for the full
//! framework spec and §6.1 for the per-effect migration order.
//!
//! ## Why a custom harness rather than a property test
//!
//! The legacy path is `EffectChain::apply_chain` (real GPU passes,
//! per-effect uniforms, ping-pong buffers). The decomposed path is the
//! graph runtime (`Executor::execute_frame_with_gpu`). Both must drive a
//! real `GpuDevice` — the existing `wgsl_validation.rs` test only
//! compiles shaders. A property-based wrapper around the legacy entry
//! point would still need the same setup, so the harness is the
//! foundation either way.

// Integration-test crates each compile this module independently —
// helpers exercised by one parity test (e.g., `run_primitive_graph`)
// are dead from the perspective of others (e.g., `parity_sanity`).
// Suppress at the module level rather than annotating each helper.
#![allow(dead_code)]

use std::ffi::c_void;
use std::slice;
use std::sync::{Arc, OnceLock};

use half::f16;
use manifold_core::effects::EffectInstance;
use manifold_core::{Beats, EffectTypeId, Seconds};
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_renderer::chain_dispatch::dispatch_chain;
use manifold_renderer::effect::EffectContext;
use manifold_renderer::effect_chain_graph::ChainGraph;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::{
    Backend, EffectNode, ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend,
    NodeInstanceId, ResourceId, Slot, Source, compile,
};
use manifold_renderer::render_target::RenderTarget;

/// Fixed render dimensions for parity tests. Small enough that
/// 24 readbacks per effect (4 fixtures × 6 param sets) finish in
/// seconds; large enough to exercise non-trivial dispatch shapes
/// (multiple of the 16×16 workgroup size).
pub const PARITY_WIDTH: u32 = 128;
pub const PARITY_HEIGHT: u32 = 128;

/// Bytes per pixel for the canonical parity format (`Rgba16Float`).
const BYTES_PER_PIXEL: u32 = 8;

/// Owns the `GpuDevice` and the canonical render dimensions. A single
/// harness instance is shared across every effect's parity sweep via
/// [`shared`] — the chain dispatch path looks effects up via
/// process-wide statics (`primitive_registry()`, `inventory::iter`),
/// so the harness holds no per-effect state.
pub struct ParityHarness {
    pub device: Arc<GpuDevice>,
    pub width: u32,
    pub height: u32,
    pub format: GpuTextureFormat,
}

/// Process-wide cached harness. The expensive part of construction is
/// `GpuDevice::new()` plus building each plugin-using effect's
/// background worker (~5s on M-series). Sharing across all parity
/// submodules drops that 21× cost down to 1×.
static SHARED: OnceLock<ParityHarness> = OnceLock::new();

/// Return the process-wide cached harness. Use this in every parity
/// submodule unless the test specifically needs an *independent*
/// instance (only [`crate::sanity::legacy_invert_is_deterministic_across_harness_instances`]
/// does — proving no shared mutable state leaks across `new()` calls).
pub fn shared() -> &'static ParityHarness {
    SHARED.get_or_init(ParityHarness::new)
}

impl ParityHarness {
    pub fn new() -> Self {
        let device = Arc::new(GpuDevice::new());
        // Prewarm the plugin-using effects so background FFI workers
        // (BlobDetector, DepthEstimator, WireframeDepth) are running
        // before the first parity sweep. The dispatch path itself
        // looks primitives up via `primitive_registry()`, so we never
        // touch the returned processors again — `mem::forget` keeps
        // them alive without storing them in `ParityHarness` (which
        // lives in a `OnceLock` and would need `Sync`, but
        // `PostProcessEffect` is only `Send`).
        std::mem::forget(manifold_renderer::plugin_prewarm::prewarm_all(&device));
        Self {
            device,
            width: PARITY_WIDTH,
            height: PARITY_HEIGHT,
            format: GpuTextureFormat::Rgba16Float,
        }
    }

    pub fn make_target(&self, label: &str) -> RenderTarget {
        RenderTarget::new(&self.device, self.width, self.height, self.format, label)
    }

    /// Upload a host-prepared `Vec<f16>` pixel buffer to a fresh
    /// **CPU-uploadable** texture. Caller is responsible for matching
    /// `w × h × 4` element count (RGBA, row-major, top-down).
    ///
    /// The returned texture uses `CPU_UPLOAD` + `SHADER_READ` +
    /// `COPY_SRC` so `replaceRegion` works (Shared storage) and the
    /// effect's first compute pass can read it. This deviates from
    /// production `RENDER_TARGET_FULL` (Private), but the harness only
    /// cares that the bytes read by the shader match the fixture —
    /// which they do, because Metal samples Shared and Private
    /// textures identically.
    pub fn upload_f16_rgba(&self, label: &str, pixels: &[f16]) -> GpuTexture {
        assert_eq!(
            pixels.len() as u32,
            self.width * self.height * 4,
            "fixture buffer size mismatch"
        );
        let texture = self.device.create_texture(&GpuTextureDesc {
            width: self.width,
            height: self.height,
            depth: 1,
            format: self.format,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label,
            mip_levels: 1,
        });
        // Reinterpret &[f16] as &[u8] — f16 is bit-identical to its
        // little-endian u16 representation on our targets.
        let bytes = unsafe {
            slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), std::mem::size_of_val(pixels))
        };
        self.device.upload_texture(&texture, bytes);
        texture
    }

    /// Run a single `EffectInstance` through the legacy `EffectChain`,
    /// copy the result into a stable destination texture, read it back
    /// to host memory. The returned `Vec<u8>` is the raw Rgba16Float
    /// byte stream — exactly what the parity comparator wants.
    ///
    /// Time / beat / dt are fixed deterministic values so any
    /// time-dependent effect (Glitch, Strobe, VoronoiPrism) produces
    /// reproducible output across runs.
    pub fn run_legacy(
        &self,
        fx: &EffectInstance,
        input: &GpuTexture,
        ctx: &EffectContext,
    ) -> Vec<u8> {
        // Stable destination — we GPU-copy the chain's output into here
        // so readback isn't borrow-locked by the chain's internal
        // ping-pong buffers.
        let dest = self.make_target("parity-legacy-dest");

        let mut chain: Option<ChainGraph> = None;
        let mut render_enc = self.device.create_encoder("parity-legacy-render");
        {
            let mut gpu = RendererGpuEncoder::new(&mut render_enc, &self.device);
            let result = dispatch_chain(&mut chain, &mut gpu, input, slice::from_ref(fx), &[], ctx);
            // `result == None` means the chain skipped (disabled / no
            // registered processor / amount==0). Parity-test contract:
            // the "effect output" in that case equals the input. Same
            // convention the compositor uses.
            let final_tex: &GpuTexture = result.unwrap_or(input);
            gpu.copy_texture_to_texture(final_tex, &dest.texture, self.width, self.height);
        }
        render_enc.commit_and_wait_completed();

        self.readback(&dest.texture)
    }

    /// Run a single-primitive graph `Source → <primitive> → FinalOutput`
    /// against the same input + ctx the legacy chain saw, then read the
    /// output back to host bytes. Companion to [`Self::run_legacy`] —
    /// the bytewise comparison of the two outputs is the parity test.
    ///
    /// Caller provides:
    /// - `prim` — a fresh boxed primitive instance. Default-constructed
    ///   via `Box::new(P::default())` at the call site so the test
    ///   controls the type.
    /// - `set_params` — closure that translates the legacy
    ///   `EffectInstance::param_values` (positional, by `ParamSlot`)
    ///   into named `graph.set_param` calls on the primitive node.
    ///   This is the *only* place parity tests encode the legacy →
    ///   primitive param mapping; if the mapping is wrong, the test
    ///   fails loudly via bytewise mismatch.
    ///
    /// Time, beat, and dt are pulled from `ctx` so any time-dependent
    /// primitive (Strobe, VoronoiPrism in their fused-composite form)
    /// sees the same clock the legacy chain did.
    pub fn run_primitive_graph<F>(
        &self,
        prim: Box<dyn EffectNode>,
        input: &GpuTexture,
        ctx: &EffectContext,
        set_params: F,
    ) -> Vec<u8>
    where
        F: FnOnce(&mut Graph, NodeInstanceId),
    {
        // Build `Source → prim → FinalOutput`.
        let mut graph = Graph::new();
        let source = graph.add_node(Box::new(Source::new()));
        let prim_inputs: Vec<String> = prim.inputs().iter().map(|p| p.name.to_string()).collect();
        let prim_outputs: Vec<String> = prim.outputs().iter().map(|p| p.name.to_string()).collect();
        let prim_id = graph.add_node(prim);
        let final_out = graph.add_node(Box::new(FinalOutput::new()));

        // Connect Source.out to the primitive's first input.
        let in_port: &'static str = leak_static_str(
            prim_inputs
                .first()
                .expect("primitive must have at least one input port")
                .clone(),
        );
        let out_port: &'static str = leak_static_str(
            prim_outputs
                .first()
                .expect("primitive must have at least one output port")
                .clone(),
        );
        graph.connect((source, "out"), (prim_id, in_port)).unwrap();
        graph
            .connect((prim_id, out_port), (final_out, "in"))
            .unwrap();

        set_params(&mut graph, prim_id);

        let plan = compile(&graph).expect("primitive graph must compile");
        let source_res = resource_for_output(&plan, source, "out");

        // GPU-copy the fixture (Shared-storage upload texture) into a
        // Private-storage RenderTarget so the graph runtime samples it
        // through the same memory mode production graphs use. Rgba16F
        // → Rgba16F copy is bit-preserving; no parity skew introduced.
        let source_rt = self.make_target("parity-graph-source");
        let mut copy_enc = self.device.create_encoder("parity-graph-copy-in");
        {
            let mut gpu = RendererGpuEncoder::new(&mut copy_enc, &self.device);
            gpu.copy_texture_to_texture(input, &source_rt.texture, self.width, self.height);
        }
        copy_enc.commit_and_wait_completed();

        // Pre-bind only Source.out — lazy-alloc handles the
        // primitive's output. Capture the next slot watermark so we
        // know where the primitive's output landed.
        let mut backend =
            MetalBackend::new(self.device.clone(), self.width, self.height, self.format);
        backend.pre_bind_texture_2d(source_res, source_rt);
        let prim_output_slot = Slot(backend.slot_count());

        let frame_time = FrameTime {
            beats: Beats(f64::from(ctx.beat)),
            seconds: Seconds(f64::from(ctx.time)),
            delta: Seconds(f64::from(ctx.dt)),
            frame_count: ctx.frame_count,
        };

        let mut native_enc = self.device.create_encoder("parity-graph-render");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &self.device);
            exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let prim_tex = exec
            .backend()
            .texture_2d(prim_output_slot)
            .expect("primitive output slot must be bound after execution");
        self.readback(prim_tex)
    }

    /// Multi-input variant of [`Self::run_primitive_graph`]. The
    /// `aux_inputs` slice maps additional primitive input port names
    /// to caller-supplied textures (typically Shared-storage upload
    /// textures produced by [`Self::upload_f16_rgba`] for LUT data
    /// or other per-frame ancillary inputs). Each aux input is
    /// GPU-copied into a Private-storage `RenderTarget` and
    /// pre-bound to the corresponding Source node's `out`
    /// `ResourceId`, mirroring how the primary fixture is handled.
    ///
    /// Suited to primitives like `ColorLut` (in + lut) or future
    /// `DisplacementMap` (in + displace). For single-input primitives
    /// the simpler [`Self::run_primitive_graph`] stays available.
    pub fn run_primitive_graph_with_aux_inputs<F>(
        &self,
        prim: Box<dyn EffectNode>,
        input: &GpuTexture,
        aux_inputs: &[(&str, &GpuTexture)],
        ctx: &EffectContext,
        set_params: F,
    ) -> Vec<u8>
    where
        F: FnOnce(&mut Graph, NodeInstanceId),
    {
        let mut graph = Graph::new();

        // Collect port names BEFORE adding the primitive to the graph
        // — `add_node` consumes the box, so we'd lose access to its
        // inputs/outputs slice after the call.
        let prim_inputs: Vec<String> = prim.inputs().iter().map(|p| p.name.to_string()).collect();
        let prim_outputs: Vec<String> = prim.outputs().iter().map(|p| p.name.to_string()).collect();
        assert!(
            !prim_inputs.is_empty(),
            "primitive must have at least one input port"
        );
        assert!(
            !prim_outputs.is_empty(),
            "primitive must have at least one output port"
        );

        // Primary `Source → prim.<first input> → FinalOutput`.
        let primary_source = graph.add_node(Box::new(Source::new()));
        let prim_id = graph.add_node(prim);
        let final_out = graph.add_node(Box::new(FinalOutput::new()));
        let primary_port: &'static str = leak_static_str(prim_inputs[0].clone());
        let out_port: &'static str = leak_static_str(prim_outputs[0].clone());
        graph
            .connect((primary_source, "out"), (prim_id, primary_port))
            .unwrap();
        graph
            .connect((prim_id, out_port), (final_out, "in"))
            .unwrap();

        // Aux Source nodes for each (port_name, texture) pair.
        let mut aux_sources: Vec<NodeInstanceId> = Vec::with_capacity(aux_inputs.len());
        for &(port_name, _) in aux_inputs {
            let src = graph.add_node(Box::new(Source::new()));
            let port_leak: &'static str = leak_static_str(port_name.to_string());
            graph.connect((src, "out"), (prim_id, port_leak)).unwrap();
            aux_sources.push(src);
        }

        set_params(&mut graph, prim_id);

        let plan = compile(&graph).expect("aux-input graph must compile");

        // GPU-copy each Shared-storage input into a Private-storage
        // RenderTarget so the executor samples through production
        // memory mode. Aux textures may be smaller than the parity
        // dims (e.g., LUT is 512×1) — allocate the RT at the aux
        // texture's actual size to keep the copy bit-preserving.
        let primary_rt = self.make_target("parity-aux-primary");
        let mut copy_enc = self.device.create_encoder("parity-aux-copy-in");
        {
            let mut gpu = RendererGpuEncoder::new(&mut copy_enc, &self.device);
            gpu.copy_texture_to_texture(input, &primary_rt.texture, self.width, self.height);
        }
        // Aux RTs sized per the source texture.
        let mut aux_rts: Vec<RenderTarget> = Vec::with_capacity(aux_inputs.len());
        for &(_, tex) in aux_inputs {
            let rt = RenderTarget::new(
                &self.device,
                tex.width,
                tex.height,
                self.format,
                "parity-aux-input",
            );
            {
                let mut gpu = RendererGpuEncoder::new(&mut copy_enc, &self.device);
                gpu.copy_texture_to_texture(tex, &rt.texture, tex.width, tex.height);
            }
            aux_rts.push(rt);
        }
        copy_enc.commit_and_wait_completed();

        // Pre-bind primary Source.out + each aux Source.out. Drain
        // aux_rts via an iterator so each Source consumes a RenderTarget
        // exactly once (Vec items aren't `Copy`, can't index in a loop
        // that also borrows from `self.device`).
        let mut backend =
            MetalBackend::new(self.device.clone(), self.width, self.height, self.format);
        let primary_res = resource_for_output(&plan, primary_source, "out");
        backend.pre_bind_texture_2d(primary_res, primary_rt);
        let mut aux_rts_iter = aux_rts.into_iter();
        for src in &aux_sources {
            let res = resource_for_output(&plan, *src, "out");
            let rt = aux_rts_iter
                .next()
                .expect("aux_rts count must match aux_sources count");
            backend.pre_bind_texture_2d(res, rt);
        }

        let prim_output_slot = Slot(backend.slot_count());

        let frame_time = FrameTime {
            beats: Beats(f64::from(ctx.beat)),
            seconds: Seconds(f64::from(ctx.time)),
            delta: Seconds(f64::from(ctx.dt)),
            frame_count: ctx.frame_count,
        };

        let mut native_enc = self.device.create_encoder("parity-aux-render");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &self.device);
            exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let prim_tex = exec
            .backend()
            .texture_2d(prim_output_slot)
            .expect("primitive output slot must be bound after execution");
        self.readback(prim_tex)
    }

    /// Read a texture's contents back to host memory as raw bytes.
    /// Allocates a shared (CPU-visible) Metal buffer, issues a
    /// texture→buffer copy, commits, waits, and snapshots the bytes.
    pub fn readback(&self, texture: &GpuTexture) -> Vec<u8> {
        let bytes_per_row = self.width * BYTES_PER_PIXEL;
        let total_bytes = u64::from(self.height * bytes_per_row);
        let buf = self.device.create_buffer_shared(total_bytes);

        let mut enc = self.device.create_encoder("parity-readback");
        enc.copy_texture_to_buffer(texture, &buf, self.width, self.height, bytes_per_row);
        enc.commit_and_wait_completed();

        let ptr = buf
            .mapped_ptr()
            .expect("shared readback buffer must expose mapped pointer");
        let bytes: &[u8] = unsafe {
            slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total_bytes as usize)
        };
        bytes.to_vec()
    }
}

impl Default for ParityHarness {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Catalog of canonical input fixtures used across every effect's
/// parity sweep. Each fixture exercises a different region of the
/// shader behavior space (HDR vs LDR, smooth gradients vs hard
/// transitions, single channel vs full RGBA). When a single fixture
/// passes parity but another fails, the failure surface is narrowed
/// before we look at math.
#[derive(Debug, Clone, Copy)]
pub enum Fixture {
    /// RGBA linear gradient: R=x, G=y, B=(x+y)/2, A=1.
    Gradient,
    /// Deterministic Rgba16Float noise (wang-hash seeded by pixel
    /// coords). Stresses random-access samplers and per-pixel ops
    /// without periodicity.
    Noise,
    /// HDR bright spots — most pixels at 0.05 luminance, a sparse
    /// scatter of pixels at 4.0. Targets soft-knee threshold,
    /// bloom prefilter, halation, HDR boost.
    BrightSpots,
    /// 8 solid color swatches arranged in a 4×2 grid:
    /// red, green, blue, white, gray, cyan, magenta, yellow. Targets
    /// color-grade / hue-shift / channel mix paths where solid input
    /// makes math errors obvious.
    Swatches,
}

impl Fixture {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gradient => "fixture-gradient",
            Self::Noise => "fixture-noise",
            Self::BrightSpots => "fixture-bright-spots",
            Self::Swatches => "fixture-swatches",
        }
    }

    pub fn build(self, h: &ParityHarness) -> GpuTexture {
        let (w, ht) = (h.width, h.height);
        let mut pixels = vec![f16::from_f32(0.0); (w * ht * 4) as usize];
        match self {
            Self::Gradient => fill_gradient(&mut pixels, w, ht),
            Self::Noise => fill_noise(&mut pixels, w, ht),
            Self::BrightSpots => fill_bright_spots(&mut pixels, w, ht),
            Self::Swatches => fill_swatches(&mut pixels, w, ht),
        }
        h.upload_f16_rgba(self.label(), &pixels)
    }

    /// Every fixture iterated in canonical order. Test sweeps use this
    /// so additions show up automatically.
    pub fn all() -> &'static [Fixture] {
        &[
            Fixture::Gradient,
            Fixture::Noise,
            Fixture::BrightSpots,
            Fixture::Swatches,
        ]
    }
}

fn write_rgba(pixels: &mut [f16], idx: usize, r: f32, g: f32, b: f32, a: f32) {
    pixels[idx] = f16::from_f32(r);
    pixels[idx + 1] = f16::from_f32(g);
    pixels[idx + 2] = f16::from_f32(b);
    pixels[idx + 3] = f16::from_f32(a);
}

fn fill_gradient(pixels: &mut [f16], w: u32, h: u32) {
    let wm = (w.max(1) - 1).max(1) as f32;
    let hm = (h.max(1) - 1).max(1) as f32;
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            let u = x as f32 / wm;
            let v = y as f32 / hm;
            write_rgba(pixels, idx, u, v, (u + v) * 0.5, 1.0);
        }
    }
}

/// Wang hash → uniform [0,1). Deterministic per-(x,y); independent of
/// run order or threading.
fn wang(mut k: u32) -> f32 {
    k = (k ^ 61) ^ (k >> 16);
    k = k.wrapping_mul(9);
    k ^= k >> 4;
    k = k.wrapping_mul(0x27d4_eb2d);
    k ^= k >> 15;
    (k as f32) * (1.0 / u32::MAX as f32)
}

fn fill_noise(pixels: &mut [f16], w: u32, h: u32) {
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            let r = wang(x.wrapping_mul(0x9E37_79B9) ^ y);
            let g = wang(x.wrapping_mul(0x6A09_E667) ^ y.wrapping_mul(3));
            let b = wang(x.wrapping_mul(0xBB67_AE85) ^ y.wrapping_mul(7));
            write_rgba(pixels, idx, r, g, b, 1.0);
        }
    }
}

fn fill_bright_spots(pixels: &mut [f16], w: u32, h: u32) {
    // 0.05 baseline, sparse 4.0 spikes on a deterministic 13×13 lattice.
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            let spike = (x % 13 == 0) && (y % 13 == 0);
            let v = if spike { 4.0 } else { 0.05 };
            write_rgba(pixels, idx, v, v, v, 1.0);
        }
    }
}

fn fill_swatches(pixels: &mut [f16], w: u32, h: u32) {
    const PALETTE: [[f32; 3]; 8] = [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 1.0, 1.0],
        [0.5, 0.5, 0.5],
        [0.0, 1.0, 1.0],
        [1.0, 0.0, 1.0],
        [1.0, 1.0, 0.0],
    ];
    // 4 columns × 2 rows.
    let col_w = w / 4;
    let row_h = h / 2;
    for y in 0..h {
        for x in 0..w {
            let cell_x = (x / col_w.max(1)).min(3);
            let cell_y = (y / row_h.max(1)).min(1);
            let p = PALETTE[(cell_y * 4 + cell_x) as usize];
            let idx = ((y * w + x) * 4) as usize;
            write_rgba(pixels, idx, p[0], p[1], p[2], 1.0);
        }
    }
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

/// Strict bytewise comparison. On mismatch, reports the count of
/// differing bytes plus the first ten differing offsets so failures
/// localize without dumping the whole buffer.
pub fn assert_bytewise_equal(label: &str, a: &[u8], b: &[u8]) {
    assert_eq!(
        a.len(),
        b.len(),
        "{label}: byte length differs (a={}, b={})",
        a.len(),
        b.len()
    );
    if a == b {
        return;
    }
    let mut diffs: Vec<usize> = a
        .iter()
        .zip(b.iter())
        .enumerate()
        .filter_map(|(i, (x, y))| if x != y { Some(i) } else { None })
        .collect();
    let total = diffs.len();
    diffs.truncate(10);
    let detail: Vec<String> = diffs
        .iter()
        .map(|&i| format!("[{i}] a={:02x} b={:02x}", a[i], b[i]))
        .collect();
    panic!(
        "{label}: {total} byte(s) differ of {}. First offsets:\n  {}",
        a.len(),
        detail.join("\n  "),
    );
}

// ---------------------------------------------------------------------------
// Effect context defaults
// ---------------------------------------------------------------------------

/// Deterministic `EffectContext` for parity runs. Time/beat are fixed so
/// any time-dependent effect (Glitch, Strobe, VoronoiPrism) produces
/// reproducible output across runs.
pub fn default_ctx(width: u32, height: u32) -> EffectContext {
    EffectContext {
        time: 1.234,
        beat: 2.5,
        dt: 1.0 / 60.0,
        width,
        height,
        output_width: width,
        output_height: height,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
    }
}

/// Default-parameter `EffectInstance` for an effect type. Pulls the
/// canonical defaults from the registry so tests don't drift from
/// effect-spec changes.
pub fn make_default_effect(effect_type: EffectTypeId) -> EffectInstance {
    let mut fx = EffectInstance::new(effect_type.clone());
    fx.align_to_definition();
    fx.enabled = true;
    fx
}

/// Look up the `ResourceId` of a node's named output port in an
/// `ExecutionPlan`. Mirrors `output_resource` in
/// `node_graph/primitives/compose.rs:207` — same signature, same
/// fall-through panic message. The plan walker only iterates a
/// handful of steps in a parity-test graph, so cost is irrelevant.
fn resource_for_output(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    for step in plan.steps() {
        if step.node == node {
            for &(name, id) in &step.outputs {
                if name == port {
                    return id;
                }
            }
        }
    }
    panic!("no output `{port}` on node {node:?} in plan");
}

/// Leak a `String` into a `&'static str` so it satisfies the
/// `Graph::connect` API which requires static port-name slices. Only
/// used in tests; the leak is bounded by the (small, finite) number
/// of parity tests that run per process.
fn leak_static_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

// Suppress dead-code warnings until at least one parity test file
// imports each helper. The harness is a foundation commit — concrete
// tests follow in §6.1.
#[allow(dead_code)]
fn _unused_anchor(_: Fixture, _: ParityHarness, _: EffectContext) {}
