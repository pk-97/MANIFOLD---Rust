//! The single pipeline-get for the standalone codegen path (BUG-elb0).
//!
//! Every barrier-free per-element atom builds its runtime kernel from its
//! `wgsl_body` spec via `standalone_for_spec` — never from hand-authored
//! WGSL. This helper concentrates that rule in one enforcement point: the
//! 150+ call sites it replaces each re-stated it in a comment.

use manifold_gpu::{GpuComputePipeline, GpuDevice};

use crate::node_graph::freeze::codegen;
use crate::node_graph::primitive::Primitive;

/// Get-or-create the primitive's standalone codegen compute pipeline,
/// labelled with the primitive's TYPE_ID. Replaces the per-file
/// `pipeline.get_or_insert_with(|| device.create_compute_pipeline(
/// &standalone_for_spec::<Self>().expect(...), ENTRY, "<type_id>"))`
/// closure — the only thing those closures ever varied was the label.
pub fn standalone_pipeline<'a, P: Primitive>(
    slot: &'a mut Option<GpuComputePipeline>,
    device: &GpuDevice,
) -> &'a mut GpuComputePipeline {
    slot.get_or_insert_with(|| {
        device.create_compute_pipeline(
            &codegen::standalone_for_spec::<P>()
                .unwrap_or_else(|e| panic!("{} standalone codegen: {e:?}", P::TYPE_ID)),
            codegen::ENTRY,
            P::TYPE_ID,
        )
    })
}

/// Elements of type `T` that fit in `bytes`, clamped to `requested` — the
/// particle-atom sizing prologue (BUG-uwgn): replaces
/// `let sz = size_of::<T>() as u64; let cap = (buf.size / sz) as u32;
/// let n = n.min(cap);`. A zero result means the caller should
/// early-return before dispatching (the guard stays at the call site —
/// some atoms skip, some fall through to a clear).
pub fn active_elements<T>(bytes: u64, requested: u32) -> u32 {
    requested.min((bytes / std::mem::size_of::<T>() as u64) as u32)
}

/// The canonical texture-path binding order (BUG-uwgn cluster 4): uniform(0),
/// sampled input textures in port order, the shared sampler, one 2D
/// storage-write output. The freeze codegen emits exactly this layout for
/// texture-path atoms (binding_contract_tests reflects every generated kernel
/// and pins the census); call sites that match it build their tail through
/// [`dispatch_standalone_2d`] instead of an inline binding array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandaloneSlot {
    Uniform,
    TexIn,
    Sampler,
    TexOut,
}

/// Slot sequence for the canonical tail — pure so the binding-contract test
/// can check it against the reflected kernel without a device.
pub fn standalone_2d_slots(n_textures: usize, has_sampler: bool) -> Vec<StandaloneSlot> {
    let mut slots = Vec::with_capacity(n_textures + 3);
    slots.push(StandaloneSlot::Uniform);
    slots.extend(std::iter::repeat_n(StandaloneSlot::TexIn, n_textures));
    if has_sampler {
        slots.push(StandaloneSlot::Sampler);
    }
    slots.push(StandaloneSlot::TexOut);
    slots
}

/// Stack capacity checked against every canonical kernel by the binding census.
pub(crate) const STANDALONE_2D_MAX_BINDINGS: usize = 32;

/// Canonical texture-path dispatch tail: builds the bindings in
/// [`standalone_2d_slots`] order and dispatches a 2D grid over `out`
/// (16×16 workgroups, the texture path's constant). Qualifying atoms only —
/// the reflected kernel must be uniform(0) + sampled textures + at most one
/// sampler + exactly one 2D storage-write output (no storage buffers, no 3D,
/// no multi-output); `binding_contract_tests` is the census that defines the
/// set. Anything else keeps its hand-written tail.
pub fn dispatch_standalone_2d(
    gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
    pipeline: &manifold_gpu::GpuComputePipeline,
    uniform_bytes: &[u8],
    textures: &[&manifold_gpu::GpuTexture],
    sampler: Option<&manifold_gpu::GpuSampler>,
    out: &manifold_gpu::GpuTexture,
    label: &str,
) {
    // The canonical standalone texture path has one uniform, one output,
    // and the reflected input texture ports (currently well below this
    // bound). Keep the storage fixed so dispatches never allocate.
    let binding_count = textures.len() + 2 + usize::from(sampler.is_some());
    assert!(binding_count <= STANDALONE_2D_MAX_BINDINGS, "standalone 2D binding count exceeds stack capacity");
    let mut bindings: [_; STANDALONE_2D_MAX_BINDINGS] = std::array::from_fn(|_| manifold_gpu::GpuBinding::Bytes {
        binding: 0,
        data: &[],
    });
    let mut len = 0;
    bindings[len] = manifold_gpu::GpuBinding::Bytes {
        binding: 0,
        data: uniform_bytes,
    };
    len += 1;
    for (i, tex) in textures.iter().enumerate() {
        bindings[len] = manifold_gpu::GpuBinding::Texture {
            binding: (i + 1) as u32,
            texture: tex,
        };
        len += 1;
    }
    let mut next = textures.len() as u32 + 1;
    if let Some(s) = sampler {
        bindings[len] = manifold_gpu::GpuBinding::Sampler {
            binding: next,
            sampler: s,
        };
        len += 1;
        next += 1;
    }
    bindings[len] = manifold_gpu::GpuBinding::Texture {
        binding: next,
        texture: out,
    };
    len += 1;
    gpu.native_enc.dispatch_compute(
        pipeline,
        &bindings[..len],
        [out.width.div_ceil(16), out.height.div_ceil(16), 1],
        label,
    );
}
