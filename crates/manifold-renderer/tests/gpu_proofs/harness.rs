//! Shared GPU harness for the `gpu-proofs` integration binary.
//!
//! Owns the `GpuDevice` and canonical render dimensions, and provides the
//! two capabilities its remaining consumers need:
//!
//! - [`ParityHarness::run_transparent_probe`] — builds a `Source → prim →
//!   FinalOutput` graph, feeds every texture input a transparent fixture,
//!   and reads back the output. The alpha-contract sweep's oracle.
//! - [`ParityHarness::make_target`] / [`ParityHarness::readback`] — the
//!   smoke suite renders each generator preset into a target and reads it
//!   back to check for NaN/Inf.
//!
//! The old byte-exact parity machinery (a legacy `dispatch_chain` render
//! path plus per-effect fixture sweeps) was deleted with the parity tests
//! it served — the legacy effect impls are gone, so there is no longer a
//! second path to compare against.

// The two consumer modules (`alpha_contract`, `smoke`) each compile this
// module independently, so a helper used by one is dead from the other's
// perspective. Suppress at the module level rather than annotating each.
#![allow(dead_code)]

use std::ffi::c_void;
use std::slice;
use std::sync::{Arc, OnceLock};

use half::f16;
use manifold_core::{Beats, Seconds};
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::{
    Backend, EffectNode, ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend,
    NodeInstanceId, PortType, ResourceId, Slot, Source, compile,
};
use manifold_renderer::render_target::RenderTarget;

/// Fixed render dimensions. Small enough that readbacks finish in
/// milliseconds; large enough to exercise non-trivial dispatch shapes
/// (multiple of the 16×16 workgroup size).
pub const PARITY_WIDTH: u32 = 128;
pub const PARITY_HEIGHT: u32 = 128;

/// Bytes per pixel for the canonical format (`Rgba16Float`).
const BYTES_PER_PIXEL: u32 = 8;

/// Owns the `GpuDevice` and the canonical render dimensions. A single
/// instance is shared across both suites via [`shared`].
pub struct ParityHarness {
    pub device: Arc<GpuDevice>,
    pub width: u32,
    pub height: u32,
    pub format: GpuTextureFormat,
}

/// Process-wide cached harness. The expensive part of construction is
/// `GpuDevice::new()` plus building each plugin-using effect's background
/// worker (~5s on M-series). Sharing it across both suites pays that once.
static SHARED: OnceLock<ParityHarness> = OnceLock::new();

/// Return the process-wide cached harness.
pub fn shared() -> &'static ParityHarness {
    SHARED.get_or_init(ParityHarness::new)
}

impl ParityHarness {
    pub fn new() -> Self {
        let device = Arc::new(GpuDevice::new());
        // Prewarm the plugin-using effects so background FFI workers
        // (BlobDetector, DepthEstimator, WireframeDepth) are running
        // before the first sweep. The graph path looks primitives up via
        // `primitive_registry()`, so we never touch the returned
        // processors again — `mem::forget` keeps them alive without
        // storing them in `ParityHarness` (which lives in a `OnceLock`
        // and would need `Sync`, but `PostProcessEffect` is only `Send`).
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
    /// first compute pass can read it. Metal samples Shared and Private
    /// textures identically, so this deviation from production
    /// `RENDER_TARGET_FULL` (Private) doesn't skew the read.
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

    /// Feed a fully-transparent (all-zero, premultiplied) fixture into
    /// EVERY `Texture2D` input of `prim` and read back its first
    /// `Texture2D` output as raw `Rgba16Float` bytes.
    ///
    /// Returns `None` when `prim` isn't a texture→texture effect (no
    /// texture input or no texture output) or the graph fails to
    /// compile/bind — the alpha-contract sweep reports those as
    /// "skipped", never as passes.
    ///
    /// The contract under test: an effect handed nothing (alpha 0
    /// everywhere) must output nothing (alpha stays 0). Any shader that
    /// hardcodes the output alpha to 1.0 manufactures opacity from a
    /// transparent layer — that's the bug class this probe detects.
    pub fn run_transparent_probe(&self, prim: Box<dyn EffectNode>) -> Option<Vec<u8>> {
        let tex_inputs: Vec<String> = prim
            .inputs()
            .iter()
            .filter(|p| port_is_texture(&p.ty))
            .map(|p| p.name.to_string())
            .collect();
        if tex_inputs.is_empty() {
            return None;
        }
        let out_port: String = prim
            .outputs()
            .iter()
            .find(|p| port_is_texture(&p.ty))
            .map(|p| p.name.to_string())?;

        let mut graph = Graph::new();
        let prim_id = graph.add_node(prim);
        let final_out = graph.add_node(Box::new(FinalOutput::new()));

        // One Source per texture input — every one fed the transparent
        // fixture below, so the effect sees "nothing" on all texture ports.
        let mut sources: Vec<NodeInstanceId> = Vec::with_capacity(tex_inputs.len());
        for port in &tex_inputs {
            let src = graph.add_node(Box::new(Source::new()));
            let port_leak: &'static str = leak_static_str(port.clone());
            graph.connect((src, "out"), (prim_id, port_leak)).ok()?;
            sources.push(src);
        }
        let out_leak: &'static str = leak_static_str(out_port);
        graph.connect((prim_id, out_leak), (final_out, "in")).ok()?;

        let plan = compile(&graph).ok()?;

        // All-zero fixture = transparent black, premultiplied. Copy it into
        // a private RT per source so the executor samples it through the
        // same memory mode production graphs use.
        let zeros = vec![f16::from_f32(0.0); (self.width * self.height * 4) as usize];
        let transparent = self.upload_f16_rgba("alpha-probe-transparent", &zeros);

        let mut backend =
            MetalBackend::new(Arc::clone(&self.device), self.width, self.height, self.format);
        let mut copy_enc = self.device.create_encoder("alpha-probe-copy-in");
        let mut rts: Vec<RenderTarget> = Vec::with_capacity(sources.len());
        {
            let mut gpu = RendererGpuEncoder::new(&mut copy_enc, &self.device);
            for _ in &sources {
                let rt = self.make_target("alpha-probe-source");
                gpu.copy_texture_to_texture(&transparent, &rt.texture, self.width, self.height);
                rts.push(rt);
            }
        }
        copy_enc.commit_and_wait_completed();

        let mut rts_iter = rts.into_iter();
        for src in &sources {
            let res = resource_for_output(&plan, *src, "out");
            let rt = rts_iter.next()?;
            backend.pre_bind_texture_2d(res, rt);
        }
        let prim_output_slot = Slot(backend.slot_count());

        let frame_time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };

        let mut native_enc = self.device.create_encoder("alpha-probe-render");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &self.device);
            exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let prim_tex = exec.backend().texture_2d(prim_output_slot)?;
        Some(self.readback(prim_tex))
    }

    /// Read a texture's contents back to host memory as raw bytes.
    /// Allocates a shared (CPU-visible) Metal buffer, issues a
    /// texture→buffer copy, commits, waits, and snapshots the bytes.
    pub fn readback(&self, texture: &GpuTexture) -> Vec<u8> {
        let bytes_per_row = self.width * BYTES_PER_PIXEL;
        let total_bytes = u64::from(self.height * bytes_per_row);
        let buf = self.device.create_buffer_shared(total_bytes);

        let mut enc = self.device.create_encoder("gpu-proof-readback");
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

/// Look up the `ResourceId` of a node's named output port in an
/// `ExecutionPlan`. Mirrors `output_resource` in
/// `node_graph/primitives/compose.rs` — same signature, same
/// fall-through panic message.
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
/// used in tests; the leak is bounded by the (small, finite) number of
/// graphs built per process.
fn leak_static_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// True for the texture port kinds the alpha-contract sweep treats as
/// image data — plain `Texture2D` and typed-channel `Texture2DTyped`.
pub fn port_is_texture(ty: &PortType) -> bool {
    matches!(ty, PortType::Texture2D | PortType::Texture2DTyped(_))
}
