//! `node.tree` — a tree grown from branching rules on the GPU
//! (`shaders/tree.wgsl`, after EZ-Tree by Dan Greenheck, MIT): the bark as
//! indexed vertices, and the leaves as crossed cards.
//!
//! A handful of rows shape the whole tree: Height and Trunk Width size the
//! trunk, Branches and Twigs say how many grow off each, Spread, Reach and
//! Start where and how far they go, Gnarl, Lift and Twist how they wander,
//! and Leaves, Leaf Size and Leaf Angle the foliage. Each level below the
//! trunk follows from these by fixed ratios. Levels and the counts are the
//! tree's structure: changing them sizes the outputs again. The node writes
//! only when a row moves and costs nothing while the tree holds.

use std::borrow::Cow;

use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::mesh_change::{MeshOutputRule, MeshRevisionRule};

const SHADER: &str = include_str!("shaders/tree.wgsl");
const THREADS: u32 = 64;
const LEVELS: usize = 4;
/// Rings up each level's branches, and sides round them.
const SECTIONS: [u32; LEVELS] = [12, 8, 6, 4];
const SEGMENTS: [u32; LEVELS] = [12, 7, 5, 3];
/// A branch's radius where it starts, as a share of its parent's there.
const CHILD_RADIUS: [f32; LEVELS] = [1.0, 0.55, 0.6, 0.6];
/// Where along their parent the levels below the branches start.
const LATER_START: [f32; 2] = [0.25, 0.2];
/// Each level's share of the Gnarl row, in radians a section.
const GNARL: [f32; LEVELS] = [0.15, 0.4, 0.6, 0.8];
/// The share of radius each level loses by its tip.
const TAPER: [f32; LEVELS] = [0.7, 0.75, 0.8, 0.9];
/// Lift at 1 turns wood a metre thick this many radians a section.
const LIFT: f32 = 0.04;
/// Leaf sizes spread this share either side of Leaf Size.
const LEAF_SPREAD: f32 = 0.35;
/// Where along a twig leaves start.
const LEAF_START: f32 = 0.25;
/// Two cards crossed at right angles, so a leaf reads from every side.
const CARDS: u32 = 2;

/// name, label, type, default, low, high.
type TreeParam = (&'static str, &'static str, ParamType, f32, f32, f32);

const TREE_PARAMS: [TreeParam; 16] = [
    ("levels", "Levels", ParamType::Int, 3.0, 0.0, 3.0),
    ("height", "Height", ParamType::Float, 10.0, 0.5, 60.0),
    (
        "trunk_width",
        "Trunk Width",
        ParamType::Float,
        0.5,
        0.02,
        3.0,
    ),
    ("branches", "Branches", ParamType::Int, 9.0, 0.0, 16.0),
    ("twigs", "Twigs", ParamType::Int, 6.0, 0.0, 10.0),
    ("spread", "Spread", ParamType::Float, 55.0, 0.0, 120.0),
    ("reach", "Reach", ParamType::Float, 0.55, 0.1, 1.0),
    ("start", "Start", ParamType::Float, 0.3, 0.0, 0.95),
    ("gnarl", "Gnarl", ParamType::Float, 0.25, 0.0, 1.0),
    ("lift", "Lift", ParamType::Float, 0.15, -1.0, 1.0),
    ("twist", "Twist", ParamType::Float, 0.0, -45.0, 45.0),
    ("leaves", "Leaves", ParamType::Int, 18.0, 0.0, 40.0),
    ("leaf_size", "Leaf Size", ParamType::Float, 0.4, 0.02, 2.0),
    (
        "leaf_angle",
        "Leaf Angle",
        ParamType::Float,
        35.0,
        0.0,
        90.0,
    ),
    ("seed", "Seed", ParamType::Int, 1.0, 0.0, 999.0),
    ("cards", "Cards", ParamType::Int, CARDS as f32, 1.0, 2.0),
];

const fn tree_param(k: usize) -> ParamDef {
    let (name, label, ty, default, low, high) = TREE_PARAMS[k];
    ParamDef {
        name: Cow::Borrowed(name),
        label,
        ty,
        default: ParamValue::Float(default),
        range: Some((low, high)),
        enum_values: &[],
    }
}

/// Param `name`, clamped to its range.
fn value(params: &ParamValues, name: &str) -> f32 {
    let (_, _, _, default, low, high) = TREE_PARAMS
        .iter()
        .find(|p| p.0 == name)
        .copied()
        .expect("a tree param");
    match params.get(name) {
        Some(ParamValue::Float(v)) => v.clamp(low, high),
        _ => default,
    }
}

fn count(params: &ParamValues, name: &str) -> u32 {
    value(params, name).round() as u32
}

/// How many of everything the rows make, and where each level's rings,
/// vertices and indices start.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Structure {
    last: u32,
    branches: [u32; LEVELS],
    children: [u32; LEVELS],
    ring_base: [u32; LEVELS],
    vert_base: [u32; LEVELS],
    index_base: [u32; LEVELS],
    rings: u32,
    vertices: u32,
    indices: u32,
    leaves: u32,
    cards: u32,
}

impl Structure {
    fn of(params: &ParamValues) -> Self {
        let last = count(params, "levels") as usize;
        let mut children = [0; LEVELS];
        children[0] = count(params, "branches");
        for c in &mut children[1..] {
            *c = count(params, "twigs");
        }
        for c in &mut children[last..] {
            *c = 0;
        }
        let mut s = Self {
            last: last as u32,
            branches: [0; LEVELS],
            children,
            ring_base: [0; LEVELS],
            vert_base: [0; LEVELS],
            index_base: [0; LEVELS],
            rings: 0,
            vertices: 0,
            indices: 0,
            leaves: 0,
            cards: count(params, "cards"),
        };
        for l in 0..LEVELS {
            s.branches[l] = match l {
                0 => 1,
                _ => s.branches[l - 1] * children[l - 1],
            };
            s.ring_base[l] = s.rings;
            s.vert_base[l] = s.vertices;
            s.index_base[l] = s.indices;
            s.rings += s.branches[l] * (SECTIONS[l] + 1);
            s.vertices += s.branches[l] * (SECTIONS[l] + 1) * (SEGMENTS[l] + 1);
            s.indices += s.branches[l] * SECTIONS[l] * SEGMENTS[l] * 6;
        }
        s.leaves = s.branches[last] * count(params, "leaves");
        s
    }

    /// Leaf vertices, at least one degenerate triangle.
    fn leaf_vertices(&self) -> u32 {
        (self.leaves * self.cards * 6).max(3)
    }
}

/// Field order identical to the shader's `TreeUniforms`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct TreeUniforms {
    branches: [u32; 4],
    sections: [u32; 4],
    segments: [u32; 4],
    children: [u32; 4],
    ring_base: [u32; 4],
    vert_base: [u32; 4],
    index_base: [u32; 4],
    length: [f32; 4],
    radius: [f32; 4],
    angle: [f32; 4],
    start: [f32; 4],
    gnarl: [f32; 4],
    taper: [f32; 4],
    twist: [f32; 4],
    force: [f32; 4],
    leaf: [f32; 4],
    counts: [u32; 4],
    level: [u32; 4],
}

impl TreeUniforms {
    fn new(params: &ParamValues, s: &Structure) -> Self {
        let height = value(params, "height");
        let reach = value(params, "reach");
        let spread = value(params, "spread").to_radians();
        let gnarl = value(params, "gnarl");
        let lift = value(params, "lift");
        let twist = value(params, "twist").to_radians();
        Self {
            branches: s.branches,
            sections: SECTIONS,
            segments: SEGMENTS,
            children: s.children,
            ring_base: s.ring_base,
            vert_base: s.vert_base,
            index_base: s.index_base,
            length: [0, 1, 2, 3].map(|l| height * reach.powi(l)),
            radius: [
                value(params, "trunk_width") * 0.5,
                CHILD_RADIUS[1],
                CHILD_RADIUS[2],
                CHILD_RADIUS[3],
            ],
            angle: [0.0, spread, spread, spread],
            start: [0.0, value(params, "start"), LATER_START[0], LATER_START[1]],
            gnarl: GNARL.map(|share| share * gnarl),
            taper: TAPER,
            twist: [twist; 4],
            force: [0.0, lift.signum(), 0.0, lift.abs() * LIFT],
            leaf: [
                value(params, "leaf_size"),
                LEAF_SPREAD,
                value(params, "leaf_angle").to_radians(),
                LEAF_START,
            ],
            counts: [
                s.last,
                count(params, "leaves"),
                s.cards,
                count(params, "seed"),
            ],
            level: [0, s.vertices, s.indices, s.leaf_vertices()],
        }
    }
}

struct Pipelines {
    grow: GpuComputePipeline,
    bark: GpuComputePipeline,
    index: GpuComputePipeline,
    leaves: GpuComputePipeline,
}

impl Pipelines {
    fn build(device: &GpuDevice) -> Self {
        let make = |entry: &str| device.create_compute_pipeline(SHADER, entry, "node.tree");
        Self {
            grow: make("grow_main"),
            bark: make("bark_main"),
            index: make("index_main"),
            leaves: make("leaves_main"),
        }
    }
}

struct TreeBuffers {
    structure: Structure,
    rings: GpuBuffer,
    vertices: GpuBuffer,
    indices: GpuBuffer,
    leaves: GpuBuffer,
}

impl TreeBuffers {
    fn new(device: &GpuDevice, s: Structure) -> Result<Self, String> {
        let vertex = size_of::<MeshVertex>() as u64;
        Ok(Self {
            structure: s,
            rings: device.try_create_buffer(u64::from(s.rings) * 32, BufferUse::Gpu)?,
            vertices: device.try_create_buffer(u64::from(s.vertices) * vertex, BufferUse::Gpu)?,
            indices: device.try_create_buffer(u64::from(s.indices) * 4, BufferUse::Gpu)?,
            leaves: device
                .try_create_buffer(u64::from(s.leaf_vertices()) * vertex, BufferUse::Gpu)?,
        })
    }
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

manifold_node_engine::primitive! {
    name: Tree,
    type_id: "node.tree",
    purpose: "Grow a tree from branching rules on the GPU (after EZ-Tree): a trunk and up to three levels of branches as indexed bark vertices, and two crossed leaf cards per leaf on the last level. Height, Trunk Width, Branches, Twigs, Spread, Reach, Start, Gnarl, Lift, Twist, Leaves, Leaf Size and Leaf Angle shape it; Seed picks one of its kind.",
    inputs: {},
    outputs: {
        vertices: Array(MeshVertex),
        indices: Array(u32),
        leaves: Array(MeshVertex),
    },
    params: [
        tree_param(0),
        tree_param(1),
        tree_param(2),
        tree_param(3),
        tree_param(4),
        tree_param(5),
        tree_param(6),
        tree_param(7),
        tree_param(8),
        tree_param(9),
        tree_param(10),
        tree_param(11),
        tree_param(12),
        tree_param(13),
        tree_param(14),
        tree_param(15),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire vertices and indices into one scene object (the bark) and leaves into another (an alpha-cut leaf material). Height is the trunk in metres; each level below is Reach times the one above, shorter toward the top of its parent. Spread is degrees out from the parent; Lift turns wood up (or down, below 0), thin wood the most. Levels, Branches, Twigs, Leaves and Cards size the outputs.",
    examples: [],
    picker: { label: "Tree", category: Atom },
    summary: "Grows a tree with branches and leaves from a few rows, the same tree every time for the same seed.",
    category: Geometry3D,
    role: Source,
    aliases: ["tree", "plant", "branches", "foliage", "ez-tree"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        pipelines: Option<Pipelines> = None,
        buffers: Option<TreeBuffers> = None,
        // The rows and executor epoch the outputs were last written at.
        drawn: Option<(TreeUniforms, u64)> = None,
    },
}

impl Primitive for Tree {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.pipelines
            .get_or_insert_with(|| Pipelines::build(device));
    }

    fn mesh_output_rule(&self, _port: &str) -> MeshOutputRule<'_> {
        MeshOutputRule {
            topology: MeshRevisionRule::Written,
            positions: MeshRevisionRule::Written,
        }
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "vertices" | "indices" | "leaves")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        let buffers = self.buffers.as_ref()?;
        match port {
            "vertices" => Some(&buffers.vertices),
            "indices" => Some(&buffers.indices),
            "leaves" => Some(&buffers.leaves),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        let s = Structure::of(params);
        match port {
            "vertices" => Some(s.vertices),
            "indices" => Some(s.indices),
            "leaves" => Some(s.leaf_vertices()),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let structure = Structure::of(ctx.params);
        let uniforms = TreeUniforms::new(ctx.params, &structure);
        let drawn = (uniforms, ctx.rebuild_epoch);
        if self.drawn == Some(drawn) && ctx.outputs_retained() {
            ctx.mark_outputs_unchanged();
            return;
        }
        let prepared = {
            let device = ctx.gpu_encoder().device;
            self.pipelines
                .get_or_insert_with(|| Pipelines::build(device));
            if self
                .buffers
                .as_ref()
                .is_none_or(|b| b.structure != structure)
            {
                self.buffers = None;
                TreeBuffers::new(device, structure).map(|buffers| self.buffers = Some(buffers))
            } else {
                Ok(())
            }
        };
        if let Err(error) = prepared {
            ctx.error(format!(
                "Tree: the tree needs storage the device cannot give: {error}"
            ));
            return;
        }
        let (Some(pipelines), Some(buffers)) = (self.pipelines.as_ref(), self.buffers.as_ref())
        else {
            return;
        };
        let enc = &mut *ctx.gpu_encoder().native_enc;
        let dispatch = |enc: &mut manifold_gpu::GpuEncoder,
                        pipeline: &GpuComputePipeline,
                        uniforms: &TreeUniforms,
                        items: u32| {
            let bindings = [
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(uniforms),
                },
                buffer(1, &buffers.rings),
                buffer(2, &buffers.vertices),
                buffer(3, &buffers.indices),
                buffer(4, &buffers.leaves),
            ];
            enc.dispatch_compute(
                pipeline,
                &bindings,
                [items.div_ceil(THREADS), 1, 1],
                "node.tree",
            );
        };
        // Each level grows from the rings of the one above.
        for l in 0..=structure.last {
            let mut level = uniforms;
            level.level[0] = l;
            dispatch(enc, &pipelines.grow, &level, structure.branches[l as usize]);
            enc.compute_memory_barrier_buffers();
        }
        dispatch(enc, &pipelines.bark, &uniforms, structure.vertices);
        dispatch(enc, &pipelines.index, &uniforms, structure.indices / 6);
        dispatch(enc, &pipelines.leaves, &uniforms, structure.leaves.max(3));
        self.drawn = Some(drawn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_lie_end_to_end_with_each_count_from_the_rows() {
        let mut params = ParamValues::default();
        for (name, v) in [
            ("levels", 2.0),
            ("branches", 3.0),
            ("twigs", 2.0),
            ("leaves", 4.0),
        ] {
            params.insert(Cow::Borrowed(name), ParamValue::Float(v));
        }
        let s = Structure::of(&params);
        assert_eq!(s.branches, [1, 3, 6, 0]);
        assert_eq!(s.children, [3, 2, 0, 0]);
        // Rings: 13, 3 × 9, 6 × 7.
        assert_eq!(s.ring_base, [0, 13, 40, 82]);
        assert_eq!(s.rings, 82);
        // Vertices: 13 × 13, 3 × 9 × 8, 6 × 7 × 6.
        assert_eq!(s.vert_base, [0, 169, 385, 637]);
        // Indices: 12 × 12 × 6, 3 × 8 × 7 × 6, 6 × 6 × 5 × 6.
        assert_eq!(s.index_base, [0, 864, 1872, 2952]);
        assert_eq!(s.indices, 2952);
        // Four leaves on each of the six twigs, two cards of two triangles.
        assert_eq!(s.leaves, 24);
        assert_eq!(s.leaf_vertices(), 288);
    }

    #[test]
    fn a_bare_trunk_carries_its_leaves() {
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("levels"), ParamValue::Float(0.0));
        params.insert(Cow::Borrowed("leaves"), ParamValue::Float(0.0));
        let s = Structure::of(&params);
        assert_eq!(s.branches, [1, 0, 0, 0]);
        assert_eq!(s.vertices, 13 * 13);
        assert_eq!(
            s.leaf_vertices(),
            3,
            "one degenerate triangle with no leaves"
        );
    }
}
