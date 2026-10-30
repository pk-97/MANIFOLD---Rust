//! `node.grid_indices` — the triangle indices of a `cols` × `rows` grid of
//! vertices laid out row-major (`row * cols + col`): two triangles a quad,
//! counter-clockwise seen from +Y with columns along +x and rows along +z,
//! in the corner order `node.make_triangles` writes. A grid drawn through
//! these shades each vertex once instead of once per triangle corner.
//!
//! The indices are written once per grid size into a buffer the node owns.
//! Every other frame declares the output unchanged, so its content version
//! holds and a draw over it keeps its ray-tracing topology while the grid's
//! positions move.

use std::borrow::Cow;

use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

const SHADER: &str = "
struct GridUniforms {
    cols: u32,
    rows: u32,
    count: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> u: GridUniforms;
@group(0) @binding(1) var<storage, read_write> indices: array<u32>;

// Quad (c, r) writes (c,r) (c,r+1) (c+1,r), then (c,r+1) (c+1,r+1) (c+1,r).
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.count {
        return;
    }
    let quad = i / 6u;
    let corner = i % 6u;
    let quads_x = u.cols - 1u;
    let col = quad % quads_x + select(0u, 1u, corner == 2u || corner >= 4u);
    let row = quad / quads_x + select(0u, 1u, corner == 1u || corner == 3u || corner == 4u);
    indices[i] = row * u.cols + col;
}
";
const THREADS: u32 = 256;
const DEFAULT_SIDE: u32 = 256;

/// Field order identical to the shader's `GridUniforms`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GridUniforms {
    cols: u32,
    rows: u32,
    count: u32,
    pad: u32,
}

fn side(params: &ParamValues, name: &str) -> u32 {
    match params.get(name) {
        Some(ParamValue::Float(n)) => n.round().clamp(2.0, 4096.0) as u32,
        _ => DEFAULT_SIDE,
    }
}

/// Indices for a `cols` × `rows` grid: six per quad.
fn index_count(cols: u32, rows: u32) -> u32 {
    (cols - 1) * (rows - 1) * 6
}

/// The size the indices were last written for, in the executor that holds
/// them: a state-carrying rebuild restarts content versions, so an old key
/// never matches.
#[derive(Clone, Copy, PartialEq)]
struct Written {
    cols: u32,
    rows: u32,
    rebuild_epoch: u64,
}

manifold_node_engine::primitive! {
    name: GridIndices,
    type_id: "node.grid_indices",
    purpose: "Emit the u32 triangle indices of a cols × rows row-major vertex grid, two counter-clockwise triangles per quad seen from +Y (columns along +x, rows along +z), the corner order node.make_triangles uses. Wire them with the grid's vertices into a scene object's indices so each grid vertex is shaded once. Written once per size; unchanged otherwise.",
    inputs: {},
    outputs: {
        indices: Array(u32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("cols"),
            label: "Columns",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rows"),
            label: "Rows",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "cols × rows must match the vertex grid the indices draw. Capacity is (cols - 1) × (rows - 1) × 6 indices: a 512 × 512 grid is about 1.6M indices, 6 MB.",
    examples: [],
    picker: { label: "Grid Indices", category: Atom },
    summary: "Lists the triangles of a grid of points, so the grid draws as a solid surface with each point worked out once.",
    category: Geometry3D,
    role: Source,
    aliases: ["grid indices", "index buffer", "grid triangles"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        kernel: Option<GpuComputePipeline> = None,
        buffer: Option<GpuBuffer> = None,
        written: Option<Written> = None,
    },
}

impl Primitive for GridIndices {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.kernel.get_or_insert_with(|| {
            device.create_compute_pipeline(SHADER, "main", "node.grid_indices")
        });
    }

    fn provides_array_output(&self, port: &str) -> bool {
        port == "indices"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "indices")
            .then_some(self.buffer.as_ref())
            .flatten()
    }

    fn array_output_capacity(
        &self,
        port: &str,
        params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "indices").then(|| index_count(side(params, "cols"), side(params, "rows")))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let written = Written {
            cols: side(ctx.params, "cols"),
            rows: side(ctx.params, "rows"),
            rebuild_epoch: ctx.rebuild_epoch,
        };
        if self.written == Some(written) && ctx.outputs_retained() {
            ctx.mark_outputs_unchanged();
            return;
        }
        let count = index_count(written.cols, written.rows);
        let device = ctx.gpu_encoder().device;
        let pipeline = self.kernel.get_or_insert_with(|| {
            device.create_compute_pipeline(SHADER, "main", "node.grid_indices")
        });
        let bytes = u64::from(count) * 4;
        if self.buffer.as_ref().is_none_or(|b| b.size != bytes) {
            self.buffer = None;
            self.written = None;
            match device.try_create_buffer(bytes, BufferUse::Gpu) {
                Ok(buffer) => self.buffer = Some(buffer),
                Err(error) => {
                    ctx.error(format!(
                        "Grid Indices: the device cannot give {bytes} bytes for the indices: {error}"
                    ));
                    return;
                }
            }
        }
        let Some(buffer) = self.buffer.as_ref() else {
            return;
        };
        let uniforms = GridUniforms {
            cols: written.cols,
            rows: written.rows,
            count,
            pad: 0,
        };
        ctx.gpu_encoder().native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer,
                    offset: 0,
                },
            ],
            [count.div_ceil(THREADS), 1, 1],
            "node.grid_indices",
        );
        self.written = Some(written);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    /// A 3-column, 4-row grid: each quad's six indices in make_triangles'
    /// corner order, and the first triangle on a flat grid faces +Y.
    #[test]
    fn three_by_four_grid_matches_make_triangles_order() {
        let (cols, rows) = (3, 4);
        let count = index_count(cols, rows);
        let device = manifold_gpu::testkit::test_device();
        let pipeline = device.create_compute_pipeline(SHADER, "main", "grid-indices-test");
        let out = device.create_buffer(u64::from(count) * 4, BufferUse::Readback);
        let uniforms = GridUniforms {
            cols,
            rows,
            count,
            pad: 0,
        };
        let mut enc = device.create_encoder("grid-indices-test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &out,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "grid-indices-test",
        );
        enc.commit_and_wait().expect("GPU work completes");
        let ptr = out.mapped_ptr().expect("readback buffer");
        let got = unsafe { std::slice::from_raw_parts(ptr as *const u32, count as usize) };
        #[rustfmt::skip]
        let want: [u32; 36] = [
            0, 3, 1, 3, 4, 1,   1, 4, 2, 4, 5, 2,
            3, 6, 4, 6, 7, 4,   4, 7, 5, 7, 8, 5,
            6, 9, 7, 9, 10, 7,  7, 10, 8, 10, 11, 8,
        ];
        assert_eq!(got, want);
        // Vertex (c, r) sits at x = c, z = r: the first triangle's normal.
        let at = |i: u32| [(i % cols) as f32, 0.0, (i / cols) as f32];
        let (a, b, c) = (at(got[0]), at(got[1]), at(got[2]));
        let (u, v) = ([b[0] - a[0], b[2] - a[2]], [c[0] - a[0], c[2] - a[2]]);
        assert!(
            u[1] * v[0] - u[0] * v[1] > 0.0,
            "counter-clockwise seen from +Y"
        );
    }
}
