//! `node.matter_fill` — seed a matter domain's points: a pool of the lowest authored cells plus one
//! box, in cell order, the box kept to a shaped fill's shape.

use manifold_gpu::BufferUse;
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer};

use super::matter_common::{COLLIDER_BVH, MATTER_TREE};
use crate::matter::MatterPoint;
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::Vec4Vertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::ports::EXACT_F32_COUNT;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_water_liquid::bodies::{
    LIQUID_COLLIDER, LIQUID_FILL_SHAPE, LIQUID_POSE, LiquidBody, LiquidShape,
};

/// Most cells per axis the `fill_cells_*` wires may carry.
const MAX_FILL_CELLS: u32 = 4096;

/// The grid box a fill seeds, from the domain's `fill_min_*`, `cell_size` and
/// `fill_cells_*` scalars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FillGrid {
    /// The box's first corner.
    pub(crate) min: [f32; 3],
    pub(crate) cell_size: f32,
    pub(crate) cells: [u32; 3],
}

impl FillGrid {
    /// The grid box on the wires `read` finds (`scalar_or_param`; the
    /// defaults are the 4 m Dam Break box at cell size 0.0625 m). A wire no
    /// domain could have produced is refused by name: cell counts must be
    /// whole and from 1 to [`MAX_FILL_CELLS`], the cell size positive, the
    /// corner finite.
    pub(crate) fn read(read: impl Fn(&str, f32) -> f32) -> Result<Self, String> {
        let cells = |name: &str| {
            let value = read(name, 64.0);
            if value.fract() == 0.0 && (1.0..=MAX_FILL_CELLS as f32).contains(&value) {
                Ok(value as u32)
            } else {
                Err(format!(
                    "fill wire {name} is {value}; it must be a whole cell count from 1 to {MAX_FILL_CELLS}"
                ))
            }
        };
        let cells = [
            cells("fill_cells_x")?,
            cells("fill_cells_y")?,
            cells("fill_cells_z")?,
        ];
        let cell_size = read("cell_size", 0.0625);
        if !(cell_size.is_finite() && cell_size > 0.0) {
            return Err(format!(
                "fill wire cell_size is {cell_size}; it must be positive"
            ));
        }
        let corner = |name: &str, default: f32| {
            let value = read(name, default);
            value
                .is_finite()
                .then_some(value)
                .ok_or_else(|| format!("fill wire {name} is {value}; it must be finite"))
        };
        let min = [
            corner("fill_min_x", -2.0)?,
            corner("fill_min_y", 0.0)?,
            corner("fill_min_z", -2.0)?,
        ];
        Ok(Self {
            min,
            cell_size,
            cells,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FillUniforms {
    fill_min_x: f32,
    fill_min_y: f32,
    fill_min_z: f32,
    cell_size: f32,
    fill_cells_x: i32,
    fill_cells_y: i32,
    fill_cells_z: i32,
    pool_cells: i32,
    column_x0: i32,
    column_x1: i32,
    column_y0: i32,
    column_y1: i32,
    column_z0: i32,
    column_z1: i32,
    points_per_cell: i32,
    seed: i32,
    body_count: i32,
    epoch: i32,
    fill_region: i32,
    dispatch_count: u32,
}

manifold_node_engine::primitive! {
    name: MatterFill,
    type_id: "node.matter_fill",
    purpose: "Seed material points at rest in cell order: every authored cell below the pool height, then one box of cells above it. Each cell holds 2³ or 3³ points, one per sub-cell, jittered by a seeded hash; ids count births from 1. A shaped fill (fill_region's row of regions) keeps only the box seeds inside its shape. Outputs the points and their count.",
    inputs: {
        fill_min_x: ScalarF32 optional, fill_min_y: ScalarF32 optional, fill_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        fill_cells_x: ScalarF32 optional, fill_cells_y: ScalarF32 optional, fill_cells_z: ScalarF32 optional,
        pool_cells: ScalarF32 optional,
        column_x0: ScalarF32 optional, column_x1: ScalarF32 optional,
        column_y0: ScalarF32 optional, column_y1: ScalarF32 optional,
        column_z0: ScalarF32 optional, column_z1: ScalarF32 optional,
        points_per_cell: ScalarF32 optional,
        seed: ScalarF32 optional,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        collider_nodes: Array(Vec4Vertex) optional,
        collider_triangles: Array(Vec4Vertex) optional,
        regions: Array(LiquidBody) optional,
        fill_region: ScalarF32 optional,
        body_count: ScalarF32 optional,
        epoch: ScalarF32 optional,
    },
    outputs: {
        points: Array(MatterPoint),
        count: ScalarF32,
    },
    params: [
        ParamDef { name: Cow::Borrowed("fill_min_x"), label: "Fill Min X", ty: ParamType::Float, default: ParamValue::Float(-2.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_min_y"), label: "Fill Min Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_min_z"), label: "Fill Min Z", ty: ParamType::Float, default: ParamValue::Float(-2.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_cells_x"), label: "Fill Cells X", ty: ParamType::Int, default: ParamValue::Float(64.0), range: Some((1.0, MAX_FILL_CELLS as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_cells_y"), label: "Fill Cells Y", ty: ParamType::Int, default: ParamValue::Float(64.0), range: Some((1.0, MAX_FILL_CELLS as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_cells_z"), label: "Fill Cells Z", ty: ParamType::Int, default: ParamValue::Float(64.0), range: Some((1.0, MAX_FILL_CELLS as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pool_cells"), label: "Pool Height (cells)", ty: ParamType::Int, default: ParamValue::Float(3.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("column_x0"), label: "Box Min X (cell)", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("column_x1"), label: "Box Max X (cell)", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("column_y0"), label: "Box Min Y (cell)", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("column_y1"), label: "Box Max Y (cell)", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("column_z0"), label: "Box Min Z (cell)", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("column_z1"), label: "Box Max Z (cell)", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("points_per_cell"), label: "Particles per Cell", ty: ParamType::Int, default: ParamValue::Float(8.0), range: Some((8.0, 27.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("seed"), label: "Seed", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_215.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, MAX_LIQUID_BODIES as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("epoch"), label: "Epoch", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_215.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_region"), label: "Fill Region", ty: ParamType::Int, default: ParamValue::Float(-1.0), range: Some((-1.0, 16_777_215.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Feeds node.matter_state's seed (copied in when the domain's epoch changes) and its count. Cell boxes are in the authored domain's cells, from node.matter_domain's fill outputs (fill_min, fill_cells); the box is clipped above the pool so no cell seeds twice. Points per Cell is 8 or 27. Wire regions and fill_region from node.matter_domain beside the collider arrays; without them the box fills whole.",
    examples: ["SandColumn"],
    picker: { label: "Matter Fill", category: Atom },
    summary: "Places the liquid's starting particles in the domain: a pool on the floor plus one box.",
    category: Particles3D,
    role: Source,
    aliases: ["seed matter", "matter fill", "initial fill", "pool"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/matter_fill_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [LIQUID_POSE, LIQUID_COLLIDER, COLLIDER_BVH, MATTER_TREE, LIQUID_FILL_SHAPE],
    extra_fields: {
        buffer: Option<GpuBuffer> = None,
        filled: Option<([u32; 19], usize)> = None,
    },
}

/// Cells a pool plus clipped box fill, per the kernel's enumeration.
pub(crate) fn fill_cells(cells: [u32; 3], pool: u32, column: [[u32; 2]; 3]) -> u64 {
    let pool = pool.min(cells[1]);
    let pool_count = u64::from(cells[0]) * u64::from(pool) * u64::from(cells[2]);
    let y0 = column[1][0].max(pool);
    let extent = |lo: u32, hi: u32| u64::from(hi.saturating_sub(lo));
    let column_count = extent(column[0][0], column[0][1])
        * extent(y0, column[1][1])
        * extent(column[2][0], column[2][1]);
    pool_count + column_count
}

/// Points a fill seeds at `points_per_cell`, refused by name past the count
/// the `count` wire carries exactly.
pub(crate) fn fill_count(
    cells: [u32; 3],
    pool: u32,
    column: [[u32; 2]; 3],
    points_per_cell: u32,
) -> Result<u32, String> {
    let count = fill_cells(cells, pool, column) * u64::from(points_per_cell);
    match u32::try_from(count) {
        Ok(count) if count <= EXACT_F32_COUNT => Ok(count),
        _ => Err(format!(
            "Matter fill: {count} points exceeds the exact f32 range of the count wire ({EXACT_F32_COUNT}). Raise Cell Size, or lower Particles per Cell or the fill."
        )),
    }
}

impl Primitive for MatterFill {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "points"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "points").then_some(self.buffer.as_ref()).flatten()
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, grown to the count at run time.
        (port_name == "points").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let grid = match FillGrid::read(|name, default| ctx.scalar_or_param(name, default)) {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(format!("Matter Fill: {refusal}"));
                return;
            }
        };
        let int =
            |name: &str, default: f32| ctx.scalar_or_param(name, default).round().max(0.0) as u32;
        let pool = int("pool_cells", 3.0);
        let column = [
            [int("column_x0", 0.0), int("column_x1", 0.0)],
            [int("column_y0", 0.0), int("column_y1", 0.0)],
            [int("column_z0", 0.0), int("column_z1", 0.0)],
        ];
        let ppc = if int("points_per_cell", 8.0) >= 27 {
            27
        } else {
            8
        };
        let seed = int("seed", 0.0);
        let colliders = (
            ctx.inputs.array("bodies"),
            ctx.inputs.array("shapes"),
            ctx.inputs.array("atlas"),
            ctx.inputs.array("collider_nodes"),
            ctx.inputs.array("collider_triangles"),
        );
        let body_count = int("body_count", 0.0).min(MAX_LIQUID_BODIES as u32) as i32;
        let fill_region = ctx.scalar_or_param("fill_region", -1.0).round();
        let regions = ctx.inputs.array("regions");
        // With colliders or a shaped fill the seeds depend on their pose at
        // the epoch's start.
        let epoch = if body_count > 0 || fill_region >= 0.0 {
            int("epoch", 0.0)
        } else {
            0
        };
        let cells = grid.cells;
        let column =
            std::array::from_fn(|d| [column[d][0].min(cells[d]), column[d][1].min(cells[d])]);
        let pool = pool.min(cells[1]);
        let count = match fill_count(cells, pool, column, ppc) {
            Ok(count) => count,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        ctx.outputs
            .set_scalar("count", ParamValue::Float(count as f32));
        if count == 0 {
            return;
        }
        let bytes = u64::from(count) * std::mem::size_of::<MatterPoint>() as u64;
        if self.buffer.as_ref().is_none_or(|b| b.size < bytes) {
            let grown = bytes.max(
                self.buffer
                    .as_ref()
                    .map_or(0, |b| b.size.saturating_mul(3) / 2),
            );
            let device = ctx.gpu_encoder().device;
            let created = manifold_node_engine::load::expand::admit_candidate_bytes(
                device.modifier_memory_snapshot(),
                grown,
            )
            .map_err(|error| error.to_string())
            .and_then(|()| device.try_create_buffer(grown, BufferUse::Gpu));
            match created {
                Ok(buffer) => {
                    self.buffer = Some(buffer);
                    self.filled = None;
                }
                Err(error) => {
                    ctx.error(format!(
                        "Matter fill needs {grown} bytes of GPU storage: {error}"
                    ));
                    return;
                }
            }
        }
        let gpu = ctx.gpu_encoder();
        let buffer = self.buffer.as_ref().expect("fill storage prepared");
        // Without all five collider arrays no body or region is read; the
        // points buffer fills their slots.
        let row_count = |rows: &GpuBuffer| rows.size / std::mem::size_of::<LiquidBody>() as u64;
        let (
            bodies,
            shapes,
            atlas,
            collider_nodes,
            collider_triangles,
            body_count,
            regions,
            fill_region,
        ) = match (colliders, regions) {
            ((Some(bodies), Some(shapes), Some(atlas), Some(nodes), Some(triangles)), regions) => {
                let (regions, fill_region) = match regions {
                    Some(regions)
                        if fill_region >= 0.0 && (fill_region as u64) < row_count(regions) =>
                    {
                        (regions, fill_region as i32)
                    }
                    _ => (buffer, -1),
                };
                (
                    bodies,
                    shapes,
                    atlas,
                    nodes,
                    triangles,
                    body_count.min(row_count(bodies) as i32),
                    regions,
                    fill_region,
                )
            }
            _ => (buffer, buffer, buffer, buffer, buffer, 0, buffer, -1),
        };
        let key = [
            grid.min[0].to_bits(),
            grid.min[1].to_bits(),
            grid.min[2].to_bits(),
            grid.cell_size.to_bits(),
            cells[0],
            cells[1],
            cells[2],
            pool,
            column[0][0],
            column[0][1],
            column[1][0],
            column[1][1],
            column[2][0],
            column[2][1],
            ppc,
            seed,
            body_count as u32,
            epoch,
            fill_region as u32,
        ];
        // A pure function of its params: refill only when one changes.
        if self.filled == Some((key, buffer.identity_key())) {
            return;
        }
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = FillUniforms {
            fill_min_x: grid.min[0],
            fill_min_y: grid.min[1],
            fill_min_z: grid.min[2],
            cell_size: grid.cell_size,
            fill_cells_x: cells[0] as i32,
            fill_cells_y: cells[1] as i32,
            fill_cells_z: cells[2] as i32,
            pool_cells: pool as i32,
            column_x0: column[0][0] as i32,
            column_x1: column[0][1] as i32,
            column_y0: column[1][0] as i32,
            column_y1: column[1][1] as i32,
            column_z0: column[2][0] as i32,
            column_z1: column[2][1] as i32,
            points_per_cell: ppc as i32,
            seed: seed as i32,
            body_count,
            epoch: epoch as i32,
            fill_region,
            dispatch_count: count,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: bodies,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: shapes,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: atlas,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: collider_nodes,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: collider_triangles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: regions,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 7,
                    buffer,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.matter_fill",
        );
        self.filled = Some((key, buffer.identity_key()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_fill_counts_pool_plus_clipped_box() {
        // 64³ cells, pool 3 cells, box x 10..29, y 0..33 (clipped to 3..33),
        // z 4..60.
        let cells = fill_cells([64; 3], 3, [[10, 29], [0, 33], [4, 60]]);
        assert_eq!(cells, 64 * 3 * 64 + 19 * 30 * 56);
        // An empty box adds nothing; the pool never exceeds the domain.
        assert_eq!(fill_cells([8; 3], 20, [[0, 0], [0, 0], [0, 0]]), 8 * 8 * 8);
    }

    /// The count crosses to every point atom as an f32 wire: 2^24 points is
    /// the last fill it carries exactly, one cell more is refused by name.
    #[test]
    fn matter_fill_refuses_counts_past_the_exact_f32_range() {
        let empty = [[0, 0]; 3];
        assert_eq!(
            fill_count([256, 32, 256], 32, empty, 8),
            Ok(EXACT_F32_COUNT)
        );
        let error = fill_count([256, 33, 256], 33, empty, 8).unwrap_err();
        assert!(
            error.contains("exact f32 range") && error.contains("Cell Size"),
            "{error}"
        );
        assert!(
            fill_count([512; 3], 512, empty, 27)
                .unwrap_err()
                .contains("exact f32 range")
        );
    }

    /// Wires no domain could have produced are refused by their name instead
    /// of clamping to a one-cell box.
    #[test]
    fn matter_fill_wires_refuse_by_name() {
        let wires = |name: &str, default: f32| match name {
            "fill_cells_x" => 13.0,
            "fill_cells_y" => 5.0,
            "fill_cells_z" => 40.0,
            "cell_size" => 0.125,
            "fill_min_y" => 0.5,
            _ => default,
        };
        let grid = FillGrid::read(wires).unwrap();
        assert_eq!(grid.cells, [13, 5, 40]);
        assert_eq!(grid.cell_size, 0.125);
        assert_eq!(grid.min, [-2.0, 0.5, -2.0]);
        for (wire, value) in [
            ("fill_cells_y", 0.0),
            ("fill_cells_x", 71.5),
            ("fill_cells_z", f32::NAN),
            ("fill_cells_z", MAX_FILL_CELLS as f32 + 1.0),
            ("cell_size", 0.0),
            ("fill_min_y", f32::INFINITY),
        ] {
            let refused = FillGrid::read(|name, default| {
                if name == wire {
                    value
                } else {
                    wires(name, default)
                }
            });
            assert!(refused.unwrap_err().contains(wire), "{wire} = {value}");
        }
    }

    #[test]
    fn matter_fill_generates_a_source_kernel() {
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<MatterFill>()
            .expect("matter_fill codegen");
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
