//! Draw batch descriptors shared by both backends: what one depth-tested
//! draw covers and the attachments of the pass that runs a batch of them.

use crate::{
    GpuBinding, GpuBuffer, GpuDepthStencilState, GpuPrimitiveType, GpuRenderPipeline, GpuTexture,
    GpuTriangleFillMode,
};

/// How many vertices or indices and instances one batch draw covers.
#[derive(Clone, Copy)]
pub enum DrawCount<'a> {
    Direct {
        vertices: u32,
        instances: u32,
    },
    /// Metal's four-word draw arguments (vertex count, instance count, vertex
    /// start, base instance) at `offset`, written on the GPU earlier in the
    /// same command buffer: the count is known only to the GPU.
    Indirect {
        args: &'a GpuBuffer,
        offset: u64,
    },
    /// Direct indexed draw. The index buffer contains `u32` indices starting
    /// at byte offset zero.
    IndexedDirect {
        indices: &'a GpuBuffer,
        index_count: u32,
        instances: u32,
    },
    /// Indexed draw whose five-word Metal argument block (index count,
    /// instance count, index start, base vertex, base instance) is written by
    /// the GPU earlier in the same command buffer. Callers commonly reserve
    /// 32-byte blocks even though Metal consumes 20 bytes, keeping subsequent
    /// argument offsets aligned for the surrounding argument layout.
    IndexedIndirect {
        indices: &'a GpuBuffer,
        args: &'a GpuBuffer,
        offset: u64,
    },
}

impl DrawCount<'_> {
    pub(crate) fn is_empty(&self) -> bool {
        matches!(
            self,
            DrawCount::Direct { vertices: 0, .. }
                | DrawCount::Direct { instances: 0, .. }
                | DrawCount::IndexedDirect { index_count: 0, .. }
                | DrawCount::IndexedDirect { instances: 0, .. }
        )
    }
}

/// One depth-tested mesh in a [`GpuEncoder::draw_instanced_depth_msaa_batch`](crate::GpuEncoder::draw_instanced_depth_msaa_batch)
/// batch: its own material pipeline, its own bindings, its own vertex count.
/// Construct via [`GpuEncoder::depth_msaa_draw`](crate::GpuEncoder::depth_msaa_draw) (fill),
/// [`GpuEncoder::depth_msaa_draw_fill_mode`](crate::GpuEncoder::depth_msaa_draw_fill_mode) (wireframe), or
/// [`GpuEncoder::depth_msaa_draw_points`](crate::GpuEncoder::depth_msaa_draw_points) (point topology).
#[derive(Clone, Copy)]
pub struct DepthMsaaDraw<'a> {
    pub(crate) pipeline: &'a GpuRenderPipeline,
    pub(crate) bindings: &'a [GpuBinding<'a>],
    pub(crate) count: DrawCount<'a>,
    /// Triangle fill mode for this draw (SCENE_RENDER_MODE_DESIGN.md D6).
    /// `Lines` = wireframe. Only the COLOUR batch entries read this; the
    /// depth-only entries (`draw_instanced_depth_only_batch` — shadow maps,
    /// opaque-depth prepass) force `Fill` per draw regardless of what a
    /// `DepthMsaaDraw` carries (INV-R3: lines-only depth would break
    /// occlusion and shadows).
    pub(crate) fill_mode: GpuTriangleFillMode,
    /// Primitive topology for this draw (SCENE_RENDER_MODE_DESIGN.md D8:
    /// `Point` = Points mode, the same vertex buffers drawn as points).
    /// Only the COLOUR batch entries read this; the depth-only batches
    /// force `Triangle` — same INV-R3 argument as `fill_mode` (point-only
    /// depth would break occlusion and shadows).
    pub(crate) primitive: GpuPrimitiveType,
    /// Skip back faces (counter-clockwise front). Only for closed,
    /// outward-wound, opaque meshes: their nearest surface is always a front
    /// face, so colour and depth match the two-sided draw at half the work.
    pub(crate) cull_back: bool,
}

impl<'a> DepthMsaaDraw<'a> {
    /// Skip back faces when `cull` (see [`DepthMsaaDraw::cull_back`]).
    pub fn cull_back_faces(mut self, cull: bool) -> Self {
        self.cull_back = cull;
        self
    }

    /// Use `indices` as the `u32` index buffer, preserving whether this draw's
    /// count is direct or GPU-written. Direct counts use the constructor's
    /// vertex count as the index count.
    pub fn indexed(mut self, indices: &'a GpuBuffer) -> Self {
        self.count = match self.count {
            DrawCount::Direct {
                vertices,
                instances,
            } => DrawCount::IndexedDirect {
                indices,
                index_count: vertices,
                instances,
            },
            DrawCount::Indirect { args, offset } => DrawCount::IndexedIndirect {
                indices,
                args,
                offset,
            },
            DrawCount::IndexedDirect {
                index_count,
                instances,
                ..
            } => DrawCount::IndexedDirect {
                indices,
                index_count,
                instances,
            },
            DrawCount::IndexedIndirect { args, offset, .. } => DrawCount::IndexedIndirect {
                indices,
                args,
                offset,
            },
        };
        self
    }

    /// Draw with GPU-written arguments instead of a CPU count: the Metal
    /// four-word draw arguments at `offset` in `args`, or the five-word
    /// indexed arguments when [`Self::indexed`] was used first.
    pub fn indirect(mut self, args: &'a GpuBuffer, offset: u64) -> Self {
        self.count = match self.count {
            DrawCount::Direct { .. } | DrawCount::Indirect { .. } => {
                DrawCount::Indirect { args, offset }
            }
            DrawCount::IndexedDirect { indices, .. } => DrawCount::IndexedIndirect {
                indices,
                args,
                offset,
            },
            DrawCount::IndexedIndirect { indices, .. } => DrawCount::IndexedIndirect {
                indices,
                args,
                offset,
            },
        };
        self
    }
}

/// Descriptor for
/// [`GpuEncoder::draw_instanced_depth_msaa_batch_desc`](crate::GpuEncoder::draw_instanced_depth_msaa_batch_desc) — the desc-struct
/// seam that lets ONE batch entry point grow optional G-buffer attachments
/// instead of a parallel `_with_depth` function per attachment combination.
/// [`GpuEncoder::draw_instanced_depth_msaa_batch`](crate::GpuEncoder::draw_instanced_depth_msaa_batch) (unchanged signature)
/// forwards here with `depth_resolve: None, aux_color: &[]` — byte-identical
/// to the pre-D3 pass (I1/I4).
/// One aux MRT attachment in a [`DepthMsaaPassDesc`]: memoryless MSAA
/// texture + single-sample resolve target + this attachment's clear color.
/// Per-attachment clear because the semantics differ: velocity clears to 0
/// (no motion), ao_mask clears to 1 (background keeps full screen-space AO —
/// RAYTRACING_DESIGN.md section 12 AM1).
#[derive(Clone, Copy)]
pub struct AuxColorAttachment<'a> {
    pub msaa: &'a GpuTexture,
    pub resolve: &'a GpuTexture,
    pub clear: [f64; 4],
}

pub struct DepthMsaaPassDesc<'a> {
    pub msaa_color: &'a GpuTexture,
    pub resolve_target: &'a GpuTexture,
    pub msaa_depth: &'a GpuTexture,
    /// `Some(tex)` → the depth attachment stores `MultisampleResolve`
    /// (filter `Sample0` — D2: deterministic, matches a single-sample
    /// render, unlike `Min`/`Max`) into this single-sample `Depth32Float`
    /// texture. The resolve target must have the same `Depth32Float` format
    /// as `msaa_depth`; callers can then copy it to an `R32Float` graph output.
    /// `None` → `DontCare`, exactly today's memoryless depth.
    pub depth_resolve: Option<&'a GpuTexture>,
    /// Extra MRT color attachments (index 1..). Order must match the
    /// pipeline's `aux_color_formats` (velocity, then ao_mask). Empty slice
    /// produces exactly the pre-D3 single-color-attachment pass.
    pub aux_color: &'a [AuxColorAttachment<'a>],
    pub depth_stencil_state: &'a GpuDepthStencilState,
    /// An optional second draw group,
    /// run immediately after `draws` within the SAME render pass — no
    /// second clear, no new pass, just a `setDepthStencilState` switch
    /// before the second group's draw calls. Used for the sorted
    /// transparent pass (depth test on, write off) layered over the
    /// opaque pass's already-resolved-on-tile depth. `None` reproduces
    /// exactly today's single-group pass (I1).
    pub second_pass: Option<(&'a GpuDepthStencilState, &'a [DepthMsaaDraw<'a>])>,
}
