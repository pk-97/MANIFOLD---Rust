//! FLIP's whitewater lifecycle, fed from outside (`docs/GPU_WHITEWATER_DESIGN.md`
//! D1, section 3.4): the vendored `DiffuseParticleSimulation` with emission
//! off advances spawns it is handed over fields it is handed. Advection,
//! buoyancy, drag, collisions, types, lifetimes and removal are FLIP's own.

use std::cell::Cell;
use std::ffi::c_void;
use std::marker::PhantomData;

use crate::{FluidError, NativeWhitewaterParticle, WhitewaterParticle, decode_whitewater, native_result};

/// One whitewater particle to hand the lifecycle. Shared with the GPU spawn
/// atoms, which write it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WhitewaterSpawn {
    /// Scene metres; w is lifetime in seconds, ≤ 0 marks an empty slot.
    pub position_lifetime: [f32; 4],
    /// m/s.
    pub velocity: [f32; 3],
    /// FLIP's DiffuseParticleType: 0 bubble, 1 foam, 2 spray.
    pub kind: u32,
}

const _: () = assert!(std::mem::size_of::<WhitewaterSpawn>() == 32);

/// The grid the lifecycle runs on: `cells` of `cell_size` from `origin`, scene
/// metres. Its nodes are the solid lattice's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WhitewaterGrid {
    pub cells: [u32; 3],
    pub cell_size: f32,
    pub origin: [f32; 3],
}

impl WhitewaterGrid {
    pub fn cell_count(&self) -> usize {
        self.cells.iter().map(|&n| n as usize).product()
    }

    pub fn node_count(&self) -> usize {
        self.cells.iter().map(|&n| n as usize + 1).product()
    }
}

/// One tick's fields, borrowed for the copy into the engine.
pub struct WhitewaterFields<'a> {
    /// Seam layout (`LIQUID_SOLVER_SEAM_DESIGN.md` section 3.2 (Grid outputs)):
    /// MAC faces over `face_cells`, x fastest, m/s.
    pub face_u: &'a [f32],
    pub face_v: &'a [f32],
    pub face_w: &'a [f32],
    pub face_cells: [u32; 3],
    /// Where the face cells start, in grid cells.
    pub face_offset: [u32; 3],
    /// The liquid's signed distance at the cell centres, metres, negative in the liquid.
    pub level: &'a [f32],
    /// The solid's signed distance at the grid nodes, metres, negative in the solid.
    pub solid: &'a [f32],
    /// m/s².
    pub gravity: [f32; 3],
}

unsafe extern "C" {
    fn manifold_fluids_whitewater_create(
        isize: u32,
        jsize: u32,
        ksize: u32,
        cell_size: f64,
        origin: *const f32,
        capacity: u32,
        seed: u64,
        lifecycle_out: *mut *mut c_void,
    ) -> i32;
    fn manifold_fluids_whitewater_destroy(lifecycle: *mut c_void);
    fn manifold_fluids_whitewater_clear(lifecycle: *mut c_void, seed: u64) -> i32;
    fn manifold_fluids_whitewater_set_fields(
        lifecycle: *mut c_void,
        face_u: *const f32,
        face_v: *const f32,
        face_w: *const f32,
        face_cells: *const u32,
        face_offset: *const u32,
        level: *const f32,
        solid: *const f32,
        gravity: *const f32,
    ) -> i32;
    fn manifold_fluids_whitewater_load(
        lifecycle: *mut c_void,
        spawns: *const WhitewaterSpawn,
        count: usize,
        loaded_out: *mut u32,
        thinned_out: *mut u32,
    ) -> i32;
    fn manifold_fluids_whitewater_step(lifecycle: *mut c_void, dt: f64) -> i32;
    fn manifold_fluids_whitewater_count(lifecycle: *mut c_void, count_out: *mut usize) -> i32;
    fn manifold_fluids_whitewater_particles(
        lifecycle: *mut c_void,
        particles: *mut NativeWhitewaterParticle,
        capacity: usize,
        count_out: *mut usize,
    ) -> i32;
}

/// Faces of `axis` over `cells`: one more along `axis`.
fn face_len(cells: [u32; 3], axis: usize) -> usize {
    (0..3).map(|b| cells[b] as usize + usize::from(b == axis)).product()
}

/// Owns one native `DiffuseParticleSimulation` and the grids it reads.
pub struct WhitewaterLifecycle {
    native: *mut c_void,
    grid: WhitewaterGrid,
    capacity: u32,
    scratch: Vec<NativeWhitewaterParticle>,
    // Cell is Send but not Sync, matching exclusive ownership.
    _not_sync: PhantomData<Cell<()>>,
}

// SAFETY: the pointer owns one lifecycle, every access takes &mut self, and
// the bridge serializes the engine's process-global state.
unsafe impl Send for WhitewaterLifecycle {}

impl WhitewaterLifecycle {
    /// A lifecycle holding at most `capacity` particles, its RNG seeded with `seed`.
    pub fn new(grid: WhitewaterGrid, capacity: u32, seed: u64) -> Result<Self, FluidError> {
        if !(grid.cell_size.is_finite() && grid.cell_size > 0.0) {
            return Err(FluidError::input("whitewater cell size must be finite and positive"));
        }
        let mut native = std::ptr::null_mut();
        // SAFETY: `origin` is three floats; the out pointer is valid.
        let ok = unsafe {
            manifold_fluids_whitewater_create(
                grid.cells[0],
                grid.cells[1],
                grid.cells[2],
                f64::from(grid.cell_size),
                grid.origin.as_ptr(),
                capacity,
                seed,
                &mut native,
            )
        };
        native_result(ok, "creating the whitewater lifecycle")?;
        Ok(Self { native, grid, capacity, scratch: Vec::new(), _not_sync: PhantomData })
    }

    pub fn grid(&self) -> WhitewaterGrid {
        self.grid
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// The native handle, for the FLIP oracle's entries.
    #[cfg(feature = "whitewater-oracle")]
    pub(crate) fn native_handle(&mut self) -> *mut c_void {
        self.native
    }

    /// Drops every particle and reseeds the RNG.
    pub fn clear(&mut self, seed: u64) -> Result<(), FluidError> {
        // SAFETY: `native` is this lifecycle's live handle.
        let ok = unsafe { manifold_fluids_whitewater_clear(self.native, seed) };
        native_result(ok, "clearing the whitewater lifecycle")
    }

    /// Copies one tick's fields into the engine. Every length is checked
    /// against the grid first.
    pub fn set_fields(&mut self, fields: &WhitewaterFields<'_>) -> Result<(), FluidError> {
        let cells = self.grid.cells;
        if (0..3).any(|a| fields.face_cells[a] == 0 || u64::from(fields.face_cells[a]) + u64::from(fields.face_offset[a]) > u64::from(cells[a])) {
            return Err(FluidError::input(format!(
                "a {:?}-cell face grid at {:?} does not sit inside the {cells:?}-cell whitewater grid",
                fields.face_cells, fields.face_offset
            )));
        }
        for (axis, faces) in [fields.face_u, fields.face_v, fields.face_w].into_iter().enumerate() {
            let wanted = face_len(fields.face_cells, axis);
            if faces.len() != wanted {
                return Err(FluidError::input(format!(
                    "whitewater face axis {axis} holds {} values; {:?} cells need {wanted}",
                    faces.len(),
                    fields.face_cells
                )));
            }
        }
        if fields.level.len() != self.grid.cell_count() {
            return Err(FluidError::input(format!(
                "whitewater level holds {} values; {cells:?} cells need {}",
                fields.level.len(),
                self.grid.cell_count()
            )));
        }
        if fields.solid.len() != self.grid.node_count() {
            return Err(FluidError::input(format!(
                "whitewater solid holds {} values; {cells:?} cells need {} nodes",
                fields.solid.len(),
                self.grid.node_count()
            )));
        }
        // SAFETY: every slice holds the length the grid needs, checked above.
        let ok = unsafe {
            manifold_fluids_whitewater_set_fields(
                self.native,
                fields.face_u.as_ptr(),
                fields.face_v.as_ptr(),
                fields.face_w.as_ptr(),
                fields.face_cells.as_ptr(),
                fields.face_offset.as_ptr(),
                fields.level.as_ptr(),
                fields.solid.as_ptr(),
                fields.gravity.as_ptr(),
            )
        };
        native_result(ok, "setting the whitewater fields")
    }

    /// Loads the records with lifetime > 0 up to capacity − live, a uniform
    /// stride subset past that (D8). Returns (loaded, thinned).
    pub fn load(&mut self, spawns: &[WhitewaterSpawn]) -> Result<(u32, u32), FluidError> {
        let (mut loaded, mut thinned) = (0u32, 0u32);
        // SAFETY: `spawns` holds `len` records laid out as the bridge's.
        let ok = unsafe {
            manifold_fluids_whitewater_load(self.native, spawns.as_ptr(), spawns.len(), &mut loaded, &mut thinned)
        };
        native_result(ok, "loading whitewater spawns")?;
        Ok((loaded, thinned))
    }

    /// One FLIP whitewater update of `dt` seconds on the last fields.
    pub fn step(&mut self, dt: f64) -> Result<(), FluidError> {
        // SAFETY: `native` is this lifecycle's live handle.
        let ok = unsafe { manifold_fluids_whitewater_step(self.native, dt) };
        native_result(ok, "stepping the whitewater lifecycle")
    }

    /// Live particles.
    pub fn len(&mut self) -> Result<usize, FluidError> {
        let mut count = 0usize;
        // SAFETY: `native` is this lifecycle's live handle.
        let ok = unsafe { manifold_fluids_whitewater_count(self.native, &mut count) };
        native_result(ok, "counting whitewater particles")?;
        Ok(count)
    }

    pub fn is_empty(&mut self) -> Result<bool, FluidError> {
        Ok(self.len()? == 0)
    }

    /// The population in scene space, replacing `out`.
    pub fn particles(&mut self, out: &mut Vec<WhitewaterParticle>) -> Result<(), FluidError> {
        let count = self.len()?;
        self.scratch.resize(count, NativeWhitewaterParticle::default());
        let mut copied = 0usize;
        // SAFETY: `scratch` holds `count` records.
        let ok = unsafe {
            manifold_fluids_whitewater_particles(self.native, self.scratch.as_mut_ptr(), self.scratch.len(), &mut copied)
        };
        native_result(ok, "reading whitewater particles")?;
        decode_whitewater(&self.scratch[..copied], out)
    }
}

impl Drop for WhitewaterLifecycle {
    fn drop(&mut self) {
        if !self.native.is_null() {
            // SAFETY: the handle is live and dropped once.
            unsafe { manifold_fluids_whitewater_destroy(self.native) };
            self.native = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WhitewaterKind;

    const H: f32 = 0.0625;
    const N: u32 = 24;
    /// The face grid sits 3 cells in, as the seam's does on the whitewater grid.
    const PAD: u32 = 3;
    const DT: f64 = 1.0 / 60.0;
    const G: f32 = -9.81;
    const ORIGIN: [f32; 3] = [-0.75, -0.1875, -0.75];
    /// A solid floor 6 cells up, 3 cells inside FLIP's boundary box.
    const FLOOR: f32 = ORIGIN[1] + 6.0 * H;

    fn grid() -> WhitewaterGrid {
        WhitewaterGrid { cells: [N; 3], cell_size: H, origin: ORIGIN }
    }

    /// Scene height of cell row `j`'s centre.
    fn cell_y(j: u32) -> f32 {
        ORIGIN[1] + (j as f32 + 0.5) * H
    }

    struct Scene {
        faces: [Vec<f32>; 3],
        level: Vec<f32>,
        solid: Vec<f32>,
    }

    impl Scene {
        /// Liquid below `surface` (scene y; above everything when None is
        /// passed as all air), the faces still, the floor solid.
        fn new(level: impl Fn(f32) -> f32) -> Self {
            let face_cells = [N - 2 * PAD; 3];
            let faces = std::array::from_fn(|axis| vec![0.0; face_len(face_cells, axis)]);
            let cells = N as usize;
            let level = (0..cells.pow(3)).map(|i| level(cell_y(((i / cells) % cells) as u32))).collect();
            let nodes = cells + 1;
            let solid = (0..nodes.pow(3)).map(|i| ORIGIN[1] + ((i / nodes) % nodes) as f32 * H - FLOOR).collect();
            Self { faces, level, solid }
        }

        fn fields(&self) -> WhitewaterFields<'_> {
            WhitewaterFields {
                face_u: &self.faces[0],
                face_v: &self.faces[1],
                face_w: &self.faces[2],
                face_cells: [N - 2 * PAD; 3],
                face_offset: [PAD; 3],
                level: &self.level,
                solid: &self.solid,
                gravity: [0.0, G, 0.0],
            }
        }
    }

    fn spawn(position: [f32; 3], velocity: [f32; 3], lifetime: f32, kind: WhitewaterKind) -> WhitewaterSpawn {
        WhitewaterSpawn {
            position_lifetime: [position[0], position[1], position[2], lifetime],
            velocity,
            kind: kind as u32,
        }
    }

    fn lifecycle(scene: &Scene, spawns: &[WhitewaterSpawn]) -> WhitewaterLifecycle {
        let mut lifecycle = WhitewaterLifecycle::new(grid(), 1000, 7).expect("lifecycle");
        lifecycle.set_fields(&scene.fields()).expect("fields");
        assert_eq!(lifecycle.load(spawns).expect("load"), (spawns.len() as u32, 0));
        lifecycle
    }

    fn population(lifecycle: &mut WhitewaterLifecycle) -> Vec<WhitewaterParticle> {
        let mut out = Vec::new();
        lifecycle.particles(&mut out).expect("particles");
        out
    }

    fn close(a: f32, b: f32, tolerance: f32) -> bool {
        (a - b).abs() <= tolerance
    }

    /// Spray falls under gravity alone (no drag at FLIP's defaults) and
    /// rebounds off the solid floor at restitution 0.2. The rebounding one
    /// starts just above FLIP's quarter-cell buffer, so its resolved step is
    /// short enough that FLIP's speed check keeps it.
    #[test]
    fn whitewater_lifecycle_spray_falls_and_rebounds() {
        let scene = Scene::new(|_| 1.0);
        let high = [0.0, FLOOR + 8.0 * H, 0.0];
        let low = [0.1, FLOOR + 0.25 * H + 0.001, 0.1];
        let mut lifecycle = lifecycle(
            &scene,
            &[
                spawn(high, [0.0; 3], 5.0, WhitewaterKind::Spray),
                spawn(low, [0.0, -2.0, 0.0], 5.0, WhitewaterKind::Spray),
            ],
        );
        lifecycle.step(DT).expect("step");
        let after = population(&mut lifecycle);
        assert_eq!(after.len(), 2);
        let rebound = after.iter().find(|p| p.position[0] > 0.05).expect("rebounding spray");
        let expected = 0.2 * 2.0 + G * DT as f32;
        assert!(close(rebound.velocity[1], expected, 1e-5), "rebound speed {} against {expected}", rebound.velocity[1]);
        assert!(close(rebound.position[1], FLOOR + 0.25 * H, 1e-5), "rebound rests a quarter cell up: {}", rebound.position[1]);
        assert_eq!(rebound.kind, WhitewaterKind::Spray);
        for _ in 1..5 {
            lifecycle.step(DT).expect("step");
        }
        let falling = population(&mut lifecycle).into_iter().find(|p| p.position[0].abs() < 1e-6).expect("falling spray");
        let t = DT as f32;
        assert!(close(falling.velocity[1], 5.0 * G * t, 1e-4), "{}", falling.velocity[1]);
        assert!(close(falling.position[1], high[1] + G * t * t * 15.0, 1e-5), "{}", falling.position[1]);
    }

    /// A bubble in still liquid rises: buoyancy 4 g, drag 1 toward the
    /// still faces, so each step's velocity is 4 g dt up.
    #[test]
    fn whitewater_lifecycle_bubble_rises() {
        let scene = Scene::new(|_| -1.0);
        let start = [0.0, FLOOR + 6.0 * H, 0.0];
        let mut lifecycle = lifecycle(&scene, &[spawn(start, [0.0; 3], 5.0, WhitewaterKind::Bubble)]);
        let t = DT as f32;
        for step in 1..=3 {
            lifecycle.step(DT).expect("step");
            let bubble = population(&mut lifecycle)[0];
            assert_eq!(bubble.kind, WhitewaterKind::Bubble);
            assert!(close(bubble.velocity[1], -4.0 * G * t, 1e-4), "{}", bubble.velocity[1]);
            assert!(close(bubble.position[1], start[1] - 4.0 * G * t * t * step as f32, 1e-5), "{}", bubble.position[1]);
        }
    }

    /// Foam on the surface moves with the faces, sampled by FLIP's MAC
    /// trilinear at the seam's offset: a shear u = a·(y − face grid min y).
    #[test]
    fn whitewater_lifecycle_foam_follows_faces() {
        let surface = cell_y(12);
        let mut scene = Scene::new(|y| y - surface);
        let face_cells = [N - 2 * PAD; 3];
        let a = 4.0;
        let dims = [face_cells[0] as usize + 1, face_cells[1] as usize, face_cells[2] as usize];
        for (index, u) in scene.faces[0].iter_mut().enumerate() {
            let j = (index / dims[0]) % dims[1];
            *u = a * (j as f32 + 0.5) * H;
        }
        let start = [-0.2, surface, 0.05];
        let mut lifecycle = lifecycle(&scene, &[spawn(start, [0.0; 3], 5.0, WhitewaterKind::Foam)]);
        let speed = a * (surface - (ORIGIN[1] + PAD as f32 * H));
        let t = DT as f32;
        for step in 1..=3 {
            lifecycle.step(DT).expect("step");
            let foam = population(&mut lifecycle)[0];
            assert_eq!(foam.kind, WhitewaterKind::Foam);
            assert!(close(foam.velocity[0], speed, 1e-4), "{} against {speed}", foam.velocity[0]);
            assert!(close(foam.velocity[1], 0.0, 1e-6) && close(foam.velocity[2], 0.0, 1e-6));
            assert!(close(foam.position[0], start[0] + speed * t * step as f32, 1e-5), "{}", foam.position[0]);
            assert!(close(foam.position[1], surface, 1e-6));
        }
    }

    /// Lifetimes fall per second by 2 for spray, 0.333 for bubbles and 1 for
    /// foam; a particle whose lifetime runs out is removed.
    #[test]
    fn whitewater_lifecycle_lifetimes_fall_by_type() {
        let surface = cell_y(12);
        let scene = Scene::new(|y| y - surface);
        let mut lifecycle = lifecycle(
            &scene,
            &[
                spawn([0.0, surface + 6.0 * H, 0.0], [0.0; 3], 3.0, WhitewaterKind::Spray),
                spawn([0.2, surface - 4.0 * H, 0.0], [0.0; 3], 3.0, WhitewaterKind::Bubble),
                spawn([-0.2, surface, 0.0], [0.0; 3], 3.0, WhitewaterKind::Foam),
                spawn([0.0, surface + 6.0 * H, 0.2], [0.0; 3], 0.02, WhitewaterKind::Spray),
            ],
        );
        lifecycle.step(DT).expect("step");
        let after = population(&mut lifecycle);
        assert_eq!(after.len(), 3, "the short-lived spray is removed: {after:?}");
        let t = DT as f32;
        for (kind, rate) in [(WhitewaterKind::Spray, 2.0), (WhitewaterKind::Bubble, 0.333), (WhitewaterKind::Foam, 1.0)] {
            let particle = after.iter().find(|p| p.kind == kind).unwrap_or_else(|| panic!("{kind:?} kept"));
            assert!(close(particle.lifetime, 3.0 - rate * t, 1e-6), "{kind:?}: {}", particle.lifetime);
        }
    }

    /// I11: FLIP's load leaves the particle system's cached size at 0 and its
    /// update then returns early; the glue refreshes it, so the count is right
    /// straight after a load and the first step moves every kind.
    #[test]
    fn whitewater_lifecycle_advances_loaded_spawns() {
        let surface = cell_y(12);
        let scene = Scene::new(|y| y - surface);
        let spawns = [
            spawn([0.0, surface + 6.0 * H, 0.0], [0.0; 3], 3.0, WhitewaterKind::Spray),
            spawn([0.2, surface - 4.0 * H, 0.0], [0.0; 3], 3.0, WhitewaterKind::Bubble),
            spawn([-0.2, surface, 0.0], [0.5, 0.0, 0.0], 3.0, WhitewaterKind::Foam),
        ];
        let mut lifecycle = lifecycle(&scene, &spawns);
        assert_eq!(lifecycle.len().expect("count"), 3, "loaded spawns are counted at once");
        let before = population(&mut lifecycle);
        lifecycle.step(DT).expect("step");
        let after = population(&mut lifecycle);
        assert_eq!(after.len(), 3);
        for (b, a) in before.iter().zip(&after) {
            assert_ne!(b.lifetime, a.lifetime, "{:?} aged", b.kind);
        }
        assert!(after[0].position[1] < before[0].position[1], "spray fell");
        assert!(after[1].position[1] > before[1].position[1], "bubble rose");
        assert!(after[2].velocity[0].abs() < 1e-6, "foam took the still faces' velocity");
    }

    /// I10: past the room left, a load keeps a uniform stride of the live
    /// records and reports the rest as thinned; empty slots are not counted.
    #[test]
    fn whitewater_capacity_thins_and_reports() {
        let scene = Scene::new(|_| 1.0);
        let mut lifecycle = WhitewaterLifecycle::new(grid(), 10, 7).expect("lifecycle");
        lifecycle.set_fields(&scene.fields()).expect("fields");
        let x = |i: usize| -0.5 + 0.03 * i as f32;
        let mut spawns: Vec<WhitewaterSpawn> =
            (0..25).map(|i| spawn([x(i), FLOOR + 8.0 * H, 0.0], [0.0; 3], 5.0, WhitewaterKind::Spray)).collect();
        for gap in [3, 9, 14, 20, 26] {
            spawns.insert(gap, WhitewaterSpawn::default());
        }
        assert_eq!(lifecycle.load(&spawns).expect("load"), (10, 15));
        let mut kept: Vec<f32> = population(&mut lifecycle).iter().map(|p| p.position[0]).collect();
        kept.sort_by(f32::total_cmp);
        let expected: Vec<f32> = (0..10).map(|j| x(j * 25 / 10)).collect();
        let same = kept.len() == expected.len() && kept.iter().zip(&expected).all(|(&k, &e)| close(k, e, 1e-6));
        assert!(same, "live record floor(j * 25 / 10) for slot j: kept {kept:?}, expected {expected:?}");
        assert_eq!(lifecycle.load(&spawns[..5]).expect("load"), (0, 4), "no room left: every live record is thinned");
        lifecycle.clear(8).expect("clear");
        assert!(lifecycle.is_empty().expect("count"));
        assert_eq!(lifecycle.load(&spawns[..5]).expect("load"), (4, 0), "clear makes room");
    }

    /// Fields that don't fit the grid are refused by name before any copy.
    #[test]
    fn whitewater_lifecycle_refuses_misfit_fields() {
        let scene = Scene::new(|_| 1.0);
        let mut lifecycle = WhitewaterLifecycle::new(grid(), 10, 7).expect("lifecycle");
        let mut fields = scene.fields();
        fields.face_offset = [PAD + 4, PAD, PAD];
        assert!(lifecycle.set_fields(&fields).unwrap_err().to_string().contains("does not sit inside"));
        let mut fields = scene.fields();
        fields.level = &scene.level[1..];
        assert!(lifecycle.set_fields(&fields).unwrap_err().to_string().contains("whitewater level holds"));
        let mut fields = scene.fields();
        fields.face_v = &scene.faces[1][1..];
        assert!(lifecycle.set_fields(&fields).unwrap_err().to_string().contains("face axis 1"));
        assert!(lifecycle.step(DT).unwrap_err().to_string().contains("needs fields first"));
        assert!(WhitewaterLifecycle::new(WhitewaterGrid { cells: [2, N, N], ..grid() }, 10, 7).is_err());
        assert!(WhitewaterLifecycle::new(grid(), 0, 7).is_err());
    }
}
