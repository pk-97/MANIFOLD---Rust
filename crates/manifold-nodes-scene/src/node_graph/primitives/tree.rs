//! `node.tree` — a tree grown from branching rules on the GPU
//! (`shaders/tree.wgsl`, after EZ-Tree by Dan Greenheck, MIT): the bark as
//! indexed vertices, and the leaves as crossed cards.
//!
//! Both publish four detail levels in `mesh_lods.rs`'s layout, so far
//! copies draw less: past the first the last level of wood goes and each
//! leaf keeps one card; past the second a coarse trunk stays, with two
//! crossed cards standing in for each last-level branch's leaves; past the
//! third three cards round the trunk stand in for the whole crown; past the
//! fourth the tree draws nothing. Each level's error is a share of Height.
//! Copies need a box round what they draw: the tree publishes one from its
//! rows.
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
use manifold_node_engine::scene::mesh_bounds::MeshBounds;
use manifold_node_engine::scene::mesh_change::{MeshOutputRule, MeshRevisionRule};

use super::mesh_lods::LOD_HEADER_WORDS;

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
/// Each detail level's error as a share of Height. At a 60° lens on 1080p
/// a 20 m tree moves to them about 150, 375, 1100 and 3700 m away.
const LOD_ERROR: [f32; 4] = [0.008, 0.02, 0.06, 0.2];
/// Crossed cards over each last-level branch's leaves at the second detail
/// level, and cards round the trunk through the crown at the third.
const BRANCH_CARDS: u32 = 2;
const CROWN_CARDS: u32 = 3;
/// Indices of the coarse trunk the second and third levels draw:
/// `COARSE_INDICES` in `tree.wgsl`.
const COARSE_INDICES: u32 = 54;

/// name, label, type, default, low, high.
type TreeParam = (&'static str, &'static str, ParamType, f32, f32, f32);

const TREE_PARAMS: [TreeParam; 17] = [
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
    ("branches", "Branches", ParamType::Int, 9.0, 0.0, 48.0),
    ("twigs", "Twigs", ParamType::Int, 6.0, 0.0, 10.0),
    ("spread", "Spread", ParamType::Float, 55.0, 0.0, 120.0),
    ("reach", "Reach", ParamType::Float, 0.55, 0.1, 1.0),
    ("start", "Start", ParamType::Float, 0.3, 0.0, 0.95),
    ("cone", "Cone", ParamType::Float, 0.5, 0.0, 1.0),
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
    /// Cards standing in for the leaves far away.
    branch_cards: u32,
    crown_cards: u32,
    /// Index count of the bark's detail level 1.
    bark_lod: u32,
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
            branch_cards: 0,
            crown_cards: 0,
            bark_lod: 0,
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
        if s.leaves > 0 {
            s.branch_cards = s.branches[last] * BRANCH_CARDS;
            s.crown_cards = CROWN_CARDS;
        }
        // Wood comes level by level, the trunk first.
        s.bark_lod = if last > 0 {
            s.index_base[last]
        } else {
            s.indices
        };
        s
    }

    fn cards(&self) -> u32 {
        self.leaves * self.cards
    }

    /// Leaf vertices: four a card, then the branch and crown cards; at
    /// least one degenerate triangle.
    fn leaf_vertices(&self) -> u32 {
        ((self.cards() + self.branch_cards + self.crown_cards) * 4).max(3)
    }

    /// Leaf indices of the full tree.
    fn leaf_indices(&self) -> u32 {
        (self.cards() * 6).max(3)
    }

    /// Index counts of the leaves' detail levels 1 to 3.
    fn leaf_lods(&self) -> [u32; 3] {
        [self.leaves * 6, self.branch_cards * 6, self.crown_cards * 6]
    }

    fn bark_lods_words(&self) -> u32 {
        LOD_HEADER_WORDS + self.bark_lod + COARSE_INDICES
    }

    fn leaf_lods_words(&self) -> u32 {
        LOD_HEADER_WORDS + self.leaf_lods().iter().sum::<u32>()
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
    shape: [f32; 4],
    lod_error: [f32; 4],
    lod_count: [u32; 4],
    cards: [u32; 4],
    crown: [f32; 4],
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
            shape: [value(params, "cone"), 0.0, 0.0, 0.0],
            lod_error: LOD_ERROR.map(|share| share * height),
            lod_count: [
                s.bark_lod,
                s.leaf_lods()[0],
                s.leaf_lods()[1],
                s.leaf_lods()[2],
            ],
            cards: [s.cards(), s.branch_cards, s.crown_cards, s.cards() * 4],
            crown: crown(params, s.last),
        }
    }
}

/// The crown as `tree.wgsl` shades it: its radius round the trunk, and the
/// heights of its bottom and top. Branches reach out by Spread, the longest
/// at the bottom, and twigs and leaves reach past them.
fn crown(params: &ParamValues, last: u32) -> [f32; 4] {
    let height = value(params, "height");
    let reach = value(params, "reach");
    let start = value(params, "start");
    let leaf = value(params, "leaf_size");
    if last == 0 {
        return [leaf, LEAF_START * height, height, 0.0];
    }
    let branch = height * reach * (1.0 - value(params, "cone") * start);
    let out = value(params, "spread").to_radians().sin().abs().max(0.3);
    let twig = if last >= 2 {
        0.5 * height * reach * reach
    } else {
        0.0
    };
    [branch * out + twig + leaf, start * height, height, 0.0]
}

/// A box sure to hold the whole tree, from the rows alone, which copies
/// need to be culled and to draw at their detail levels: the trunk leans
/// at most as far as its Gnarl can bend it in all its sections, and wood
/// and leaves reach at most every level's length and the largest leaf
/// past it, any way.
fn bounds(params: &ParamValues, s: &Structure) -> Option<MeshBounds> {
    let height = value(params, "height");
    let reach = value(params, "reach");
    let bend = GNARL[0] * value(params, "gnarl") * SECTIONS[0] as f32;
    let lean = height * bend.min(std::f32::consts::FRAC_PI_2).sin();
    let leaf = value(params, "leaf_size") * (1.0 + LEAF_SPREAD);
    let out = (1..=s.last as i32)
        .map(|l| height * reach.powi(l))
        .sum::<f32>()
        + leaf;
    let side = lean + out;
    MeshBounds::of_positions([[-side, -out, -side], [side, height + out, side]])
}

struct Pipelines {
    grow: GpuComputePipeline,
    bark: GpuComputePipeline,
    index: GpuComputePipeline,
    leaves: GpuComputePipeline,
    leaf_index: GpuComputePipeline,
    cards: GpuComputePipeline,
    lods: GpuComputePipeline,
}

impl Pipelines {
    fn build(device: &GpuDevice) -> Self {
        let make = |entry: &str| device.create_compute_pipeline(SHADER, entry, "node.tree");
        Self {
            grow: make("grow_main"),
            bark: make("bark_main"),
            index: make("index_main"),
            leaves: make("leaves_main"),
            leaf_index: make("leaf_index_main"),
            cards: make("cards_main"),
            lods: make("lods_main"),
        }
    }
}

struct TreeBuffers {
    structure: Structure,
    rings: GpuBuffer,
    vertices: GpuBuffer,
    indices: GpuBuffer,
    leaves: GpuBuffer,
    leaf_indices: GpuBuffer,
    bark_lods: GpuBuffer,
    leaf_lods: GpuBuffer,
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
            leaf_indices: device
                .try_create_buffer(u64::from(s.leaf_indices()) * 4, BufferUse::Gpu)?,
            bark_lods: device
                .try_create_buffer(u64::from(s.bark_lods_words()) * 4, BufferUse::Gpu)?,
            leaf_lods: device
                .try_create_buffer(u64::from(s.leaf_lods_words()) * 4, BufferUse::Gpu)?,
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
    purpose: "Grow a tree from branching rules on the GPU (after EZ-Tree): a trunk and up to three levels of branches as indexed bark vertices, and two crossed leaf cards per leaf on the last level, each with four detail levels for far copies. Height, Trunk Width, Branches, Twigs, Spread, Reach, Start, Cone, Gnarl, Lift, Twist, Leaves, Leaf Size and Leaf Angle shape it; Seed picks one of its kind.",
    inputs: {},
    outputs: {
        vertices: Array(MeshVertex),
        indices: Array(u32),
        leaves: Array(MeshVertex),
        leaf_indices: Array(u32),
        bark_lods: Array(u32),
        leaf_lods: Array(u32),
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
        tree_param(16),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire vertices, indices and bark_lods into one scene object (the bark) and leaves, leaf_indices and leaf_lods into another (an alpha-cut leaf material). Height is the trunk in metres; each level below is Reach times the one above, shorter toward the top of its parent, and the branches off the trunk by Cone (1 makes a spire). Spread is degrees out from the parent; Lift turns wood up (or down, below 0), thin wood the most. Levels, Branches, Twigs, Leaves and Cards size the outputs.",
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
        matches!(
            port,
            "vertices" | "indices" | "leaves" | "leaf_indices" | "bark_lods" | "leaf_lods"
        )
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        let buffers = self.buffers.as_ref()?;
        match port {
            "vertices" => Some(&buffers.vertices),
            "indices" => Some(&buffers.indices),
            "leaves" => Some(&buffers.leaves),
            "leaf_indices" => Some(&buffers.leaf_indices),
            "bark_lods" => Some(&buffers.bark_lods),
            "leaf_lods" => Some(&buffers.leaf_lods),
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
            "leaf_indices" => Some(s.leaf_indices()),
            "bark_lods" => Some(s.bark_lods_words()),
            "leaf_lods" => Some(s.leaf_lods_words()),
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
                buffer(5, &buffers.leaf_indices),
                buffer(6, &buffers.bark_lods),
                buffer(7, &buffers.leaf_lods),
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
        dispatch(enc, &pipelines.index, &uniforms, structure.indices);
        dispatch(enc, &pipelines.leaves, &uniforms, structure.leaves.max(3));
        dispatch(
            enc,
            &pipelines.leaf_index,
            &uniforms,
            structure.leaf_indices(),
        );
        dispatch(
            enc,
            &pipelines.cards,
            &uniforms,
            structure.branch_cards / BRANCH_CARDS + structure.crown_cards,
        );
        dispatch(
            enc,
            &pipelines.lods,
            &uniforms,
            structure.bark_lods_words().max(structure.leaf_lods_words()),
        );
        let bounds = bounds(ctx.params, &structure);
        ctx.outputs.set_cpu_value("vertices", bounds);
        ctx.outputs.set_cpu_value("leaves", bounds);
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
        // Four leaves on each of the six twigs, two cards of four corners,
        // then two branch cards on each twig and three round the crown.
        assert_eq!(s.leaves, 24);
        assert_eq!(s.leaf_vertices(), (24 * 2 + 6 * 2 + 3) * 4);
        assert_eq!(s.leaf_indices(), 24 * 2 * 6);
        // Far away: the wood without twigs, then the coarse trunk; one
        // card a leaf, then the branch cards, then the crown's.
        assert_eq!(s.bark_lod, 1872);
        assert_eq!(s.leaf_lods(), [24 * 6, 12 * 6, 3 * 6]);
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
