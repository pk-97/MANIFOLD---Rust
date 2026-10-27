// Sealed pockets (PressureSolver::_conditionSolidVelocityField): water a
// solid closes off from air cannot take the solid's push, so its solid face
// velocity is zeroed and the pressure solve stays consistent. Water cells
// link through a face whose open fraction is at least POCKET_LINK. A water
// cell touches air through a linked face to a dry cell
// (_computeBordersAirGridThread; pocket_seed_reason). Whether a region
// reaches air spreads from those cells
// by line sweeps along each axis, repeated until a round changes nothing.
//
// The states and labels are pooled cell fields, indexed by pool index
// (world_tiles_read.wgsl); a pocket's label is the lowest pool index among
// its cells, which is the same on every run because the slot table is.
@group(0) @binding(23) var<storage, read_write> pocket: array<u32>;

// Words 0-8: the three sweeps' indirect dispatch sizes (x, y, z); 9: a
// sweep changed a cell this round; 10: a sealed cell still links to one
// that reaches air (the spread stopped at its cap unfinished); 11: the held
// slots the sweeps start their lines in (set by pocket_start);
// 12-13: the sweep-and-setup replay range {0, 4} or {0, 0};
// 14-16: the next round setup's indirect dispatch size.
@group(0) @binding(24) var<storage, read_write> pocket_gate: array<u32>;

// Each water cell's pocket: the lowest pool index of the sealed water it
// links to. A pocket's label is its leader cell's pool index, so the leader
// is the cell whose label is its own index. A coarse level's labels are
// pool indices of the level's pool.
@group(0) @binding(25) var<storage, read_write> pocket_label: array<u32>;

// Words 0-1: the magnitude of everything removed this solve, a 64-bit
// fixed-point integer (low, high), taken to 0 again by pocket_flux. Then
// three words per label, from word 2: a right-hand side sum as a 64-bit
// fixed-point integer (low, high) and the cell count. The array is indexed
// by label (a fine cell's pool index, or a level cell's at a coarse level),
// not pooled itself.
@group(0) @binding(26) var<storage, read_write> pocket_sum: array<atomic<u32>>;
