//! What one draw covers and the pass it runs in, shared by both backends.
//! Every render-pass draw goes through `GpuEncoder::draw(&DrawPass, &[Draw],
//! label)`; only the UI's long-lived pass (`begin_render_pass`) is separate.

use crate::{
    GpuBinding, GpuBuffer, GpuDepthStencilState, GpuLoadAction, GpuPrimitiveType,
    GpuRenderPipeline, GpuTexture, GpuTriangleFillMode,
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

/// One draw in a pass: its pipeline, bindings and count, and how it
/// rasterises. Front faces wind counter-clockwise on every draw.
#[derive(Clone, Copy)]
pub struct Draw<'a> {
    pub(crate) pipeline: &'a GpuRenderPipeline,
    pub(crate) bindings: &'a [GpuBinding<'a>],
    pub(crate) count: DrawCount<'a>,
    /// `Lines` draws wireframe. A depth-only pass always fills: lines-only
    /// depth would break occlusion and shadows.
    pub(crate) fill_mode: GpuTriangleFillMode,
    /// `Point` draws the same vertices as points. A depth-only pass always
    /// draws triangles, for the same reason.
    pub(crate) primitive: GpuPrimitiveType,
    /// Skip back faces. Only for closed, outward-wound, opaque meshes: their
    /// nearest surface is always a front face, so colour and depth match the
    /// two-sided draw at half the work.
    pub(crate) cull_back: bool,
    /// Constant, slope-scaled and clamp depth bias.
    pub(crate) depth_bias: Option<(f32, f32, f32)>,
    /// A vertex buffer and its offset, for pipelines with a vertex layout.
    pub(crate) vertices: Option<(&'a GpuBuffer, u64)>,
}

impl<'a> Draw<'a> {
    /// `vertices` vertices of `instances` instances, filled triangles.
    pub fn new(
        pipeline: &'a GpuRenderPipeline,
        bindings: &'a [GpuBinding<'a>],
        vertices: u32,
        instances: u32,
    ) -> Self {
        Self::counted(
            pipeline,
            bindings,
            DrawCount::Direct {
                vertices,
                instances,
            },
        )
    }

    /// One triangle covering the target, made by the vertex shader.
    pub fn fullscreen(pipeline: &'a GpuRenderPipeline, bindings: &'a [GpuBinding<'a>]) -> Self {
        Self::new(pipeline, bindings, 3, 1)
    }

    /// A draw of `count`, direct, indexed or GPU-written.
    pub fn counted(
        pipeline: &'a GpuRenderPipeline,
        bindings: &'a [GpuBinding<'a>],
        count: DrawCount<'a>,
    ) -> Self {
        Self {
            pipeline,
            bindings,
            count,
            fill_mode: GpuTriangleFillMode::Fill,
            primitive: GpuPrimitiveType::Triangle,
            cull_back: false,
            depth_bias: None,
            vertices: None,
        }
    }

    /// Rasterise triangles as `fill_mode` (`Lines` is wireframe).
    pub fn fill_mode(mut self, fill_mode: GpuTriangleFillMode) -> Self {
        self.fill_mode = fill_mode;
        self
    }

    /// Draw with `primitive` topology. Points need a pipeline built with a
    /// point-size location.
    pub fn primitive(mut self, primitive: GpuPrimitiveType) -> Self {
        self.primitive = primitive;
        self
    }

    /// Offset depth by `constant` plus `slope` times the depth slope, at most
    /// `clamp`.
    pub fn depth_bias(mut self, constant: f32, slope: f32, clamp: f32) -> Self {
        self.depth_bias = Some((constant, slope, clamp));
        self
    }

    /// Read vertices from `buffer` at `offset` through the pipeline's
    /// vertex layout.
    pub fn vertex_buffer(mut self, buffer: &'a GpuBuffer, offset: u64) -> Self {
        self.vertices = Some((buffer, offset));
        self
    }

    /// Skip back faces when `cull` (see [`Draw::cull_back`]).
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

/// One extra MSAA colour attachment (index 1..) of a pass: the multisample
/// texture, its single-sample resolve target and its own clear colour.
/// Velocity clears to 0 (no motion), the AO mask to 1 (the background keeps
/// full screen-space AO).
#[derive(Clone, Copy)]
pub struct AuxColorAttachment<'a> {
    pub msaa: &'a GpuTexture,
    pub resolve: &'a GpuTexture,
    pub clear: [f64; 4],
}

/// Colour attachment 0 of a pass. It always stores, by resolving into
/// `resolve` when the texture is multisampled. A clear is transparent black,
/// so an offscreen layer keys where nothing draws.
#[derive(Clone, Copy)]
pub struct ColorTarget<'a> {
    pub texture: &'a GpuTexture,
    pub resolve: Option<&'a GpuTexture>,
    pub load: GpuLoadAction,
}

/// The depth attachment of a pass and the depth test its draws use. A clear
/// writes the state's `clear_depth`.
#[derive(Clone, Copy)]
pub struct DepthTarget<'a> {
    pub texture: &'a GpuTexture,
    pub state: &'a GpuDepthStencilState,
    pub load: GpuLoadAction,
    pub store: DepthStore<'a>,
}

/// What a pass keeps of its depth.
#[derive(Clone, Copy)]
pub enum DepthStore<'a> {
    Store,
    /// Scratch depth, such as memoryless MSAA depth, that dies with the pass.
    Discard,
    /// Resolve sample 0 of the MSAA depth into this single-sample
    /// `Depth32Float` texture: deterministic, as a single-sample render.
    Resolve(&'a GpuTexture),
}

/// Where one render pass draws and how its attachments load and store.
#[derive(Clone, Copy)]
pub struct DrawPass<'a> {
    /// `None` for a depth-only pass, such as a shadow map.
    pub color: Option<ColorTarget<'a>>,
    /// Extra colour attachments; their order matches the pipelines'
    /// `aux_color_formats`.
    pub aux_color: &'a [AuxColorAttachment<'a>],
    pub depth: Option<DepthTarget<'a>>,
    /// The pixel rectangle `(x, y, width, height)` the draws cover. `None`
    /// covers the whole target.
    pub viewport: Option<(f32, f32, f32, f32)>,
    /// Draws run after the main ones in the same pass with another depth
    /// state: no clear, no new pass. The sorted transparent group tests
    /// against the opaque group's depth without writing it.
    pub second_group: Option<(&'a GpuDepthStencilState, &'a [Draw<'a>])>,
}

impl<'a> DrawPass<'a> {
    /// A single-sample colour pass into `texture`.
    pub fn color(texture: &'a GpuTexture, load: GpuLoadAction) -> Self {
        Self::with_color(ColorTarget {
            texture,
            resolve: None,
            load,
        })
    }

    /// A multisample colour pass into `texture`, resolved into `resolve`.
    pub fn msaa(texture: &'a GpuTexture, resolve: &'a GpuTexture, load: GpuLoadAction) -> Self {
        Self::with_color(ColorTarget {
            texture,
            resolve: Some(resolve),
            load,
        })
    }

    /// A depth-only pass into `texture`, which stores.
    pub fn depth_only(
        texture: &'a GpuTexture,
        state: &'a GpuDepthStencilState,
        load: GpuLoadAction,
    ) -> Self {
        Self {
            color: None,
            aux_color: &[],
            depth: Some(DepthTarget {
                texture,
                state,
                load,
                store: DepthStore::Store,
            }),
            viewport: None,
            second_group: None,
        }
    }

    fn with_color(color: ColorTarget<'a>) -> Self {
        Self {
            color: Some(color),
            aux_color: &[],
            depth: None,
            viewport: None,
            second_group: None,
        }
    }

    /// Add a depth attachment.
    pub fn depth(
        mut self,
        texture: &'a GpuTexture,
        state: &'a GpuDepthStencilState,
        load: GpuLoadAction,
        store: DepthStore<'a>,
    ) -> Self {
        self.depth = Some(DepthTarget {
            texture,
            state,
            load,
            store,
        });
        self
    }

    /// Cover only `(x, y, width, height)`.
    pub fn viewport(mut self, viewport: (f32, f32, f32, f32)) -> Self {
        self.viewport = Some(viewport);
        self
    }

    /// The target's size in pixels: the colour attachment's, else the depth's.
    pub fn size(&self) -> (u32, u32) {
        match (&self.color, &self.depth) {
            (Some(color), _) => (color.texture.width, color.texture.height),
            (None, Some(depth)) => (depth.texture.width, depth.texture.height),
            (None, None) => panic!("a draw pass needs a colour or a depth attachment"),
        }
    }
}
