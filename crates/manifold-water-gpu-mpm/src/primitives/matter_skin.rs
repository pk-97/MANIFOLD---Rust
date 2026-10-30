//! `node.matter_skin` — a Matter item drawn as its fill's own mesh, carried
//! by the item's points (MATTER_DRAW_DESIGN.md, shared piece 3). Jelly and
//! clay keep their shape, so a real mesh moved by the material draws them
//! exactly: crisp edges, the mesh's UVs and normals, no surface build.
//! Hand WGSL under exclusion 2: the bonds and the A/B ring of skinned
//! vertices are cross-frame state.
//!
//! A point keeps its index from its fill to a Reset, so each vertex is bound
//! once an epoch: to its nearest seeds in the item's fill box, by their
//! offsets at rest. After each tick the vertices move with the points, and
//! every frame draws the blend of the last two ticks on the matter frame's
//! display clock.

use std::borrow::Cow;
use std::sync::mpsc;

use ahash::AHashMap;
use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use crate::matter::MatterPoint;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::mesh_change::{MeshOutputRule, MeshRevisionRule};
use manifold_node_engine::scene::mesh_source::{MeshSource, subdivide};
use manifold_node_engine::scene::transform::Transform;
use manifold_water_liquid::frame_ring::FrameRing;

const SHADER: &str = include_str!("shaders/matter_skin.wgsl");

/// Bonds a vertex keeps (`BONDS` in the shader).
const BONDS: u64 = 8;
/// Threads per workgroup in every kernel.
const GROUP: u32 = 64;
/// Bytes of one skinned vertex of a tick (`Skinned`: three vec4).
const SKINNED_BYTES: u64 = 48;
/// The most triangles a coarse mesh is split into to follow the points.
const MAX_TRIANGLES: usize = 400_000;
/// Segments round a Sphere fill's carried ball (rings half that): its
/// outline stays round in a close-up, where the fill's own 32 show facets.
const SPHERE_SEGMENTS: u32 = 128;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SkinParams {
    vertices: u32,
    slot: u32,
    points_per_cell: u32,
    point_count: u32,
    fill_min: [f32; 3],
    cell_size: f32,
    box_lo: [f32; 3],
    box_fill: u32,
    box_hi: [f32; 3],
    blend: f32,
    has_previous: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

pub struct SkinPipelines {
    bind: GpuComputePipeline,
    skin: GpuComputePipeline,
    normal: GpuComputePipeline,
    blend: GpuComputePipeline,
}

manifold_node_engine::primitive! {
    name: MatterSkin,
    type_id: "node.matter_skin",
    purpose: "Draw one Matter item as its fill's own mesh carried by its points: each vertex bonds once an epoch to its 8 nearest seeds in the item's fill box (row `slot` of the fill table, whose cells hold their points in cell order) by its offset from each at rest; after every tick a vertex is the kernel-weighted mean over its live bonds of point + F · offset, and its normal and tangent turn as its smoothing group's faces turn. The vertices drawn are the blend of the last two ticks on the display clock. A Box fill (no mesh) carries the box its row seeds, a Sphere fill a 128-segment ball; a coarse mesh is split until its edges are about one point spacing. No transform draws nothing.",
    inputs: {
        // node.matter_state's out: the live points.
        points: Array(MatterPoint) required,
        // node.matter_fill's points: where each point was seeded.
        seeds: Array(MatterPoint) required,
        stats: Array(u32) required,
        // node.matter_domain's fill table.
        fills: Array(u32) required,
        // The fill's shape: none for a Box.
        mesh: MeshSource optional,
        // The fill's transform, read when the vertices bond.
        transform: Transform optional,
        fill_min_x: ScalarF32 optional, fill_min_y: ScalarF32 optional, fill_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        points_per_cell: ScalarF32 optional,
        simulation_time: ScalarF32 optional,
        display_time: ScalarF32 optional,
        epoch: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex),
        indices: Array(u32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("slot"), label: "Slot", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, (crate::matter::MAX_MATTER_SLOTS - 1) as f32)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire points and stats from node.matter_state, seeds from node.matter_fill's points, fills, fill_min_*, cell_size, points_per_cell, simulation_time, display_time and epoch from node.matter_domain (the frame's clock), and the item's Fill source's transform and mesh. vertices and indices go to a scene object, which keeps last frame's positions itself for motion blur. Slot is the item's row of the fill table. Bonds rebuild on a new epoch or mesh; a Gltf mesh loads off the content thread and the object draws nothing until it lands.",
    examples: [],
    picker: { label: "Matter Skin", category: Atom },
    summary: "Carries a jelly's or clay's own mesh with the simulated material, so it keeps crisp edges and its UVs.",
    category: Particles3D,
    role: Map,
    aliases: ["matter skin", "carried mesh", "embedded mesh", "point deform"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        pipelines: Option<SkinPipelines> = None,
        // The carried mesh in its own space, for the source it came from.
        local: Option<(MeshSource, Vec<MeshVertex>)> = None,
        // A mesh loading off the content thread.
        loading: Option<(MeshSource, mpsc::Receiver<Result<Vec<MeshVertex>, String>>)> = None,
        bound: Option<Bound> = None,
        ring: FrameRing = FrameRing::default(),
        drawn: Option<GpuBuffer> = None,
        // Ticks written, and what the drawn vertices last showed.
        ticks: u64 = 0,
        shown: Option<(u64, u32)> = None,
    },
}

/// What the bonds were built for: a new epoch, mesh, seed or table buffer,
/// slot or grid rebuilds them.
#[derive(Clone, Debug, PartialEq)]
struct BindKey {
    epoch: u32,
    source: MeshSource,
    seeds: usize,
    fills: usize,
    slot: u32,
    grid: [u32; 5],
}

/// One bound mesh on the GPU.
pub struct Bound {
    key: BindKey,
    vertices: u32,
    rest: GpuBuffer,
    indices: GpuBuffer,
    vertex_group: GpuBuffer,
    group_normal: GpuBuffer,
    group_start: GpuBuffer,
    group_triangles: GpuBuffer,
    bonds: GpuBuffer,
    bond_points: GpuBuffer,
}

/// A carried mesh on the CPU, in world space: welded vertices and
/// triangles, and each vertex's smoothing group (its position and normal),
/// with the group's rest face-normal sum and its triangles.
struct CarriedMesh {
    vertices: Vec<MeshVertex>,
    indices: Vec<u32>,
    vertex_group: Vec<u32>,
    group_normal: Vec<[f32; 4]>,
    group_start: Vec<u32>,
    group_triangles: Vec<u32>,
}

/// The mesh the skin carries for a fill of `source`: the unit cube a Box
/// fills, a Sphere at [`SPHERE_SEGMENTS`], any other shape as it is.
fn carried_source(source: Option<&MeshSource>) -> MeshSource {
    match source {
        None => MeshSource::Cube { size: 1.0 },
        Some(MeshSource::Sphere { radius, .. }) => MeshSource::Sphere {
            radius: *radius,
            segments: SPHERE_SEGMENTS,
            rings: SPHERE_SEGMENTS / 2,
        },
        Some(other) => other.clone(),
    }
}

/// Shapes built from their parameters alone, cheap enough to build in place.
fn builds_in_place(source: &MeshSource) -> bool {
    matches!(
        source,
        MeshSource::Cube { .. }
            | MeshSource::Sphere { .. }
            | MeshSource::Platonic { .. }
            | MeshSource::Plane { .. }
    )
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(a: [f32; 3]) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

fn normalized(a: [f32; 3]) -> [f32; 3] {
    let l = length(a);
    if l > 0.0 { a.map(|v| v / l) } else { a }
}

/// Bits that weld: -0 is 0.
fn weld_bits(v: f32) -> u32 {
    if v == 0.0 { 0 } else { v.to_bits() }
}

/// `local` (a triangle list in the mesh's own space) placed by `transform`,
/// split until its edges are about `spacing` (at most [`MAX_TRIANGLES`]),
/// welded and grouped for the normal pass.
fn carried_mesh(
    mut local: Vec<MeshVertex>,
    transform: &Transform,
    spacing: f32,
) -> Result<CarriedMesh, String> {
    if local.is_empty() || !local.len().is_multiple_of(3) {
        return Err("the fill's mesh is not a triangle list".into());
    }
    let m = transform.world();
    let linear = |v: [f32; 3]| -> [f32; 3] {
        std::array::from_fn(|row| m[0][row] * v[0] + m[1][row] * v[1] + m[2][row] * v[2])
    };
    let columns = [0, 1, 2].map(|c| [m[c][0], m[c][1], m[c][2]]);
    let det = columns[0][0] * (columns[1][1] * columns[2][2] - columns[1][2] * columns[2][1])
        - columns[1][0] * (columns[0][1] * columns[2][2] - columns[0][2] * columns[2][1])
        + columns[2][0] * (columns[0][1] * columns[1][2] - columns[0][2] * columns[1][1]);
    if !det.is_finite() || det.abs() < 1e-12 {
        return Err("the fill's transform squashes it flat".into());
    }
    // Normals turn by the inverse transpose: the cofactor matrix over det.
    let cofactor = [
        cross(columns[1], columns[2]),
        cross(columns[2], columns[0]),
        cross(columns[0], columns[1]),
    ];
    let normal = |n: [f32; 3]| -> [f32; 3] {
        normalized(cofactor.map(|row| (row[0] * n[0] + row[1] * n[1] + row[2] * n[2]) / det))
    };
    for vertex in &mut local {
        let p = linear(vertex.position);
        vertex.position = [p[0] + m[3][0], p[1] + m[3][1], p[2] + m[3][2]];
        vertex.normal = normal(vertex.normal);
        let t = normalized(linear([
            vertex.tangent[0],
            vertex.tangent[1],
            vertex.tangent[2],
        ]));
        vertex.tangent = [t[0], t[1], t[2], vertex.tangent[3]];
    }
    // A mirroring transform turns the triangles inside out: rewind them.
    if det < 0.0 {
        for tri in local.chunks_exact_mut(3) {
            tri.swap(1, 2);
        }
    }
    let longest = local
        .chunks_exact(3)
        .flat_map(|t| {
            [(0, 1), (1, 2), (2, 0)].map(|(a, b)| length(sub(t[a].position, t[b].position)))
        })
        .fold(0.0f32, f32::max);
    let mut levels = 0;
    let mut triangles = local.len() / 3;
    while longest / (1u32 << levels) as f32 > spacing && triangles * 4 <= MAX_TRIANGLES {
        levels += 1;
        triangles *= 4;
    }
    let triangle_list = subdivide(local, levels);

    let mut welded: AHashMap<[u32; 20], u32> = AHashMap::new();
    let mut groups: AHashMap<[u32; 6], u32> = AHashMap::new();
    let mut vertices = Vec::new();
    let mut vertex_group = Vec::new();
    let mut indices = Vec::with_capacity(triangle_list.len());
    for vertex in &triangle_list {
        let record: [f32; 20] = bytemuck::cast(*vertex);
        let key = record.map(weld_bits);
        let next = vertices.len() as u32;
        let index = *welded.entry(key).or_insert_with(|| {
            vertices.push(*vertex);
            // Rounded: a seam's two copies differ in the last bits (a
            // sphere's sin 2π is not 0) and must turn as one.
            let p = vertex.position.map(|v| (v * 1.0e5).round() as i32 as u32);
            let n = vertex.normal.map(|v| (v * 1.0e3).round() as i32 as u32);
            let group_key = [p[0], p[1], p[2], n[0], n[1], n[2]];
            let next_group = groups.len() as u32;
            vertex_group.push(*groups.entry(group_key).or_insert(next_group));
            next
        });
        indices.push(index);
    }
    let group_count = groups.len();
    let mut sums = vec![[0.0f32; 3]; group_count];
    let mut lists: Vec<Vec<u32>> = vec![Vec::new(); group_count];
    for (t, tri) in indices.chunks_exact(3).enumerate() {
        let [a, b, c] = [0, 1, 2].map(|k| vertices[tri[k] as usize].position);
        let face = cross(sub(b, a), sub(c, a));
        for k in 0..3 {
            let g = vertex_group[tri[k] as usize] as usize;
            if lists[g].last() != Some(&(t as u32)) {
                lists[g].push(t as u32);
                for (s, f) in sums[g].iter_mut().zip(face) {
                    *s += f;
                }
            }
        }
    }
    let group_normal = sums
        .iter()
        .map(|&sum| {
            let n = normalized(sum);
            [n[0], n[1], n[2], 0.0]
        })
        .collect();
    let mut group_start = Vec::with_capacity(group_count + 1);
    let mut group_triangles = Vec::new();
    for list in &lists {
        group_start.push(group_triangles.len() as u32);
        group_triangles.extend_from_slice(list);
    }
    group_start.push(group_triangles.len() as u32);
    Ok(CarriedMesh {
        vertices,
        indices,
        vertex_group,
        group_normal,
        group_start,
        group_triangles,
    })
}

/// A buffer holding `data`, written once by the CPU.
fn upload<T: bytemuck::Pod>(device: &GpuDevice, data: &[T]) -> Result<GpuBuffer, String> {
    let bytes: &[u8] = bytemuck::cast_slice(data);
    let buffer = device.try_create_buffer(bytes.len().max(16) as u64, BufferUse::Upload)?;
    // SAFETY: the buffer was just created with room for `bytes`.
    unsafe { buffer.write(0, bytes) };
    Ok(buffer)
}

impl Bound {
    fn new(device: &GpuDevice, key: BindKey, mesh: &CarriedMesh) -> Result<Self, String> {
        let vertices = mesh.vertices.len() as u32;
        let bond_records = u64::from(vertices) * BONDS;
        manifold_node_engine::load::expand::admit_world_bytes(
            device.modifier_memory_snapshot(),
            bond_records * 20,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            key,
            vertices,
            rest: upload(device, &mesh.vertices)?,
            indices: upload(device, &mesh.indices)?,
            vertex_group: upload(device, &mesh.vertex_group)?,
            group_normal: upload(device, &mesh.group_normal)?,
            group_start: upload(device, &mesh.group_start)?,
            group_triangles: upload(device, &mesh.group_triangles)?,
            bonds: device.try_create_buffer(bond_records * 16, BufferUse::Gpu)?,
            bond_points: device.try_create_buffer(bond_records * 4, BufferUse::Gpu)?,
        })
    }
}

impl MatterSkin {
    /// The carried mesh for `source` in its own space, once it has loaded:
    /// built in place for a parametric shape, else loaded off the content
    /// thread, `None` while it loads.
    fn local_mesh(&mut self, source: MeshSource) -> Option<Result<&[MeshVertex], String>> {
        if self.local.as_ref().is_some_and(|(have, _)| *have == source) {
            return self.local.as_ref().map(|(_, v)| Ok(v.as_slice()));
        }
        let loaded = if builds_in_place(&source) {
            source.load_vertices()
        } else {
            if self
                .loading
                .as_ref()
                .is_none_or(|(want, _)| *want != source)
            {
                let (send, receive) = mpsc::channel();
                let job = source.clone();
                std::thread::spawn(move || {
                    let _ = send.send(job.load_vertices());
                });
                self.loading = Some((source.clone(), receive));
            }
            let (_, receive) = self.loading.as_ref().expect("a load in flight");
            match receive.try_recv() {
                Ok(loaded) => {
                    self.loading = None;
                    loaded
                }
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.loading = None;
                    Err("the mesh load stopped".into())
                }
            }
        };
        match loaded {
            Ok(vertices) => {
                self.local = Some((source, vertices));
                self.local.as_ref().map(|(_, v)| Ok(v.as_slice()))
            }
            Err(error) => Some(Err(error)),
        }
    }
}

impl Primitive for MatterSkin {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        if self.pipelines.is_none() {
            let make =
                |entry: &str, label: &str| device.create_compute_pipeline(SHADER, entry, label);
            self.pipelines = Some(SkinPipelines {
                bind: make("bind_main", "node.matter_skin.bind"),
                skin: make("skin_main", "node.matter_skin.skin"),
                normal: make("normal_main", "node.matter_skin.normal"),
                blend: make("blend_main", "node.matter_skin.blend"),
            });
        }
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "vertices" | "indices")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "vertices" => self.drawn.as_ref(),
            "indices" => self.bound.as_ref().map(|bound| &bound.indices),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, sized to the mesh at run time.
        self.provides_array_output(port_name).then_some(1)
    }

    /// The triangles change only when the mesh is rebound, into new buffers;
    /// every tick moves the positions.
    fn mesh_output_rule(&self, port: &str) -> MeshOutputRule<'_> {
        MeshOutputRule {
            topology: MeshRevisionRule::Fixed,
            positions: if port == "vertices" {
                MeshRevisionRule::Written
            } else {
                MeshRevisionRule::Fixed
            },
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(points), Some(seeds), Some(stats), Some(fills), Some(transform)) = (
            ctx.inputs.array("points"),
            ctx.inputs.array("seeds"),
            ctx.inputs.array("stats"),
            ctx.inputs.array("fills"),
            ctx.inputs.transform("transform"),
        ) else {
            return;
        };
        let epoch = ctx.scalar_or_param("epoch", 0.0).round().max(0.0) as u32;
        let simulation_time = f64::from(ctx.scalar_or_param("simulation_time", 0.0));
        let display_time = f64::from(ctx.scalar_or_param("display_time", 0.0));
        let fill_min =
            ["fill_min_x", "fill_min_y", "fill_min_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let points_per_cell = if ctx.scalar_or_param("points_per_cell", 8.0) >= 27.0 {
            27
        } else {
            8
        };
        let slot = ctx
            .scalar_or_param("slot", 0.0)
            .round()
            .clamp(0.0, (crate::matter::MAX_MATTER_SLOTS - 1) as f32) as u32;
        let point_bytes = std::mem::size_of::<MatterPoint>() as u64;
        let point_count = (points.size.min(seeds.size) / point_bytes) as u32;
        let shape = ctx.inputs.mesh_source("mesh");
        // A Box fill seeds an axis-aligned box: carry the box, not the
        // transform's turn; the bind moves its sides onto the seeded cells.
        let box_fill = shape.is_none();
        let transform = if box_fill {
            transform.world_box()
        } else {
            transform
        };
        let half = transform.scale.map(|s| 0.5 * s.abs());
        let source = carried_source(shape.as_ref());
        let key = BindKey {
            epoch,
            source: source.clone(),
            seeds: seeds.identity_key(),
            fills: fills.identity_key(),
            slot,
            grid: [
                fill_min[0].to_bits(),
                fill_min[1].to_bits(),
                fill_min[2].to_bits(),
                cell_size.to_bits(),
                points_per_cell,
            ],
        };
        let mut params = SkinParams {
            vertices: 0,
            slot,
            points_per_cell,
            point_count,
            fill_min,
            cell_size,
            box_lo: std::array::from_fn(|d| transform.pos[d] - half[d]),
            box_fill: u32::from(box_fill),
            box_hi: std::array::from_fn(|d| transform.pos[d] + half[d]),
            blend: 1.0,
            has_previous: 0,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        let rebind = self.bound.as_ref().is_none_or(|bound| bound.key != key);
        if rebind {
            let spacing = cell_size / (points_per_cell as f32).cbrt();
            let mesh = match self.local_mesh(source) {
                None => {
                    ctx.mark_outputs_pending();
                    return;
                }
                Some(Err(error)) => {
                    ctx.error(format!("Matter Skin: the fill's mesh: {error}"));
                    return;
                }
                Some(Ok(local)) => carried_mesh(local.to_vec(), &transform, spacing),
            };
            let device = ctx.gpu_encoder().device;
            let bound = mesh.and_then(|mesh| Bound::new(device, key, &mesh));
            let drawn = bound.as_ref().map_err(String::clone).and_then(|bound| {
                device.try_create_buffer(
                    u64::from(bound.vertices) * std::mem::size_of::<MeshVertex>() as u64,
                    BufferUse::Gpu,
                )
            });
            match (bound, drawn) {
                (Ok(bound), Ok(drawn)) => {
                    self.bound = Some(bound);
                    self.drawn = Some(drawn);
                    // The old ring holds another mesh's vertices.
                    self.ring = FrameRing::default();
                    self.shown = None;
                }
                (Err(error), _) | (_, Err(error)) => {
                    self.bound = None;
                    self.drawn = None;
                    ctx.error(format!("Matter Skin: {error}"));
                    return;
                }
            }
        }
        let (Some(bound), Some(drawn)) = (self.bound.as_ref(), self.drawn.as_ref()) else {
            return;
        };
        params.vertices = bound.vertices;
        let groups = [bound.vertices.div_ceil(GROUP), 1, 1];
        let pipelines = self
            .pipelines
            .as_ref()
            .expect("skin kernels built by prepare_pipelines");
        let gpu = ctx.gpu_encoder();
        let mut refused = None;
        if rebind {
            gpu.native_enc.dispatch_compute(
                &pipelines.bind,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&params),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: &bound.rest,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: seeds,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: fills,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 4,
                        buffer: &bound.bonds,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 5,
                        buffer: &bound.bond_points,
                        offset: 0,
                    },
                ],
                groups,
                "node.matter_skin.bind",
            );
            gpu.native_enc.compute_memory_barrier_buffers();
        }
        if rebind || self.ring.wants_tick(epoch, simulation_time) {
            let bytes = u64::from(bound.vertices) * SKINNED_BYTES;
            match self.ring.begin(gpu.device, bytes, epoch) {
                Ok(write) => {
                    params.has_previous = u32::from(
                        !write.grown && !write.restarted && write.previous_count == bound.vertices,
                    );
                    let bindings = [
                        GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(&params),
                        },
                        GpuBinding::Buffer {
                            binding: 1,
                            buffer: &bound.rest,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 4,
                            buffer: &bound.bonds,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 5,
                            buffer: &bound.bond_points,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 6,
                            buffer: points,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 7,
                            buffer: stats,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 8,
                            buffer: self.ring.slot(write.previous),
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 9,
                            buffer: self.ring.slot(write.write),
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 10,
                            buffer: &bound.vertex_group,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 11,
                            buffer: &bound.group_normal,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 12,
                            buffer: &bound.group_start,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 13,
                            buffer: &bound.group_triangles,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 14,
                            buffer: &bound.indices,
                            offset: 0,
                        },
                    ];
                    gpu.native_enc.dispatch_compute(
                        &pipelines.skin,
                        &bindings,
                        groups,
                        "node.matter_skin.skin",
                    );
                    gpu.native_enc.compute_memory_barrier_buffers();
                    gpu.native_enc.dispatch_compute(
                        &pipelines.normal,
                        &bindings,
                        groups,
                        "node.matter_skin.normal",
                    );
                    gpu.native_enc.compute_memory_barrier_buffers();
                    self.ring
                        .finish(write, bound.vertices, epoch, simulation_time);
                    self.ticks += 1;
                }
                Err(error) => {
                    refused = Some(format!(
                        "Matter Skin: the skinned frames need 3 × {bytes} bytes the device cannot give: {error}"
                    ));
                }
            }
        }
        let (blend, _) = self.ring.blend(display_time);
        let shown = (self.ticks, blend.to_bits());
        if let (Some(a), Some(b)) = (self.ring.buffer_a(), self.ring.buffer_b()) {
            if self.shown == Some(shown) && ctx.outputs_retained() {
                ctx.mark_outputs_unchanged();
            } else {
                params.blend = blend;
                let gpu = ctx.gpu_encoder();
                gpu.native_enc.dispatch_compute(
                    &pipelines.blend,
                    &[
                        GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(&params),
                        },
                        GpuBinding::Buffer {
                            binding: 1,
                            buffer: &bound.rest,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 15,
                            buffer: a,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 16,
                            buffer: b,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 17,
                            buffer: drawn,
                            offset: 0,
                        },
                    ],
                    groups,
                    "node.matter_skin.blend",
                );
                self.shown = Some(shown);
            }
        }
        if let Some(error) = refused {
            ctx.error(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_skin_kernels_validate_and_match_their_params() {
        assert_eq!(std::mem::size_of::<SkinParams>(), 80);
        let module = naga::front::wgsl::parse_str(SHADER)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
    }

    /// A box fill 2 × 0.5 × 1 m, mirrored in x, at 3 cm point spacing: split
    /// until its longest edge (a face diagonal) is under the spacing, welded
    /// so each face's grid shares its corners, one smoothing group per
    /// position and normal, every group's rest normal its face's outward
    /// axis even though the mirror turned the triangles inside out.
    #[test]
    fn a_mirrored_box_splits_welds_and_keeps_its_faces_outward() {
        let transform = Transform {
            pos: [0.5, 1.0, -0.25],
            scale: [-2.0, 0.5, 1.0],
            ..Transform::default()
        };
        let local = MeshSource::Cube { size: 1.0 }.load_vertices().unwrap();
        let mesh = carried_mesh(local, &transform, 0.03).unwrap();
        let triangles = mesh.indices.len() / 3;
        assert_eq!(
            triangles,
            12 * 4usize.pow(7),
            "2.24 m diagonals halve 7 times to 1.7 cm"
        );
        let longest = mesh
            .indices
            .chunks_exact(3)
            .flat_map(|t| {
                [(0, 1), (1, 2), (2, 0)].map(|(a, b)| {
                    length(sub(
                        mesh.vertices[t[a] as usize].position,
                        mesh.vertices[t[b] as usize].position,
                    ))
                })
            })
            .fold(0.0f32, f32::max);
        assert!(longest <= 0.03, "{longest}");
        // Each face is a 129 × 129 grid of welded corners.
        assert_eq!(mesh.vertices.len(), 6 * 129 * 129);
        assert_eq!(mesh.group_start.len(), mesh.group_normal.len() + 1);
        let centre = [0.5, 1.0, -0.25];
        for (v, vertex) in mesh.vertices.iter().enumerate() {
            let group = mesh.group_normal[mesh.vertex_group[v] as usize];
            let n = [group[0], group[1], group[2]];
            assert!(
                length(sub(n, vertex.normal)) < 1e-4,
                "{n:?} vs {:?}",
                vertex.normal
            );
            let out = sub(vertex.position, centre);
            assert!(
                out[0] * n[0] + out[1] * n[1] + out[2] * n[2] > 0.0,
                "inward at {out:?}"
            );
        }
        for tri in mesh.indices.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|k| mesh.vertices[tri[k] as usize].position);
            let face = cross(sub(b, a), sub(c, a));
            let n = mesh.vertices[tri[0] as usize].normal;
            assert!(
                face[0] * n[0] + face[1] * n[1] + face[2] * n[2] > 0.0,
                "wound inward"
            );
        }
    }

    /// A round fill carries a ball fine enough to look round close up, its
    /// seam and poles in one smoothing group with their neighbours.
    #[test]
    fn a_sphere_fill_carries_a_fine_ball_without_seams() {
        let source = carried_source(Some(&MeshSource::Sphere {
            radius: 0.5,
            segments: 32,
            rings: 16,
        }));
        let local = source.load_vertices().unwrap();
        let transform = Transform {
            scale: [0.8; 3],
            ..Transform::default()
        };
        let mesh = carried_mesh(local, &transform, 0.03).unwrap();
        assert_eq!(mesh.indices.len(), 6 * 128 * 63, "no split: edges are 2 cm");
        // The seam column and the pole fans weld by position and normal.
        assert_eq!(mesh.group_normal.len(), 128 * 63 + 2);
        for (v, vertex) in mesh.vertices.iter().enumerate() {
            let group = mesh.group_normal[mesh.vertex_group[v] as usize];
            let radial = normalized(vertex.position);
            let n = [group[0], group[1], group[2]];
            assert!(length(sub(n, radial)) < 0.02, "{n:?} vs {radial:?}");
        }
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
