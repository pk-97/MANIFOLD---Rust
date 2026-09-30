//! FLIP's own whitewater fields and emitter as test oracles
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.7, O1 and O2). Built only with
//! the `whitewater-oracle` feature; nothing in the product calls them.

use std::ffi::c_void;

use crate::{FluidError, WhitewaterLifecycle, native_result};

unsafe extern "C" {
    fn manifold_fluids_oracle_curvature(
        phi: *const f32,
        isize: u32,
        jsize: u32,
        ksize: u32,
        dx: f64,
        surface_phi_out: *mut f32,
        curvature_out: *mut f32,
    ) -> i32;
    fn manifold_fluids_oracle_emit(
        lifecycle: *mut c_void,
        curvature: *const f32,
        positions: *const f32,
        count: usize,
        dt: f64,
    ) -> i32;
}

/// FLIP's own emitter on the lifecycle's last fields, then one update of
/// `dt`: the liquid particles at `positions` (scene metres) are its markers,
/// `curvature` (cell centres, x fastest) its curvature grid, with turbulence
/// emission and lifetime variance 0. The population then holds what FLIP
/// emitted, advanced, retyped and aged once.
pub fn emit(lifecycle: &mut WhitewaterLifecycle, curvature: &[f32], positions: &[[f32; 3]], dt: f64) -> Result<(), FluidError> {
    let cells = lifecycle.grid().cell_count();
    if curvature.len() != cells {
        return Err(FluidError::input(format!("oracle curvature holds {} values; the grid has {cells} cells", curvature.len())));
    }
    // SAFETY: the handle is live; `curvature` covers the grid's cells and
    // `positions` holds `len` triples.
    let ok = unsafe {
        manifold_fluids_oracle_emit(
            lifecycle.native_handle(),
            curvature.as_ptr(),
            positions.as_ptr().cast::<f32>(),
            positions.len(),
            dt,
        )
    };
    native_result(ok, "oracle emit")
}

/// FLIP's curvature of a cell-centred level set, x fastest.
#[derive(Clone, Debug)]
pub struct OracleCurvature {
    /// The field after FLIP's reinitialisation: the one its validity rule
    /// (|φ| < 2 dx at a node and its six neighbours, off the border) reads.
    pub surface_phi: Vec<f32>,
    /// Curvature at the valid nodes, extrapolated three layers past them.
    pub curvature: Vec<f32>,
}

/// `ParticleLevelSet::calculateCurvatureGrid` on `phi` over `cells` of size
/// `dx`.
pub fn curvature(phi: &[f32], cells: [u32; 3], dx: f64) -> Result<OracleCurvature, FluidError> {
    let count = cells
        .iter()
        .try_fold(1usize, |n, &c| n.checked_mul(c as usize))
        .ok_or_else(|| FluidError::input("oracle curvature grid is too large"))?;
    if phi.len() != count {
        return Err(FluidError::input(format!(
            "oracle curvature field holds {} values; a {cells:?} grid needs {count}",
            phi.len()
        )));
    }
    let mut surface_phi = vec![0.0; count];
    let mut curvature = vec![0.0; count];
    // SAFETY: every pointer covers `count` floats, the grid's size, checked above.
    let ok = unsafe {
        manifold_fluids_oracle_curvature(
            phi.as_ptr(),
            cells[0],
            cells[1],
            cells[2],
            dx,
            surface_phi.as_mut_ptr(),
            curvature.as_mut_ptr(),
        )
    };
    native_result(ok, "oracle curvature")?;
    Ok(OracleCurvature { surface_phi, curvature })
}

#[cfg(test)]
mod tests {
    use crate::{WhitewaterFields, WhitewaterGrid, WhitewaterKind, WhitewaterLifecycle};

    /// A flat surface rising at 5 m/s with its curvature past FLIP's
    /// maximum: every particle within 1.5 cells of it is a full wavecrest
    /// emitter at energy 0.207, so each emits (int)(175 · 0.207 / 60 + 0.5) = 1
    /// foam or spray particle of lifetime 7 · 0.207, and none emits from deep
    /// in the liquid.
    #[test]
    fn oracle_emit_spawns_at_a_rising_crest() {
        const N: u32 = 12;
        const H: f32 = 0.25;
        let grid = WhitewaterGrid { cells: [N; 3], cell_size: H, origin: [0.0; 3] };
        let n = N as usize;
        let level: Vec<f32> = (0..n * n * n).map(|i| ((i / n) % n) as f32 * H + 0.5 * H - 1.5).collect();
        let solid = vec![10.0f32; (n + 1).pow(3)];
        let u = vec![0.0f32; (n + 1) * n * n];
        let v = vec![5.0f32; n * (n + 1) * n];
        let w = vec![0.0f32; n * n * (n + 1)];
        let fields = WhitewaterFields {
            face_u: &u,
            face_v: &v,
            face_w: &w,
            face_cells: [N; 3],
            face_offset: [0; 3],
            level: &level,
            solid: &solid,
            gravity: [0.0, -9.81, 0.0],
        };
        let mut lifecycle = WhitewaterLifecycle::new(grid, 100_000, 7).expect("lifecycle");
        lifecycle.set_fields(&fields).expect("fields");
        let surface: Vec<[f32; 3]> =
            (0..8).flat_map(|x| (0..8).map(move |z| [1.0 + 0.125 * x as f32, 1.4, 1.0 + 0.125 * z as f32])).collect();
        let deep: Vec<[f32; 3]> = (0..8).map(|x| [1.0 + 0.125 * x as f32, 0.5, 1.5]).collect();
        let positions: Vec<[f32; 3]> = surface.iter().chain(&deep).copied().collect();
        let curvature = vec![3.0 / H; n * n * n];
        super::emit(&mut lifecycle, &curvature, &positions, 1.0 / 60.0).expect("oracle emit");
        let mut out = Vec::new();
        lifecycle.particles(&mut out).expect("particles");
        assert!((surface.len() / 2..=surface.len()).contains(&out.len()), "{} of {} surface emitters emitted", out.len(), surface.len());
        let energy = (0.5 * 25.0 - 0.1) / 59.9;
        for p in &out {
            assert!(p.position[1] > 1.0, "{p:?} emitted from deep in the liquid");
            assert!(p.lifetime > 7.0 * energy - 0.1 && p.lifetime <= 7.0 * energy, "{p:?}");
            assert!(p.kind != WhitewaterKind::Bubble, "{p:?}");
        }
    }

    /// A plane has zero curvature everywhere FLIP computes it, and FLIP's
    /// reinitialisation keeps an exact plane's distances.
    #[test]
    fn oracle_curvature_of_a_plane_is_zero() {
        let cells = [12u32, 11, 10];
        let dx = 0.5;
        let phi: Vec<f32> = (0..cells.iter().product::<u32>() as usize)
            .map(|index| {
                let j = (index / cells[0] as usize) % cells[1] as usize;
                ((j as f64 + 0.5) * dx - 2.6) as f32
            })
            .collect();
        let oracle = super::curvature(&phi, cells, dx).expect("oracle");
        assert!(oracle.curvature.iter().all(|k| k.abs() < 1e-4), "a plane is flat");
        let near: Vec<usize> = (0..phi.len()).filter(|&i| phi[i].abs() < 2.0 * dx as f32).collect();
        assert!(!near.is_empty());
        for i in near {
            assert!((oracle.surface_phi[i] - phi[i]).abs() < 1e-4, "node {i}: {} against {}", oracle.surface_phi[i], phi[i]);
        }
        assert!(super::curvature(&phi[1..], cells, dx).is_err(), "a short field is refused");
    }
}
