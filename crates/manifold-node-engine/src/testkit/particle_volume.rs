    fn index(p: [usize; 3], n: [usize; 3]) -> usize {
        p[0] + n[0] * (p[1] + n[1] * p[2])
    }
    pub(crate) fn trilinear_interior(
        interior: &[f32],
        p: [f32; 3],
        lattice_min: [f32; 3],
        size: [f32; 3],
        solid_nodes: [usize; 3],
    ) -> f32 {
        let cells = crate::water::liquid::lattice::interior_cells(solid_nodes.map(|n| n as u32), interior.len() as u64).expect("valid interior grid").map(|n| n as usize);
        assert_eq!(interior.len(), cells.iter().product::<usize>());
        let spacing: [f32; 3] =
            std::array::from_fn(|axis| size[axis] / (solid_nodes[axis] - 1) as f32);
        let top = cells.map(|n| n - 1);
        let mut g = [0.0; 3];
        for axis in 0..3 {
            let physical_min = lattice_min[axis] + (solid_nodes[axis] - cells[axis] - 1) as f32 * 0.5 * spacing[axis];
            g[axis] = ((p[axis] - physical_min) / spacing[axis] - 0.5).clamp(0.0, top[axis] as f32);
        }
        let base = g.map(|v| v.floor() as usize);
        let f: [f32; 3] = std::array::from_fn(|axis| g[axis] - base[axis] as f32);
        let mut value = 0.0;
        for corner in 0..8 {
            let offset = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
            let at = std::array::from_fn(|axis| (base[axis] + offset[axis]).min(top[axis]));
            let weight = (0..3)
                .map(|axis| {
                    if offset[axis] == 1 {
                        f[axis]
                    } else {
                        1.0 - f[axis]
                    }
                })
                .product::<f32>();
            value += weight * interior[index(at, cells)];
        }
        value
    }
    pub fn union(
        particle_phi: f32,
        interior: Option<&[f32]>,
        p: [f32; 3],
        lattice_min: [f32; 3],
        size: [f32; 3],
        solid_nodes: [usize; 3],
    ) -> f32 {
        let Some(interior) = interior else {
            return particle_phi;
        };
        assert!(!interior.is_empty(), "wired interior must cover its physical cells");
        let spacing: [f32; 3] =
            std::array::from_fn(|axis| size[axis] / (solid_nodes[axis] - 1) as f32);
        let h = spacing.into_iter().fold(f32::INFINITY, f32::min);
        particle_phi.min(trilinear_interior(interior, p, lattice_min, size, solid_nodes) + h)
    }
