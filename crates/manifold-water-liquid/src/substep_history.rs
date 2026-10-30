//! Accepted liquid substeps on the solver's world tiles.
//! Four f32 words per schedule row: duration, elapsed endpoint, bitcast
//! timestamped impulse index/valid bit, reserved zero. Zero duration is inactive.
//! The faces concatenate one pooled grid per row in schedule order: row k's
//! element e is at (k · pool_len(P) + e) · 4, the x, y and z velocity of the
//! face record of the pool index e (a face is its cell's low face along the
//! axis, 0 where the face has no weight) and a zero. Every row lies on the
//! table the step publishes at the frame's end, so a reader finds a face
//! with `ts_face` on that table whichever substep it reads. Inactive rows do
//! not publish faces; consumers must skip them before reading a row.
//!
//! The step's table changes from substep to substep (a tile can change slot
//! or come and go), so each capture keeps its table's words beside the faces,
//! and after the frame's last capture [`SubstepHistory::remap`] moves every
//! captured row onto the final table: a tile the final table lacks drops out,
//! a tile the substep lacked reads the canonical element.
use crate::fluid_particles::FaceSample;
use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};
use manifold_node_engine::scene::world_tiles::{READ_SIDE, pool_len, table_words_at};

/// Indirect dispatch words per row: the table copy's groups, then the
/// faces', each three words.
const ROW_ARGUMENT_BYTES: u64 = 2 * 3 * 4;
/// The words a pass reads as its uniform.
const PARAM_WORDS: usize = 8;

// The passes. The table of the substep a pass reads through (`ts_*`, from the
// world tiles' read side) is its row of `tables`, the copy the capture made.
// The GPU owns activity: the schedule pass writes each row's indirect
// arguments, zero groups for an inactive row, and no readback is needed.
const HISTORY_SHADER: &str = r#"
struct FaceSample { face_velocity: vec4<f32>, face_weight: vec4<f32> }
struct Params {
    row: u32,
    table_groups: u32,
    element_groups: u32,
    pool_tiles: u32,
    pool_len: u32,
    table_words: u32,
    entries: u32,
    _pad: u32,
}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> plan: array<u32>;
@group(0) @binding(2) var<storage, read_write> schedule: array<u32>;
@group(0) @binding(3) var<storage, read_write> arguments: array<u32>;
@group(0) @binding(4) var<storage, read> faces: array<FaceSample>;
@group(0) @binding(5) var<storage, read> live_table: array<u32>;
@group(0) @binding(6) var<storage, read_write> tables: array<u32>;
@group(0) @binding(7) var<storage, read_write> history: array<f32>;
@group(0) @binding(8) var<storage, read_write> stash: array<f32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return tables[params.row * params.table_words + i]; }

@compute @workgroup_size(1)
fn capture_schedule() {
    let row = params.row * 4u;
    schedule[row] = plan[0];
    schedule[row + 1u] = plan[1];
    schedule[row + 2u] = plan[7];
    schedule[row + 3u] = 0u;
    let enabled = bitcast<f32>(plan[0]) > 0.0;
    let at = params.row * 6u;
    arguments[at] = select(0u, params.table_groups, enabled);
    arguments[at + 1u] = 1u;
    arguments[at + 2u] = 1u;
    arguments[at + 3u] = select(0u, params.element_groups, enabled);
    arguments[at + 4u] = 1u;
    arguments[at + 5u] = 1u;
}

// The step's table, kept as row `row` of `tables`.
@compute @workgroup_size(256)
fn capture_table(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.table_words { return; }
    tables[params.row * params.table_words + gid.x] = live_table[gid.x];
}

// The step's faces as row `row`: a face without weight reads 0, as the
// whitewater atoms read it (lf_face_velocity). A slot the step's table does
// not hold is left as it was: the remap reads a row's element only through a
// tile the row's table holds, or the canonical element (past the slots).
@compute @workgroup_size(256)
fn capture_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    let e = gid.x;
    if e >= params.pool_len { return; }
    let slot = e >> 9u;
    if slot < params.pool_tiles && live_table[WT_HEADER + 2u * params.entries + 4u * slot + 3u] == WT_NONE { return; }
    let s = faces[e];
    let at = (params.row * params.pool_len + e) * 4u;
    history[at] = select(0.0, s.face_velocity.x, s.face_weight.x > 0.0);
    history[at + 1u] = select(0.0, s.face_velocity.y, s.face_weight.y > 0.0);
    history[at + 2u] = select(0.0, s.face_velocity.z, s.face_weight.z > 0.0);
    history[at + 3u] = 0.0;
}

// Row `row` as it lies, one row of scratch for the remap to read.
@compute @workgroup_size(256)
fn stash_row(@builtin(global_invocation_id) gid: vec3<u32>) {
    let e = gid.x;
    if e >= params.pool_len { return; }
    let row_at = (params.row * params.pool_len + e) * 4u;
    for (var a = 0u; a < 4u; a = a + 1u) {
        stash[e * 4u + a] = history[row_at + a];
    }
}

// Row `row` rewritten onto the final table (`live_table`): element e holds the
// tile of the final table's slot e >> 9, which the row's own table (the copy
// in `tables`) finds under whatever slot it had then. The canonical element,
// a free slot and a tile the substep did not hold read the stash's canonical
// element.
@compute @workgroup_size(256)
fn remap_row(@builtin(global_invocation_id) gid: vec3<u32>) {
    let e = gid.x;
    if e >= params.pool_len { return; }
    var src = params.pool_len - 1u;
    let slot = e >> 9u;
    if slot < params.pool_tiles {
        let word = WT_HEADER + 2u * params.entries + 4u * slot;
        if live_table[word + 3u] != WT_NONE {
            let tile = bitcast<vec3<i32>>(vec3<u32>(live_table[word], live_table[word + 1u], live_table[word + 2u]));
            let held = ts_hash_slot(tile);
            if held != WT_NONE {
                src = held * 512u + (e & 511u);
            }
        }
    }
    let dst = (params.row * params.pool_len + e) * 4u;
    for (var a = 0u; a < 4u; a = a + 1u) {
        history[dst + a] = stash[src * 4u + a];
    }
}
"#;

const ENTRIES: [&str; 5] = [
    "capture_schedule",
    "capture_table",
    "capture_faces",
    "stash_row",
    "remap_row",
];

/// The passes' kernels: the world tiles' read side, then the history's.
fn history_source() -> String {
    format!("{READ_SIDE}\n{HISTORY_SHADER}")
}

manifold_core::testkit_visible! {
/// Bytes the history holds for `slots` rows over a pool of `pool_tiles`
/// tiles and a table of `entries`: per row its schedule words, indirect
/// arguments, table copy and faces, and one row of scratch for the remap.
pub(crate) fn history_bytes(pool_tiles: u32, entries: u32, slots: u32) -> u64 {
    let row = pool_len(pool_tiles) * 16;
    let table = table_words_at(entries, pool_tiles) * 4;
    row + u64::from(slots) * (16 + ROW_ARGUMENT_BYTES + table + row)
}
}

/// The step's pooled face records as the history reads them.
#[derive(Clone, Copy)]
pub struct PooledFaces<'a> {
    pub faces: &'a GpuBuffer,
    /// The step's world-tile table over `pool_tiles` tiles, of `entries`.
    pub table: &'a GpuBuffer,
    pub pool_tiles: u32,
    pub entries: u32,
}

#[derive(Default)]
pub struct SubstepHistory {
    pub schedule: Option<GpuBuffer>,
    /// `slots · pool_len(P) · 4` floats: row k's element e at `(k · pool_len +
    /// e) · 4`, the x, y and z face velocity and a zero. After
    /// [`SubstepHistory::remap`] every active row lies on the final table.
    pub faces: Option<GpuBuffer>,
    pipelines: Vec<GpuComputePipeline>,
    /// Each captured row's table words, `table_words_at(H, P)` a row.
    tables: Option<GpuBuffer>,
    /// One row of faces: the remap's source while it rewrites a row.
    stash: Option<GpuBuffer>,
    arguments: Option<GpuBuffer>,
    clear_faces: bool,
    /// The pool's tiles, the table's entries, and the rows reserved.
    shape: Option<(u32, u32, u32)>,
}
impl SubstepHistory {
    pub fn prepare(&mut self, device: &GpuDevice) {
        if self.pipelines.is_empty() {
            let source = history_source();
            for entry in ENTRIES {
                self.pipelines.push(device.create_compute_pipeline(
                    &source,
                    entry,
                    "liquid.substep_history",
                ));
            }
        }
    }
    pub fn reserve(
        &mut self,
        device: &GpuDevice,
        pool_tiles: u32,
        entries: u32,
        slots: u32,
    ) -> Result<(), String> {
        if self.shape.is_some_and(|(old, old_entries, capacity)| {
            old == pool_tiles && old_entries == entries && slots <= capacity
        }) {
            return Ok(());
        }
        manifold_node_engine::load::expand::admit_world_bytes(
            device.modifier_memory_snapshot(),
            history_bytes(pool_tiles, entries, slots),
        )
        .map_err(|e| e.to_string())?;
        let rows = u64::from(slots);
        let row_bytes = pool_len(pool_tiles) * 16;
        let schedule = device.try_create_buffer(rows * 16, BufferUse::Upload)?;
        schedule.zero_fill();
        let faces = device.try_create_buffer(rows * row_bytes, BufferUse::Gpu)?;
        let tables = device.try_create_buffer(
            rows * table_words_at(entries, pool_tiles) * 4,
            BufferUse::Gpu,
        )?;
        let stash = device.try_create_buffer(row_bytes, BufferUse::Gpu)?;
        let arguments = device.try_create_buffer(rows * ROW_ARGUMENT_BYTES, BufferUse::Gpu)?;
        self.faces = Some(faces);
        self.schedule = Some(schedule);
        self.tables = Some(tables);
        self.stash = Some(stash);
        self.arguments = Some(arguments);
        self.clear_faces = true;
        self.shape = Some((pool_tiles, entries, slots));
        Ok(())
    }

    /// The uniform of the passes over row `row`.
    fn params(row: u32, pool_tiles: u32, entries: u32) -> [u32; PARAM_WORDS] {
        let words = table_words_at(entries, pool_tiles) as u32;
        [
            row,
            words.div_ceil(256),
            (pool_len(pool_tiles) as u32).div_ceil(256),
            pool_tiles,
            pool_len(pool_tiles) as u32,
            words,
            entries,
            0,
        ]
    }

    /// GPU FLIP adapts its scheduler and native face records here: `pooled`
    /// holds its pooled records and the table that finds them. Row `index`
    /// keeps the faces and a copy of the table as this substep leaves them;
    /// an inactive row (no time to step) keeps what it had. Consumers never
    /// see either solver-private layout.
    pub fn capture(
        &mut self,
        enc: &mut GpuEncoder,
        index: u32,
        plan: &GpuBuffer,
        pooled: &PooledFaces<'_>,
    ) {
        let (pool_tiles, entries, slots) = self.shape.expect("history reserved");
        assert!(index < slots);
        assert_eq!(
            (pooled.pool_tiles, pooled.entries),
            (pool_tiles, entries),
            "history reserved for this table"
        );
        let schedule = self.schedule.as_ref().expect("schedule reserved");
        let arguments = self.arguments.as_ref().expect("arguments reserved");
        let tables = self.tables.as_ref().expect("tables reserved");
        let faces = self.faces.as_ref().expect("faces reserved");
        if self.clear_faces {
            enc.clear_buffer(faces);
            self.clear_faces = false;
        }
        let [capture_schedule, capture_table, capture_faces, ..] = &self.pipelines[..] else {
            panic!("history prepared");
        };
        let words = Self::params(index, pool_tiles, entries);
        let uniform = GpuBinding::Bytes {
            binding: 0,
            data: bytemuck::cast_slice(&words),
        };
        enc.dispatch_compute(
            capture_schedule,
            &[
                uniform,
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: plan,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: schedule,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: arguments,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "liquid.substep_history.schedule",
        );
        enc.compute_memory_barrier_buffers();
        let row_arguments = u64::from(index) * ROW_ARGUMENT_BYTES;
        enc.dispatch_compute_indirect(
            capture_table,
            &[
                uniform,
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: pooled.table,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: tables,
                    offset: 0,
                },
            ],
            arguments,
            row_arguments,
            "liquid.substep_history.table",
        );
        enc.dispatch_compute_indirect(
            capture_faces,
            &[
                uniform,
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: pooled.faces,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: pooled.table,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 7,
                    buffer: faces,
                    offset: 0,
                },
            ],
            arguments,
            row_arguments + 12,
            "liquid.substep_history.faces",
        );
        enc.compute_memory_barrier_buffers();
    }

    /// After the frame's last capture: every active row of the first `rows`
    /// rewritten onto `table`, the table the step publishes. Row k's element
    /// for a tile of `table` is the row's own element for that tile, wherever
    /// the tile sat when the row was captured; a tile `table` lacks drops out
    /// and a tile the row lacked reads the row's canonical element. An
    /// inactive row dispatches nothing.
    pub fn remap(&self, enc: &mut GpuEncoder, table: &GpuBuffer, rows: u32) {
        let (pool_tiles, entries, slots) = self.shape.expect("history reserved");
        assert!(rows <= slots);
        let arguments = self.arguments.as_ref().expect("arguments reserved");
        let tables = self.tables.as_ref().expect("tables reserved");
        let faces = self.faces.as_ref().expect("faces reserved");
        let stash = self.stash.as_ref().expect("stash reserved");
        let [_, _, _, stash_row, remap_row] = &self.pipelines[..] else {
            panic!("history prepared");
        };
        for row in 0..rows {
            let words = Self::params(row, pool_tiles, entries);
            let uniform = GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::cast_slice(&words),
            };
            let row_arguments = u64::from(row) * ROW_ARGUMENT_BYTES + 12;
            enc.dispatch_compute_indirect(
                stash_row,
                &[
                    uniform,
                    GpuBinding::Buffer {
                        binding: 7,
                        buffer: faces,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 8,
                        buffer: stash,
                        offset: 0,
                    },
                ],
                arguments,
                row_arguments,
                "liquid.substep_history.stash",
            );
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute_indirect(
                remap_row,
                &[
                    uniform,
                    GpuBinding::Buffer {
                        binding: 5,
                        buffer: table,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 6,
                        buffer: tables,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 7,
                        buffer: faces,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 8,
                        buffer: stash,
                        offset: 0,
                    },
                ],
                arguments,
                row_arguments,
                "liquid.substep_history.remap",
            );
            enc.compute_memory_barrier_buffers();
        }
    }
}

const _: () = assert!(size_of::<FaceSample>() == 32);

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::scene::world_tiles::table_entries;

    #[test]
    fn liquid_substep_history_shader_validates() {
        let source = history_source();
        let module = naga::front::wgsl::parse_str(&source).expect("history WGSL parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("history WGSL validates");
        for entry in ENTRIES {
            assert!(
                module.entry_points.iter().any(|e| e.name == entry),
                "{entry} is an entry point"
            );
        }
    }

    /// The faces of a row are `pool_len` records of four floats, whatever the
    /// pool, and each row adds its schedule, arguments, table copy and faces;
    /// the remap's scratch is one row, not one per slot.
    #[test]
    fn liquid_substep_history_bytes_count_every_buffer() {
        for pool in [1, 3, 8, 4096] {
            let row = pool_len(pool) * 16;
            // Its own H, and one sized for a shared key set.
            for entries in [table_entries(pool), table_entries(4 * pool)] {
                let per_slot =
                    16 + ROW_ARGUMENT_BYTES + table_words_at(entries, pool) * 4 + row;
                for slots in [1, 6, 7, 262] {
                    assert_eq!(
                        history_bytes(pool, entries, slots),
                        row + u64::from(slots) * per_slot,
                        "{pool} tiles, {entries} entries, {slots} slots"
                    );
                }
            }
        }
    }
}
