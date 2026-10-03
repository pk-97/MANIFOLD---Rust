//! GPU liquid grids: the simulation lattice retains [`PADDING_NODES`] per
//! side; [`LiquidLattice::surface`] derives the native FLIP mesh/solid grid
//! with 1.5-cell padding. Both retain the authored box and uniform spacing.

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid::FluidDomainLayout;
use crate::node_graph::transform::Transform;
use crate::node_graph::parameters::ParamValue;

/// Nodes added outside the authored box on every side (taichi `padding = 3`).
pub const PADDING_NODES: u32 = 3;

/// Native meshing extent: three extra cells total, 1.5 on each side.
pub const SURFACE_PADDING_CELLS: f32 = 1.5;
pub const SURFACE_EXTRA_NODES: u32 = 4;

/// Most nodes per axis a lattice wire may carry, as every lattice atom.
pub const MAX_LATTICE_NODES: u32 = 1024;

/// The optional cell-centred interior field identifies its physical grid by
/// exact length. Support the native mesh (1.5h padding) and existing solver
/// lattices (3h); their products are strictly ordered, so never ambiguous.
pub(crate) fn interior_cells(nodes: [u32; 3], values: u64) -> Option<[u32; 3]> {
    [SURFACE_EXTRA_NODES, 1 + 2 * PADDING_NODES].into_iter().find_map(|extra| {
        let cells = nodes.map(|n| n.saturating_sub(extra));
        (cells.iter().all(|&n| n > 0)
            && cells.into_iter().map(u64::from).product::<u64>() == values)
            .then_some(cells)
    })
}

/// Old embedded graphs feed a simulation-grid solid. New builders explicitly
/// opt into the native grid when they also wire the separately sampled solid.
pub(crate) fn frame_lattice(lattice: LiquidLattice, params: &crate::node_graph::effect_node::ParamValues) -> LiquidLattice {
    if matches!(params.get("native_mesh_grid"), Some(ParamValue::Bool(true))) {
        lattice.surface()
    } else {
        lattice
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
        let dx = self.cell_size;
        let low = self.box_min();
        let high: [f32; 3] = std::array::from_fn(|d| low[d] + self.cells[d] as f32 * dx);
        let far = self.nodes.iter().map(|&n| (n as f32 * dx).powi(2)).sum::<f32>().sqrt();
        let [nx, ny, nz] = self.nodes;
        let mut out = Vec::with_capacity(self.node_count() as usize);
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    let p = [i, j, k].map(|c| c as f32 * dx);
                    let mut distance = far;
                    for d in 0..3 {
                        let x = self.min[d] + p[d];
                        if closed_faces & (1 << (2 * d)) != 0 {
                            distance = distance.min(x - low[d]);
                        }
                        if closed_faces & (1 << (2 * d + 1)) != 0 {
                            distance = distance.min(high[d] - x);
                        }
                    }
                    out.push(distance);
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
    fn surface_lattice_matches_native_engine_nodes_and_crossings() {
        // Independent native formula: config adds 3 cells, native_origin
        // subtracts 1.5h; particlemesher.cpp multiplies by subdivision then
        // adds one; polygonizer3d.cpp emits h * index without another shift.
        for (size, resolution) in [(4.0, 64), (1.0, 8), (6.0, 32)] {
            let layout = crate::node_graph::fluid::domain_layout(None, size, resolution).unwrap();
            let mesh = LiquidLattice::from_layout(&layout).surface();
            let (native_bounds, native_nodes) = layout.solid_lattice();
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
        let layout = crate::node_graph::fluid::domain_layout(None, 4.0, 64).unwrap();
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
        let layout = crate::node_graph::fluid::domain_layout(None, 4.0, 32).unwrap();
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
        let layout = crate::node_graph::fluid::domain_layout(None, 1.0, 8).unwrap();
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
                let layout = crate::node_graph::fluid::domain_layout(None, domain, resolution).unwrap();
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

    /// Every bundled host preset, scene modifiers expanded and groups
    /// flattened. Scene-modifier recipes prepare only on a host scene, so the
    /// hosts are what the wiring guards walk.
    fn flat_bundled_hosts() -> Vec<(String, manifold_core::effect_graph_def::EffectGraphDef)> {
        use crate::node_graph::bundled_presets::{bundled_preset_def, bundled_preset_type_ids};
        use manifold_core::preset_def::PresetKind;

        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let mut hosts = Vec::new();
        for kind in [PresetKind::Effect, PresetKind::Generator] {
            for type_id in bundled_preset_type_ids(kind) {
                let def = bundled_preset_def(&type_id).expect("bundled preset");
                let expanded = crate::node_graph::scene_modifier_expand::expand_scene_modifiers(def, &registry)
                    .unwrap_or_else(|error| panic!("{type_id}: {error}"));
                let flat = manifold_core::flatten::flatten_groups(&expanded)
                    .unwrap_or_else(|error| panic!("{type_id}: {error}"));
                hosts.push((type_id.to_string(), flat));
            }
        }
        hosts
    }

    /// The one wire into `id.port`, as (from node, from port).
    fn source<'a>(
        type_id: &str,
        flat: &'a manifold_core::effect_graph_def::EffectGraphDef,
        id: u32,
        port: &str,
    ) -> (u32, &'a str) {
        let mut wires = flat.wires.iter().filter(|w| w.to_node == id && w.to_port == port);
        let wire = wires.next().unwrap_or_else(|| panic!("{type_id}: {port} is not wired"));
        assert!(wires.next().is_none(), "{type_id}: {port} has two sources");
        (wire.from_node, wire.from_port.as_str())
    }

    fn type_of(flat: &manifold_core::effect_graph_def::EffectGraphDef, id: u32) -> Option<&str> {
        flat.nodes.iter().find(|n| n.id == id).map(|n| n.type_id.as_str())
    }

    /// Every bundled Liquid Surface meshes on the lattice a solver's frame
    /// node published: its solid, node counts and box are wired straight from
    /// one node.fluid_surface, node.matter_frame or node.liquid_frame, never
    /// a hand-made transform or value that could drop the padding.
    #[test]
    fn liquid_surface_lattice_comes_from_the_frame() {
        const FRAMES: [&str; 3] =
            [manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID, "node.matter_frame", "node.liquid_frame"];
        let mut checked = Vec::new();
        for (type_id, flat) in flat_bundled_hosts() {
            let source = |id: u32, port: &str| source(&type_id, &flat, id, port);
            for volume in flat.nodes.iter().filter(|n| n.type_id == "node.particle_volume") {
                let (frame, port) = source(volume.id, "solid");
                assert!(FRAMES.contains(&type_of(&flat, frame).unwrap_or("")), "{type_id}: solid from {port}");
                assert!(matches!(port, "solid_a" | "solid_b"), "{type_id}: solid from {port}");
                let (box_node, _) = source(volume.id, "center_x");
                assert_eq!(type_of(&flat, box_node), Some("node.transform_components"), "{type_id}: lattice box");
                assert_eq!(source(box_node, "transform"), (frame, "grid_bounds"), "{type_id}: lattice box");
                for axis in ["x", "y", "z"] {
                    let grid_nodes = format!("grid_nodes_{axis}");
                    assert_eq!(source(volume.id, &format!("nodes_{axis}")), (frame, grid_nodes.as_str()), "{type_id}");
                    for (to, from) in [("center", "pos"), ("size", "scale")] {
                        let port = format!("{from}_{axis}");
                        assert_eq!(source(volume.id, &format!("{to}_{axis}")), (box_node, port.as_str()), "{type_id}");
                    }
                }
                checked.push(type_id.clone());
            }
        }
        assert!(!checked.is_empty(), "no bundled preset meshes a liquid surface");
    }

    /// Every bundled liquid mesher reads the clamped level set: the solid and
    /// border clamp is the last step before meshing (BUG-koy0 (solid clamp
    /// before smoothing)), after any smoothing, on the same solid, box, bin
    /// size and lattice as the volume it clamps.
    #[test]
    fn liquid_surface_meshes_the_clamped_level_set() {
        const MESHERS: [&str; 2] = ["node.count_surface_triangles", "node.volume_surface_mesh"];
        let mut checked = 0;
        for (type_id, flat) in flat_bundled_hosts() {
            let source = |id: u32, port: &str| source(&type_id, &flat, id, port);
            for mesher in flat.nodes.iter().filter(|n| MESHERS.contains(&n.type_id.as_str())) {
                let (clamp, port) = source(mesher.id, "levelset");
                assert_eq!(type_of(&flat, clamp), Some("node.clamp_liquid_to_solids"), "{type_id}: mesher reads {port}");
                assert_eq!(port, "clamped", "{type_id}");
                // Back through the smoothing chain to the volume.
                let mut upstream = source(clamp, "levelset");
                while type_of(&flat, upstream.0) == Some("node.smooth_lattice") {
                    upstream = source(upstream.0, "levelset");
                }
                // Optional closing is three distinct operations: grow,
                // rebuild distance, shrink. Solids still clip last.
                if type_of(&flat, upstream.0) == Some("node.offset_lattice") {
                    assert_eq!(upstream.1, "out");
                    upstream = source(upstream.0, "levelset");
                    assert_eq!(type_of(&flat, upstream.0), Some("node.redistance_lattice"));
                    assert_eq!(upstream.1, "out");
                    upstream = source(upstream.0, "levelset");
                    assert_eq!(type_of(&flat, upstream.0), Some("node.offset_lattice"));
                    assert_eq!(upstream.1, "out");
                    upstream = source(upstream.0, "levelset");
                }
                let volume = upstream.0;
                assert_eq!(upstream, (volume, "levelset"), "{type_id}: the clamp's level set");
                assert_eq!(type_of(&flat, volume), Some("node.particle_volume"), "{type_id}: the clamp's level set");
                assert_eq!(source(clamp, "solid"), source(volume, "solid"), "{type_id}: solid");
                assert_eq!(source(clamp, "cell_size"), source(volume, "cell_size"), "{type_id}: bin size");
                for axis in ["x", "y", "z"] {
                    let volume_nodes = format!("volume_nodes_{axis}");
                    assert_eq!(source(clamp, &format!("nodes_{axis}")), (volume, volume_nodes.as_str()), "{type_id}");
                    let solid_nodes = source(clamp, &format!("solid_nodes_{axis}"));
                    assert_eq!(solid_nodes, source(volume, &format!("nodes_{axis}")), "{type_id}");
                    for side in ["center", "size"] {
                        let port = format!("{side}_{axis}");
                        assert_eq!(source(clamp, &port), source(volume, &port), "{type_id}: {port}");
                    }
                }
                checked += 1;
            }
        }
        assert!(checked > 0, "no bundled preset meshes a liquid surface");
    }
}
