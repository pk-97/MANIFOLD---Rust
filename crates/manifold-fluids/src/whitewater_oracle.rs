//! FLIP's own whitewater fields as a test oracle
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.7, O1). Built only with the
//! `whitewater-oracle` feature; nothing in the product calls it.

use crate::{FluidError, native_result};

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
