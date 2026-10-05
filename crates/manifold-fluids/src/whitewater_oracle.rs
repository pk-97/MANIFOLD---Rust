//! Checked against FLIP Fluids diffuseparticlesimulation.cpp and particlelevelset.cpp (MIT); see THIRD_PARTY_NOTICES.md.
//! FLIP's own whitewater fields and emitter as test oracles
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.7, O1 and O2). Built only with
//! the `whitewater-oracle` feature; nothing in the product calls them.

use std::ffi::c_void;

use crate::{FluidError, WhitewaterLifecycle, native_result};

unsafe extern "C" {
    fn manifold_fluids_oracle_turbulence(lifecycle: *mut c_void, values: *mut f32,
        positions: *const f32, count: usize, samples: *mut f32) -> i32;
    fn manifold_fluids_oracle_emission_options(lifecycle: *mut c_void,
        wavecrest: f64, turbulence: f64, minimum: f64, maximum: f64,
        generation: f64, speed: f64, influence: f64) -> i32;
    fn manifold_fluids_oracle_emit_configured(lifecycle: *mut c_void,
        curvature: *const f32, positions: *const f32, count: usize, dt: f64) -> i32;
    fn manifold_fluids_oracle_emit_engine(lifecycle: *mut c_void, positions: *const f32, count: usize,
        dt: f64, influence_base: f64, influence_decay: f64, surface_out: *mut f32,
        curvature_out: *mut f32, influence_out: *mut f32) -> i32;
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

/// Test controls for the vendored emitter; defaults from diffuseparticlesimulation.h.
#[derive(Clone, Copy, Debug)]
pub struct EmissionOptions {
    pub wavecrest: f64,
    pub turbulence: f64,
    pub minimum: f64,
    pub maximum: f64,
    pub generation: f64,
    pub speed: f64,
    pub influence: f64,
}

impl Default for EmissionOptions {
    fn default() -> Self {
        Self { wavecrest: 175.0, turbulence: 175.0, minimum: 100.0, maximum: 200.0,
            generation: 1.0, speed: 1.0, influence: 1.0 }
    }
}

/// The vendored turbulence lattice and its trilinear samples on the last fields.
pub fn turbulence(lifecycle: &mut WhitewaterLifecycle, positions: &[[f32; 3]]) -> Result<(Vec<f32>, Vec<f32>), FluidError> {
    let mut values = vec![0.0; lifecycle.grid().cell_count()];
    let mut samples = vec![0.0; positions.len()];
    // SAFETY: live handle, correctly sized lattice and matching sample slices.
    let ok = unsafe { manifold_fluids_oracle_turbulence(lifecycle.native_handle(), values.as_mut_ptr(),
        positions.as_ptr().cast(), positions.len(), samples.as_mut_ptr()) };
    native_result(ok, "oracle turbulence")?;
    Ok((values, samples))
}

/// Run the engine emitter with explicit controls, then its lifecycle.
pub fn emit_configured(lifecycle: &mut WhitewaterLifecycle, curvature: &[f32], positions: &[[f32; 3]],
    dt: f64, options: EmissionOptions) -> Result<(), FluidError> {
    if curvature.len() != lifecycle.grid().cell_count() {
        return Err(FluidError::input("oracle curvature length differs from grid"));
    }
    if ![options.wavecrest, options.turbulence, options.minimum, options.maximum,
        options.generation, options.speed, options.influence].iter().all(|x| x.is_finite() && *x >= 0.0)
        || options.maximum <= options.minimum || options.generation > 1.0 || options.speed < 1.0 {
        return Err(FluidError::input("invalid oracle emission controls"));
    }
    // SAFETY: live handle; scalar options are consumed synchronously.
    let ok = unsafe { manifold_fluids_oracle_emission_options(lifecycle.native_handle(),
        options.wavecrest, options.turbulence, options.minimum, options.maximum,
        options.generation, options.speed, options.influence) };
    native_result(ok, "oracle emission options")?;
    // SAFETY: curvature covers the grid; positions is a slice of triples.
    let ok = unsafe { manifold_fluids_oracle_emit_configured(lifecycle.native_handle(),
        curvature.as_ptr(), positions.as_ptr().cast(), positions.len(), dt) };
    native_result(ok, "oracle configured emit")
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

/// The fields FLIP's emitter read on one engine-as-configured tick.
#[derive(Clone, Debug)]
pub struct EngineEmitFields {
    /// `calculateCurvatureGrid`'s reinitialised surface distance, cells.
    pub surface: Vec<f32>,
    /// Its curvature, cells.
    pub curvature: Vec<f32>,
    /// The obstacle influence after this tick's update, nodes.
    pub influence: Vec<f32>,
}

/// One tick of FLIP's emitter and lifecycle as `FluidSimulation` configures
/// them, on the lifecycle's last fields: the engine's lifetime variance, an
/// `InfluenceGrid` updated on the solid every tick with `influence_base` and
/// `influence_decay`, and the surface distance and curvature from
/// `calculateCurvatureGrid` on the level set. `positions` (scene metres) are
/// the markers. Rates come from [`set_emission_rates`] or the engine
/// defaults. The solid carries no mesh objects, so no obstacle raises
/// influence above its base.
pub fn emit_engine(lifecycle: &mut WhitewaterLifecycle, positions: &[[f32; 3]], dt: f64,
    influence_base: f64, influence_decay: f64) -> Result<EngineEmitFields, FluidError> {
    let cells = lifecycle.grid().cell_count();
    let c = lifecycle.grid().cells.map(|n| n as usize + 1);
    let mut fields = EngineEmitFields {
        surface: vec![0.0; cells],
        curvature: vec![0.0; cells],
        influence: vec![0.0; c[0] * c[1] * c[2]],
    };
    // SAFETY: live handle; outputs cover the grid's cells and nodes, positions holds `len` triples.
    let ok = unsafe { manifold_fluids_oracle_emit_engine(lifecycle.native_handle(), positions.as_ptr().cast(),
        positions.len(), dt, influence_base, influence_decay, fields.surface.as_mut_ptr(),
        fields.curvature.as_mut_ptr(), fields.influence.as_mut_ptr()) };
    native_result(ok, "oracle engine emit")?;
    Ok(fields)
}

/// The emitter's rates for [`emit_engine`]; `options.influence` is unused there.
pub fn set_emission_rates(lifecycle: &mut WhitewaterLifecycle, options: EmissionOptions) -> Result<(), FluidError> {
    // SAFETY: live handle; scalar options are consumed synchronously.
    let ok = unsafe { manifold_fluids_oracle_emission_options(lifecycle.native_handle(),
        options.wavecrest, options.turbulence, options.minimum, options.maximum,
        options.generation, options.speed, options.influence) };
    native_result(ok, "oracle emission options")
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

    /// The engine-as-configured variant on a sheared, rising slab: it emits
    /// all three types, its lifetimes carry the engine's variance, its
    /// influence relaxes at the decay rate rather than jumping to the base,
    /// and its surface is calculateCurvatureGrid's, not the raw level set.
    #[test]
    fn oracle_engine_emit_runs_as_the_engine_configures_it() {
        const N: u32 = 16;
        const H: f32 = 0.25;
        const DT: f64 = 1.0 / 60.0;
        let n = N as usize;
        let grid = WhitewaterGrid { cells: [N; 3], cell_size: H, origin: [0.0; 3] };
        // Liquid below y = 2.6, a dome on top so the crest curves.
        let level: Vec<f32> = (0..n * n * n).map(|i| {
            let (x, y, z) = (i % n, (i / n) % n, i / (n * n));
            let (px, py, pz) = ((x as f32 + 0.5) * H, (y as f32 + 0.5) * H, (z as f32 + 0.5) * H);
            let r = ((px - 2.0).powi(2) + (pz - 2.0).powi(2)).sqrt();
            py - (2.6 + 0.6 * (-r * r).exp())
        }).collect();
        // A floor a cell down: solid nodes inside the influence band, which
        // the engine variant must take without mesh objects.
        let solid: Vec<f32> = (0..(n + 1).pow(3)).map(|i| ((i / (n + 1)) % (n + 1)) as f32 * H - H).collect();
        // A hash-noise shear so the liquid holds turbulence for bubbles.
        let noise = |i: usize, salt: usize| (((i * 2_654_435_761 + salt * 40_503) % 1000) as f32 / 500.0) - 1.0;
        let u: Vec<f32> = (0..(n + 1) * n * n).map(|i| 30.0 * noise(i, 1)).collect();
        let v: Vec<f32> = (0..n * (n + 1) * n).map(|i| 6.0 + 30.0 * noise(i, 2)).collect();
        let w: Vec<f32> = (0..n * n * (n + 1)).map(|i| 30.0 * noise(i, 3)).collect();
        let fields = WhitewaterFields {
            face_u: &u, face_v: &v, face_w: &w, face_cells: [N; 3], face_offset: [0; 3],
            level: &level, solid: &solid, gravity: [0.0, -9.81, 0.0],
        };
        let mut lifecycle = WhitewaterLifecycle::new(grid, 200_000, 11).expect("lifecycle");
        lifecycle.set_fields(&fields).expect("fields");
        // Eight markers a cell in the liquid, as FLIP seeds them.
        let mut markers = Vec::new();
        for (i, &phi) in level.iter().enumerate() {
            if phi >= 0.0 { continue; }
            let (x, y, z) = (i % n, (i / n) % n, i / (n * n));
            for s in 0..8 {
                let o = [(s & 1) as f32, ((s >> 1) & 1) as f32, ((s >> 2) & 1) as f32].map(|b| (0.25 + 0.5 * b) * H);
                markers.push([x as f32 * H + o[0], y as f32 * H + o[1], z as f32 * H + o[2]]);
            }
        }
        super::set_emission_rates(&mut lifecycle, super::EmissionOptions::default()).expect("rates");
        let first = super::emit_engine(&mut lifecycle, &markers, DT, 1.0, 2.0).expect("first tick");
        let mut out = Vec::new();
        lifecycle.particles(&mut out).expect("particles");
        let kinds = [WhitewaterKind::Foam, WhitewaterKind::Bubble, WhitewaterKind::Spray]
            .map(|k| out.iter().filter(|p| p.kind == k).count());
        println!("engine oracle tick 1: foam/bubble/spray {kinds:?} of {} markers", markers.len());
        assert!(kinds.iter().all(|&c| c > 0), "every type must emit: {kinds:?}");

        // (a) Without variance a lifetime is 7 times an energy of at most 1,
        // so only the engine's variance of 3 lifts one past 7.
        let hi = out.iter().map(|p| p.lifetime).fold(f32::MIN, f32::max);
        assert!(hi > 7.0, "longest lifetime {hi}: no variance");

        // (c) The surface is calculateCurvatureGrid's on the level set.
        let reference = super::curvature(&level, [N; 3], f64::from(H)).expect("reference curvature");
        assert_eq!(first.surface, reference.surface_phi, "surface is not calculateCurvatureGrid's");
        assert_eq!(first.curvature, reference.curvature, "curvature is not calculateCurvatureGrid's");
        assert!(first.surface.iter().zip(&level).any(|(a, b)| (a - b).abs() > 1e-3), "surface is the raw level set");

        // (b) Influence starts at base 1; with the base lowered to 0.5 it
        // relaxes by decay · dt a tick, where a constant fill would jump.
        assert!(first.influence.iter().all(|&x| (x - 1.0).abs() < 1e-6), "first tick influence is the base");
        let second = super::emit_engine(&mut lifecycle, &markers, DT, 0.5, 2.0).expect("second tick");
        let expected = 1.0 - 2.0 * DT as f32;
        assert!(second.influence.iter().all(|&x| (x - expected).abs() < 1e-5), "influence did not decay at 2/s: {:?}", &second.influence[..4]);
    }
}
