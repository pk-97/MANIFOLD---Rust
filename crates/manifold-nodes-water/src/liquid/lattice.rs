//! GPU liquid grids: the simulation lattice retains [`PADDING_NODES`] per
//! side; [`LiquidLattice::surface`] derives the native FLIP mesh/solid grid
//! with 1.5-cell padding. Both retain the authored box and uniform spacing.

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_core::fluid_domain::FluidDomainLayout;
#[cfg(test)]
use manifold_core::fluid_domain::domain_layout;
use manifold_node_engine::scene::transform::Transform;

/// Nodes added outside the authored box on every side (taichi `padding = 3`).
pub const PADDING_NODES: u32 = 3;

/// Native meshing extent: three extra cells total, 1.5 on each side.
pub const SURFACE_PADDING_CELLS: f32 = 1.5;
pub const SURFACE_EXTRA_NODES: u32 = 4;

/// Most nodes per axis a lattice wire may carry, as every lattice atom.
pub const MAX_LATTICE_NODES: u32 = 1024;

/// The optional cell-centred interior field identifies its physical grid by
/// exact length. Native FLIP fills all surface cells (nodes − 1); authored
/// fields from MPM and saved graphs retain their padded-grid counts. The
/// products are strictly ordered, so no valid length is ambiguous.
pub(crate) fn interior_cells(nodes: [u32; 3], values: u64) -> Option<[u32; 3]> {
    [1, SURFACE_EXTRA_NODES, 1 + 2 * PADDING_NODES].into_iter().find_map(|extra| {
        let cells = nodes.map(|n| n.saturating_sub(extra));
        (cells.iter().all(|&n| n > 0)
            && cells.into_iter().map(u64::from).product::<u64>() == values)
            .then_some(cells)
    })
}

/// Native FLIP boundary shrinks the engine box by 3h + 1e-4 total.
/// AABB::expand divides that by two on each side; epsilon is in metres.
pub(crate) const FLIP_WALL_EPSILON: f32 = 5.0e-5;

manifold_core::testkit_visible! {
/// GPU FLIP's MAC grid, distinct from the authored-grid scalar wire contract.
/// The native engine adds three cells and offsets its origin by 1.5h.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FlipSolverGrid {
    surface: LiquidLattice,
}
}

impl FlipSolverGrid {
manifold_core::testkit_visible! {
    pub(crate) fn from_lattice(authored: LiquidLattice) -> Self {
        Self { surface: authored.surface() }
    }
}

manifold_core::testkit_visible! {
    pub(crate) fn cells(self) -> [u32; 3] { self.surface.nodes().map(|n| n - 1) }
}
manifold_core::testkit_visible! {
    pub(crate) fn min(self) -> [f32; 3] { self.surface.min() }
}
    pub(crate) fn nodes(self) -> [u32; 3] { self.surface.nodes() }
    pub(crate) fn bounds(self) -> Transform { self.surface.bounds() }
    pub(crate) fn wall_inset(self) -> f32 {
        SURFACE_PADDING_CELLS + FLIP_WALL_EPSILON / self.surface.cell_size()
    }
}

/// Node (i, j, k) at `min + (i, j, k) · cell_size`. The fields are private:
/// Constructed from a domain layout, then optionally converted to its surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiquidLattice {
    min: [f32; 3],
    nodes: [u32; 3],
    cell_size: f32,
    /// Cells of the authored box per axis.
    cells: [u32; 3],
}

impl LiquidLattice {
    pub fn from_layout(layout: &FluidDomainLayout) -> Self {
        let dx = layout.cell_size as f32;
        let pad = PADDING_NODES as f32 * dx;
        Self {
            min: layout.min.map(|v| v - pad),
            nodes: layout.cells.map(|n| n + 1 + 2 * PADDING_NODES),
            cell_size: dx,
            cells: layout.cells,
        }
    }

    /// Native FLIP meshing grid: authored cells + 3, with one more node.
    /// ParticleMesher::_initialize and Polygonizer3d::_getVertexPosition use
    /// integer node positions; the native origin supplies the half-cell phase.
    pub fn surface(&self) -> Self {
        Self {
            min: self.box_min().map(|v| v - SURFACE_PADDING_CELLS * self.cell_size),
            nodes: self.cells.map(|n| n + SURFACE_EXTRA_NODES),
            cell_size: self.cell_size,
            cells: self.cells,
        }
    }

manifold_core::testkit_visible! {
    /// The lattice a domain published on its scalar wires (`lattice_min_x/y/z`,
    /// `cell_size`, `nodes_x/y/z`; generated uniforms pack scalars only, so
    /// the lattice travels that way). Defaults are the 4 m Dam Break lattice
    /// at resolution 64, matching each atom's param defaults. Wires no padded
    /// layout could have produced are reported as `node`'s error and give
    /// `None`.
    pub(crate) fn from_wires(ctx: &mut EffectNodeContext<'_, '_>, node: &str) -> Option<Self> {
        Self::from_scalars(|name, default| ctx.scalar_or_param(name, default))
            .map_err(|refusal| ctx.error(format!("{node}: {refusal}")))
            .ok()
    }
}

    /// [`Self::from_wires`] over any `scalar_or_param` reader: the extent
    /// checker reads the same wires without a frame. Node counts must be
    /// whole and hold at least one cell inside the padding; the cell size
    /// positive; the corner finite.
    pub(crate) fn from_scalars(read: impl Fn(&str, f32) -> f32) -> Result<Self, String> {
        let least = 2 + 2 * PADDING_NODES;
        let nodes = |name: &str| {
            let value = read(name, 71.0);
            if value.fract() == 0.0 && (least as f32..=MAX_LATTICE_NODES as f32).contains(&value) {
                Ok(value as u32)
            } else {
                Err(format!("lattice wire {name} is {value}; it must be a whole node count from {least} to {MAX_LATTICE_NODES}"))
            }
        };
        let nodes = [nodes("nodes_x")?, nodes("nodes_y")?, nodes("nodes_z")?];
        let cell_size = read("cell_size", 0.0625);
        if !(cell_size.is_finite() && cell_size > 0.0) {
            return Err(format!("lattice wire cell_size is {cell_size}; it must be positive"));
        }
        let corner = |name: &str, default: f32| {
            let value = read(name, default);
            value.is_finite().then_some(value).ok_or_else(|| format!("lattice wire {name} is {value}; it must be finite"))
        };
        let min = [corner("lattice_min_x", -2.1875)?, corner("lattice_min_y", -0.1875)?, corner("lattice_min_z", -2.1875)?];
        Ok(Self { min, nodes, cell_size, cells: nodes.map(|n| n - (1 + 2 * PADDING_NODES)) })
    }

    pub fn min(&self) -> [f32; 3] {
        self.min
    }

    pub fn nodes(&self) -> [u32; 3] {
        self.nodes
    }

    /// Minimum corner of the authored box: the first cell inside the padding.
    pub fn box_min(&self) -> [f32; 3] {
        std::array::from_fn(|d| self.min[d] + (self.nodes[d] - self.cells[d] - 1) as f32 * 0.5 * self.cell_size)
    }

    pub fn cell_size(&self) -> f32 {
        self.cell_size
    }

    pub fn cells(&self) -> [u32; 3] {
        self.cells
    }

    pub fn node_count(&self) -> u32 {
        self.nodes[0] * self.nodes[1] * self.nodes[2]
    }

    /// Bytes of the solid lattice: one f32 per node.
    pub fn solid_bytes(&self) -> u64 {
        self.nodes.iter().map(|&n| u64::from(n)).product::<u64>() * 4
    }

    /// Scene AABB of the lattice nodes (the seam's `grid_bounds`).
    pub fn bounds(&self) -> Transform {
        let size: [f32; 3] = std::array::from_fn(|i| (self.nodes[i] - 1) as f32 * self.cell_size);
        Transform {
            pos: std::array::from_fn(|i| self.min[i] + size[i] * 0.5),
            scale: size,
            ..Transform::default()
        }
    }

    /// Signed distance from each node to the nearest closed wall of the
    /// authored box (positive inside, negative past a closed face): the
    /// seam's solid lattice for a domain whose only solids are its walls.
    /// Open faces contribute nothing; with none closed, every node reads the
    /// lattice diagonal.
    pub fn wall_distance(&self, closed_faces: u32) -> Vec<f32> {
        self.wall_distance_inset(closed_faces, 0.0)
    }

    /// Native FLIP domain object's fixed physical interior epsilon.
    pub(crate) fn flip_wall_distance(&self, closed_faces: u32) -> Vec<f32> {
        self.wall_distance_inset(closed_faces, FLIP_WALL_EPSILON)
    }

    fn wall_distance_inset(&self, closed_faces: u32, inset: f32) -> Vec<f32> {
        let dx = self.cell_size;
        let low = self.box_min().map(|v| v + inset);
        let high: [f32; 3] = std::array::from_fn(|d| low[d] + self.cells[d] as f32 * dx - 2.0 * inset);
        let far = self.nodes.iter().map(|&n| (n as f32 * dx).powi(2)).sum::<f32>().sqrt();
        let [nx, ny, nz] = self.nodes;
        let mut out = Vec::with_capacity(self.node_count() as usize);
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    let p = [i, j, k].map(|c| c as f32 * dx);
                    let mut distance = far;
                    let mut outside_squared = 0.0;
                    for d in 0..3 {
                        let x = self.min[d] + p[d];
                        if closed_faces & (1 << (2 * d)) != 0 {
                            let candidate = x - low[d];
                            distance = distance.min(candidate);
                            outside_squared += candidate.min(0.0).powi(2);
                        }
                        if closed_faces & (1 << (2 * d + 1)) != 0 {
                            let candidate = high[d] - x;
                            distance = distance.min(candidate);
                            outside_squared += candidate.min(0.0).powi(2);
                        }
                    }
                    out.push(if outside_squared > 0.0 { -outside_squared.sqrt() } else { distance });
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flip_solver_grid_matches_native_counts_without_reinterpreting_authored_wires() {
        for resolution in [8, 31, 64] {
            let layout = domain_layout(None, 4.0, resolution).unwrap();
            let authored = LiquidLattice::from_layout(&layout);
            let solver = FlipSolverGrid::from_lattice(authored);
            assert_eq!(authored.cells(), [resolution; 3]);
            assert_eq!(authored.nodes(), [resolution + 7; 3]);
            assert_eq!(authored.box_min(), layout.min);
            assert_eq!(solver.cells(), [resolution + 3; 3]);
            assert_eq!(solver.nodes(), [resolution + 4; 3]);
            assert_eq!(solver.min(), layout.min.map(|x| x - 1.5 * layout.cell_size as f32));
            assert_eq!(solver.bounds(), authored.surface().bounds());
            assert_eq!(authored.surface().solid_bytes(), u64::from(resolution + 4).pow(3) * 4);
            assert_eq!(crate::primitives::gpu_flip_step::face_bytes(solver.cells()),
                u64::from(resolution + 4).pow(3) * std::mem::size_of::<crate::fluid_particles::FaceSample>() as u64);
            assert_eq!(interior_cells(solver.nodes(), u64::from(resolution + 3).pow(3)), Some(solver.cells()));
            assert_eq!(crate::whitewater::face_offset(solver.nodes(), solver.cells()).unwrap(), [0; 3]);
        }
        assert_eq!(crate::primitives::gpu_flip_pressure::level_lattices([67; 3]),
            vec![[67; 3], [34; 3], [17; 3], [9; 3], [5; 3], [3; 3]]);
    }

    #[test]
    fn flip_wall_distance_matches_native_face_edge_corner_and_absolute_epsilon() {
        for size in [1.0, 8.0] {
            let layout = domain_layout(None, size, 8).unwrap();
            let surface = LiquidLattice::from_layout(&layout).surface();
            let n = surface.nodes();
            let distance = surface.flip_wall_distance(63);
            let at = |i, j, k| distance[(i + n[0] * (j + n[1] * k)) as usize];
            // Vendored _getBoundaryAABB: N+3 solver cells, shrink 3h+1e-4
            // total. Mesh distance is Euclidean outside the inverted box.
            let half = 0.5 * layout.cell_size + 5e-5;
            for (coord, axes) in [([1, 5, 5], 1.0f64), ([1, 1, 5], 2.0), ([1, 1, 1], 3.0)] {
                assert!((f64::from(at(coord[0], coord[1], coord[2])) + half * axes.sqrt()).abs() < 1e-6);
            }
            let one_wall = surface.flip_wall_distance(1 << 2);
            let i = (1 + n[0] * (1 + n[1])) as usize;
            assert!((f64::from(one_wall[i]) + half).abs() < 1e-6);
        }
    }

    /// The native engine's padded solid lattice: 1.5 cells outside each wall,
    /// three extra cells per axis, one extra node past them.
    fn native_solid_lattice(layout: &FluidDomainLayout) -> (Transform, [u32; 3]) {
        let origin = layout.min.map(|value| value - (1.5 * layout.cell_size) as f32);
        let size: [f32; 3] =
            std::array::from_fn(|axis| (f64::from(layout.cells[axis] + 3) * layout.cell_size) as f32);
        let bounds = Transform {
            pos: std::array::from_fn(|axis| origin[axis] + size[axis] * 0.5),
            scale: size,
            ..Transform::default()
        };
        (bounds, layout.cells.map(|cells| cells + 4))
    }

    #[test]
    fn surface_lattice_matches_native_engine_nodes_and_crossings() {
        // Independent native formula: config adds 3 cells, native_origin
        // subtracts 1.5h; particlemesher.cpp multiplies by subdivision then
        // adds one; polygonizer3d.cpp emits h * index without another shift.
        for (size, resolution) in [(4.0, 64), (1.0, 8), (6.0, 32)] {
            let layout = domain_layout(None, size, resolution).unwrap();
            let mesh = LiquidLattice::from_layout(&layout).surface();
            let (native_bounds, native_nodes) = native_solid_lattice(&layout);
            assert_eq!(mesh.nodes(), native_nodes);
            assert_eq!(mesh.bounds(), native_bounds);
            assert_eq!(mesh.nodes(), [resolution + 4; 3]);
            if resolution == 64 {
                assert_eq!(mesh.min(), [-2.09375, -0.09375, -2.09375]);
                assert_eq!(mesh.bounds().scale, [4.1875; 3]);
            }
            for axis in 0..3 {
                let h = layout.cell_size;
                let origin = f64::from(layout.min[axis]) - 1.5 * h;
                assert_eq!(f64::from(mesh.min()[axis]), origin);
                for subdivision in [1, 2, 3] {
                    let nodes = (resolution + 3) * subdivision + 1;
                    let step = h / f64::from(subdivision);
                    for i in 0..nodes {
                        let native = origin + f64::from(i) * step;
                        let actual = f64::from(mesh.min()[axis])
                            + f64::from(i) * f64::from(mesh.cell_size()) / f64::from(subdivision);
                        assert!((native - actual).abs() < 1e-12);
                    }
                }
            }
            let field = mesh.wall_distance(1 << 2);
            let nx = mesh.nodes()[0] as usize;
            let a = f64::from(field[nx]);
            let b = f64::from(field[2 * nx]);
            assert_eq!((a, b), (-0.5 * layout.cell_size, 0.5 * layout.cell_size));
            let crossing = f64::from(mesh.min()[1]) + (1.0 - a / (b - a)) * layout.cell_size;
            assert_eq!(crossing, f64::from(layout.min[1]));
        }
    }

    #[test]
    fn interior_grid_counts_identify_padding_and_reject_short_fields() {
        for cells in [[8u32; 3], [32, 16, 8], [64; 3]] {
            let values = cells.into_iter().map(u64::from).product::<u64>();
            for extra in [4, 7] {
                let nodes = cells.map(|n| n + extra);
                assert_eq!(interior_cells(nodes, values), Some(cells));
                assert_eq!(interior_cells(nodes, values - 1), None);
                assert_eq!(interior_cells(nodes, 0), None);
            }
        }
    }

    #[test]
    fn liquid_lattice_pads_the_authored_box() {
        let layout = domain_layout(None, 4.0, 64).unwrap();
        let lattice = LiquidLattice::from_layout(&layout);
        assert_eq!(lattice.nodes(), [71; 3]);
        assert_eq!(lattice.cell_size(), 0.0625);
        assert_eq!(lattice.min(), [-2.1875, -0.1875, -2.1875]);
        assert_eq!(lattice.bounds().scale, [4.375; 3]);
    }

    /// Wires no padded layout could have produced: the reader names the wire
    /// instead of clamping to a one-node lattice.
    #[test]
    fn liquid_lattice_wires_refuse_by_name() {
        let layout = domain_layout(None, 4.0, 32).unwrap();
        let lattice = LiquidLattice::from_layout(&layout);
        let wires = |name: &str| match name {
            "nodes_x" | "nodes_y" | "nodes_z" => lattice.nodes()[0] as f32,
            "cell_size" => lattice.cell_size(),
            _ => lattice.min()[0],
        };
        let read = LiquidLattice::from_scalars(|name, _| wires(name)).unwrap();
        assert_eq!((read.nodes(), read.cells()), (lattice.nodes(), lattice.cells()));
        let zeroed = LiquidLattice::from_scalars(|name, default| if name == "nodes_y" { 0.0 } else { default });
        assert!(zeroed.unwrap_err().contains("nodes_y is 0"));
        for (wire, value) in [("nodes_x", 7.0), ("nodes_x", 71.5), ("nodes_z", f32::NAN), ("cell_size", 0.0), ("lattice_min_y", f32::INFINITY)] {
            let refused = LiquidLattice::from_scalars(|name, default| if name == wire { value } else { default });
            assert!(refused.unwrap_err().contains(wire), "{wire} = {value}");
        }
    }

    #[test]
    fn liquid_wall_distance_is_signed_distance_to_closed_faces() {
        let layout = domain_layout(None, 1.0, 8).unwrap();
        let lattice = LiquidLattice::from_layout(&layout);
        let dx = lattice.cell_size();
        let all = lattice.wall_distance(63);
        let n = lattice.nodes();
        let at = |i: u32, j: u32, k: u32| all[((k * n[1] + j) * n[0] + i) as usize];
        // The authored floor sits on node 3; one node below reads −dx.
        assert!((at(7, 3, 7)).abs() < 1e-6);
        assert!((at(7, 2, 7) + dx).abs() < 1e-6);
        assert!((at(7, 5, 7) - 2.0 * dx).abs() < 1e-5);
        // An open top: the node above the ceiling is not inside a solid.
        let open_top = lattice.wall_distance(63 & !(1 << 3));
        let top = n[1] - 1;
        assert!(open_top[((7 * n[1] + top) * n[0] + 7) as usize] > 0.0);
    }

    /// With every face closed, every padding node is solid (a node or more
    /// behind a wall) and no node of the authored box is, at every size and
    /// resolution. The walls come from the layout alone, so no surface dial
    /// can move them.
    #[test]
    fn liquid_lattice_padding_is_solid_behind_closed_walls() {
        for domain in [0.5f32, 4.0, 20.0] {
            for resolution in [8u32, 33, 64] {
                let layout = domain_layout(None, domain, resolution).unwrap();
                let lattice = LiquidLattice::from_layout(&layout);
                let distance = lattice.wall_distance(63);
                let half_cell = 0.5 * lattice.cell_size();
                let [nx, ny, nz] = lattice.nodes();
                let pad = PADDING_NODES;
                for k in 0..nz {
                    for j in 0..ny {
                        for i in 0..nx {
                            let padding = [i, j, k]
                                .iter()
                                .zip(lattice.nodes())
                                .any(|(&c, n)| c < pad || c > n - 1 - pad);
                            let value = distance[(i + nx * (j + ny * k)) as usize];
                            assert_eq!(
                                value < -half_cell,
                                padding,
                                "domain {domain} resolution {resolution} node ({i}, {j}, {k}): distance {value}"
                            );
                        }
                    }
                }
            }
        }
    }










}
