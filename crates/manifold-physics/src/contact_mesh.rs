//! A Collider's contact mesh (SCENE_REFINEMENT_DESIGN.md section 3, P2d
//! (Scan colliders)): the drawn surface simplified for Box3D by quadric edge
//! collapse with a maximum geometric error, meshoptimizer's `simplify`
//! through the `meshopt` crate, never by vertex clustering.
//!
//! The simplifier's error is an estimate: on a noisy scan, a 5 mm budget
//! leaves thousands of drawn vertices 5 to 20 mm off the result. So the
//! distance between the two surfaces is measured after each simplification,
//! every place over the tolerance has the drawn vertices there locked, and
//! the simplification runs again until nothing is over. Water never reads
//! the contact mesh: water builds its own distance lattice from the exact
//! mesh.

use crate::sdf::distance_squared_to_triangle;
use crate::TriangleMesh;

/// The farthest a Collider's contact mesh may sit from its drawn surface, in
/// metres. A small body or a splash rests on what it sees.
pub const CONTACT_TOLERANCE: f32 = 0.005;

/// Simplifications tried, each with more drawn vertices locked, before the
/// exact mesh is used as it is.
const ATTEMPTS: usize = 12;

/// The simplifier's budget as a share of the tolerance: its estimate runs
/// under the measured distance, so it aims lower and the locks catch the rest.
const BUDGET: f32 = 0.5;

/// A simplified surface and its measured distance from the original.
#[derive(Clone, Debug)]
pub struct Simplified {
    /// Local and unscaled, like the mesh it came from.
    pub mesh: TriangleMesh,
    /// The largest measured distance between the two surfaces at the scale
    /// it was built for, metres; 0 when nothing could be removed.
    pub error: f32,
}

/// `exact` (local) simplified so that, at `scale`, every vertex of the
/// drawn surface and the middle and edge midpoints of every kept triangle
/// lie within `tolerance` metres of the other surface. The simplifier works
/// on the scaled points, so the bound is in metres on every axis. When no
/// attempt meets the bound, or nothing can be removed, the exact mesh comes
/// back with error 0. Costs a simplification and a measurement per attempt;
/// never on the content thread.
pub fn simplify_within(exact: &TriangleMesh, scale: [f32; 3], tolerance: f32) -> Simplified {
    let unchanged = || Simplified { mesh: exact.clone(), error: 0.0 };
    if !(tolerance.is_finite() && tolerance > 0.0) {
        return unchanged();
    }
    let world: Vec<[f32; 3]> =
        exact.vertices.iter().map(|vertex| std::array::from_fn(|axis| vertex[axis] * scale[axis])).collect();
    let indices = exact.triangles.as_flattened();
    let exact_grid = TriangleGrid::new(&world, &exact.triangles);
    let rings = Rings::new(world.len(), &exact.triangles);
    let budget = tolerance * BUDGET;
    let mut locked = vec![false; world.len()];
    for _ in 0..ATTEMPTS {
        let kept = meshopt::simplify_with_locks_decoder(indices, &world, &locked, 0, budget, meshopt::SimplifyOptions::ErrorAbsolute, None);
        let triangles: Vec<[u32; 3]> = kept.chunks_exact(3).map(|corners| [corners[0], corners[1], corners[2]]).collect();
        if triangles.is_empty() || triangles.len() >= exact.triangles.len() {
            return unchanged();
        }
        let measured = measure(&world, &exact.triangles, &exact_grid, &triangles, f64::from(tolerance));
        if measured.lock.is_empty() && measured.farthest <= f64::from(tolerance) {
            return Simplified { mesh: compact(&exact.vertices, &triangles), error: measured.farthest as f32 };
        }
        for vertex in measured.lock {
            locked[vertex as usize] = true;
            for &neighbour in rings.of(vertex) {
                locked[neighbour as usize] = true;
            }
        }
    }
    unchanged()
}

/// Each vertex's neighbours: the vertices it shares a triangle with.
struct Rings {
    starts: Vec<u32>,
    neighbours: Vec<u32>,
}

impl Rings {
    fn new(vertex_count: usize, triangles: &[[u32; 3]]) -> Self {
        let mut starts = vec![0u32; vertex_count + 1];
        for triangle in triangles {
            for &corner in triangle {
                starts[corner as usize + 1] += 2;
            }
        }
        for index in 0..vertex_count {
            starts[index + 1] += starts[index];
        }
        let mut fill = starts.clone();
        let mut neighbours = vec![0u32; starts[vertex_count] as usize];
        for triangle in triangles {
            for corner in 0..3 {
                let slot = &mut fill[triangle[corner] as usize];
                neighbours[*slot as usize] = triangle[(corner + 1) % 3];
                neighbours[*slot as usize + 1] = triangle[(corner + 2) % 3];
                *slot += 2;
            }
        }
        Self { starts, neighbours }
    }

    fn of(&self, vertex: u32) -> &[u32] {
        &self.neighbours[self.starts[vertex as usize] as usize..self.starts[vertex as usize + 1] as usize]
    }
}

/// The largest distance between two surfaces over the same points, and the
/// drawn vertices to lock where it is over the tolerance.
struct Measured {
    farthest: f64,
    lock: Vec<u32>,
}

/// Measure `kept` against `exact` (triangles over the same `points`): every
/// drawn vertex `kept` dropped against `kept`, which locks that vertex when
/// it is over `tolerance`; and the middle and edge midpoints of every kept
/// triangle against `exact`, which locks every drawn vertex under that kept
/// triangle when over, so the simplifier cannot span that place again. A
/// kept vertex lies on both surfaces, so it is skipped.
fn measure(points: &[[f32; 3]], exact: &[[u32; 3]], exact_grid: &TriangleGrid, kept: &[[u32; 3]], tolerance: f64) -> Measured {
    let mut on_kept = vec![false; points.len()];
    for &index in kept.as_flattened() {
        on_kept[index as usize] = true;
    }
    let mut on_exact = vec![false; points.len()];
    for &index in exact.as_flattened() {
        on_exact[index as usize] = true;
    }
    let dropped: Vec<u32> = (0..points.len() as u32).filter(|&index| on_exact[index as usize] && !on_kept[index as usize]).collect();
    let kept_grid = TriangleGrid::new(points, kept);
    let (drawn_far, mut lock) = search(&dropped, |&vertex, lock| {
        let (distance, _) = kept_grid.nearest(points[vertex as usize].map(f64::from));
        if distance > tolerance {
            lock.push(vertex);
        }
        distance
    });
    // Each sample with its reach: the farthest corner of its kept triangle.
    let samples: Vec<([f64; 3], f64)> = kept
        .iter()
        .flat_map(|triangle| {
            let corners = triangle.map(|index| points[index as usize].map(f64::from));
            let [a, b, c] = corners;
            let mid = |p: [f64; 3], q: [f64; 3]| -> [f64; 3] { std::array::from_fn(|axis| 0.5 * (p[axis] + q[axis])) };
            [std::array::from_fn(|axis| (a[axis] + b[axis] + c[axis]) / 3.0), mid(a, b), mid(b, c), mid(c, a)].map(|sample| {
                let reach = corners.iter().map(|corner| distance(sample, *corner)).fold(0.0, f64::max);
                (sample, reach)
            })
        })
        .collect();
    let (kept_far, more) = search(&samples, |&(sample, reach), lock| {
        let (distance, _) = exact_grid.nearest(sample);
        if distance > tolerance {
            exact_grid.within(sample, reach, |triangle| lock.extend(exact[triangle as usize]));
        }
        distance
    });
    lock.extend(more);
    lock.sort_unstable();
    lock.dedup();
    Measured { farthest: drawn_far.max(kept_far), lock }
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|axis| (a[axis] - b[axis]).powi(2)).sum::<f64>().sqrt()
}

/// `probe` over every query on every core: the largest distance it returns,
/// and every vertex it pushes to lock.
fn search<Q: Sync>(queries: &[Q], probe: impl Fn(&Q, &mut Vec<u32>) -> f64 + Sync) -> (f64, Vec<u32>) {
    if queries.is_empty() {
        return (0.0, Vec::new());
    }
    let workers = std::thread::available_parallelism().map_or(1, usize::from).min(queries.len());
    let chunk = queries.len().div_ceil(workers);
    let probe = &probe;
    std::thread::scope(|scope| {
        let handles: Vec<_> = queries
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    let mut farthest = 0.0f64;
                    let mut lock = Vec::new();
                    for query in part {
                        farthest = farthest.max(probe(query, &mut lock));
                    }
                    (farthest, lock)
                })
            })
            .collect();
        handles.into_iter().fold((0.0f64, Vec::new()), |(farthest, mut lock), handle| {
            let (part_far, part_lock) = handle.join().unwrap_or((f64::INFINITY, Vec::new()));
            lock.extend(part_lock);
            (farthest.max(part_far), lock)
        })
    })
}

/// A local mesh holding only the vertices `triangles` use, renumbered.
fn compact(local: &[[f32; 3]], triangles: &[[u32; 3]]) -> TriangleMesh {
    let mut remap = vec![u32::MAX; local.len()];
    let mut vertices = Vec::new();
    let triangles = triangles
        .iter()
        .map(|triangle| {
            triangle.map(|index| {
                let slot = &mut remap[index as usize];
                if *slot == u32::MAX {
                    *slot = vertices.len() as u32;
                    vertices.push(local[index as usize]);
                }
                *slot
            })
        })
        .collect();
    TriangleMesh { vertices, triangles }
}

/// Bits per axis in a cell key.
const KEY_BITS: u32 = 21;

/// Triangles binned on a uniform grid, for nearest-surface distance.
struct TriangleGrid<'a> {
    points: &'a [[f32; 3]],
    triangles: &'a [[u32; 3]],
    origin: [f64; 3],
    cell: f64,
    dims: [i64; 3],
    /// Sorted occupied cell keys, with `starts[i]..starts[i + 1]` their
    /// span of `members`.
    keys: Vec<u64>,
    starts: Vec<u32>,
    members: Vec<u32>,
}

impl<'a> TriangleGrid<'a> {
    /// Cells start at the mean triangle size and double until a triangle
    /// touches about four cells on average, so one huge triangle among small
    /// ones never fills millions of cells.
    fn new(points: &'a [[f32; 3]], triangles: &'a [[u32; 3]]) -> Self {
        let corners = |triangle: &[u32; 3]| triangle.map(|index| points[index as usize].map(f64::from));
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        let mut size_sum = 0.0;
        for triangle in triangles {
            let [a, b, c] = corners(triangle);
            let mut longest = 0.0f64;
            for axis in 0..3 {
                let low = a[axis].min(b[axis]).min(c[axis]);
                let high = a[axis].max(b[axis]).max(c[axis]);
                min[axis] = min[axis].min(low);
                max[axis] = max[axis].max(high);
                longest = longest.max(high - low);
            }
            size_sum += longest;
        }
        let extent = (0..3).map(|axis| max[axis] - min[axis]).fold(0.0f64, f64::max);
        let floor = (extent / f64::from(1u32 << (KEY_BITS - 1))).max(f64::MIN_POSITIVE);
        let mut cell = (size_sum / triangles.len().max(1) as f64).max(floor);
        let origin = if extent.is_finite() { min } else { [0.0; 3] };
        let span = |triangle: &[u32; 3], cell: f64| -> [[i64; 3]; 2] {
            let [a, b, c] = corners(triangle);
            let low = std::array::from_fn(|axis| ((a[axis].min(b[axis]).min(c[axis]) - origin[axis]) / cell).floor() as i64);
            let high = std::array::from_fn(|axis| ((a[axis].max(b[axis]).max(c[axis]) - origin[axis]) / cell).floor() as i64);
            [low, high]
        };
        let budget = 4 * triangles.len() as u64 + (1 << 20);
        loop {
            let touched: u64 = triangles
                .iter()
                .map(|triangle| {
                    let [low, high] = span(triangle, cell);
                    (0..3).map(|axis| (high[axis] - low[axis] + 1) as u64).product::<u64>()
                })
                .sum();
            if touched <= budget {
                break;
            }
            cell *= 2.0;
        }
        let dims = std::array::from_fn(|axis| if extent.is_finite() { ((max[axis] - min[axis]) / cell).floor() as i64 + 1 } else { 1 });
        let mut pairs = Vec::new();
        for (index, triangle) in triangles.iter().enumerate() {
            let [low, high] = span(triangle, cell);
            for x in low[0]..=high[0] {
                for y in low[1]..=high[1] {
                    for z in low[2]..=high[2] {
                        pairs.push((key([x, y, z]), index as u32));
                    }
                }
            }
        }
        pairs.sort_unstable();
        let mut keys = Vec::new();
        let mut starts = Vec::new();
        let mut members = Vec::with_capacity(pairs.len());
        for (cell_key, triangle) in pairs {
            if keys.last() != Some(&cell_key) {
                keys.push(cell_key);
                starts.push(members.len() as u32);
            }
            members.push(triangle);
        }
        starts.push(members.len() as u32);
        Self { points, triangles, origin, cell, dims, keys, starts, members }
    }

    fn members(&self, cell: [i64; 3]) -> &[u32] {
        match self.keys.binary_search(&key(cell)) {
            Ok(slot) => &self.members[self.starts[slot] as usize..self.starts[slot + 1] as usize],
            Err(_) => &[],
        }
    }

    /// The distance from `p` to the nearest triangle, and that triangle's
    /// index (0 when there is none). Searches shells of
    /// cells outward from `p`'s cell; a triangle in an unsearched shell is
    /// at least `k` cells away, so the search stops once the nearest found
    /// is closer than that.
    fn nearest(&self, p: [f64; 3]) -> (f64, u32) {
        let centre: [i64; 3] = std::array::from_fn(|axis| ((p[axis] - self.origin[axis]) / self.cell).floor() as i64);
        let last_shell = (0..3).map(|axis| centre[axis].abs().max((self.dims[axis] - 1 - centre[axis]).abs())).max().unwrap_or(0);
        let mut best = (f64::INFINITY, 0u32);
        let visit = |cell: [i64; 3], best: &mut (f64, u32)| {
            for &triangle in self.members(cell) {
                let corners = self.triangles[triangle as usize].map(|index| self.points[index as usize].map(f64::from));
                let squared = distance_squared_to_triangle(p, &corners);
                if squared < best.0 {
                    *best = (squared, triangle);
                }
            }
        };
        for k in 0..=last_shell {
            let range = |axis: usize| (centre[axis] - k).max(0)..=(centre[axis] + k).min(self.dims[axis] - 1);
            for x in range(0) {
                for y in range(1) {
                    if (x - centre[0]).abs() == k || (y - centre[1]).abs() == k {
                        for z in range(2) {
                            visit([x, y, z], &mut best);
                        }
                    } else {
                        for z in [centre[2] - k, centre[2] + k] {
                            if (0..self.dims[2]).contains(&z) {
                                visit([x, y, z], &mut best);
                            }
                        }
                    }
                }
            }
            let reach = k as f64 * self.cell;
            if best.0 <= reach * reach {
                break;
            }
        }
        (best.0.sqrt(), best.1)
    }

    /// Visit every triangle within `reach` of `p`, some more than once.
    fn within(&self, p: [f64; 3], reach: f64, mut visit: impl FnMut(u32)) {
        let cell_of = |value: f64, axis: usize| ((value - self.origin[axis]) / self.cell).floor() as i64;
        let low: [i64; 3] = std::array::from_fn(|axis| cell_of(p[axis] - reach, axis).max(0));
        let high: [i64; 3] = std::array::from_fn(|axis| cell_of(p[axis] + reach, axis).min(self.dims[axis] - 1));
        for x in low[0]..=high[0] {
            for y in low[1]..=high[1] {
                for z in low[2]..=high[2] {
                    for &triangle in self.members([x, y, z]) {
                        let corners = self.triangles[triangle as usize].map(|index| self.points[index as usize].map(f64::from));
                        if distance_squared_to_triangle(p, &corners) <= reach * reach {
                            visit(triangle);
                        }
                    }
                }
            }
        }
    }
}

fn key(cell: [i64; 3]) -> u64 {
    let mask = (1u64 << KEY_BITS) - 1;
    ((cell[0] as u64 & mask) << (2 * KEY_BITS)) | ((cell[1] as u64 & mask) << KEY_BITS) | (cell[2] as u64 & mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `side` × `side` metre square of `n` × `n` quads at height `height(x, z)`.
    fn sheet(n: u32, side: f32, height: impl Fn(f32, f32) -> f32) -> TriangleMesh {
        let step = side / n as f32;
        let mut vertices = Vec::new();
        for i in 0..=n {
            for j in 0..=n {
                let (x, z) = (i as f32 * step, j as f32 * step);
                vertices.push([x, height(x, z), z]);
            }
        }
        let at = |i: u32, j: u32| i * (n + 1) + j;
        let mut triangles = Vec::new();
        for i in 0..n {
            for j in 0..n {
                triangles.push([at(i, j), at(i, j + 1), at(i + 1, j)]);
                triangles.push([at(i + 1, j), at(i, j + 1), at(i + 1, j + 1)]);
            }
        }
        TriangleMesh { vertices, triangles }
    }

    /// A flat dense sheet collapses to almost nothing, exactly on its plane.
    #[test]
    fn a_flat_sheet_collapses_on_its_plane() {
        let exact = sheet(64, 4.0, |_, _| 0.0);
        let simplified = simplify_within(&exact, [1.0; 3], CONTACT_TOLERANCE);
        assert!(simplified.mesh.triangles.len() < 64, "kept {}", simplified.mesh.triangles.len());
        assert!(simplified.error < 1e-6, "error {}", simplified.error);
    }

    /// A bumpy surface keeps its bumps: the measured distance stays within
    /// the tolerance at the scale it was built for, and the measurement is
    /// a real one (a scale that shrinks the tolerance keeps more triangles).
    #[test]
    fn a_bumpy_sheet_stays_within_the_tolerance_at_its_scale() {
        let exact = sheet(96, 4.0, |x, z| 0.2 * (x * 3.0).sin() * (z * 2.0).cos());
        let near = simplify_within(&exact, [1.0; 3], CONTACT_TOLERANCE);
        assert!(near.error <= CONTACT_TOLERANCE && near.error > 0.0, "error {}", near.error);
        assert!(near.mesh.triangles.len() < exact.triangles.len() / 2, "kept {}", near.mesh.triangles.len());
        let grown = simplify_within(&exact, [10.0; 3], CONTACT_TOLERANCE);
        assert!(grown.error <= CONTACT_TOLERANCE);
        assert!(grown.mesh.triangles.len() > near.mesh.triangles.len(), "a bigger item keeps more detail");
    }

    /// The grid's distance matches a brute-force scan over every triangle.
    #[test]
    fn grid_distance_matches_brute_force() {
        let mesh = sheet(12, 2.0, |x, z| 0.3 * (x * 2.0).sin() + 0.1 * z);
        let grid = TriangleGrid::new(&mesh.vertices, &mesh.triangles);
        for i in 0..40 {
            let t = i as f64 * 0.37;
            let p = [t.sin() * 3.0, t.cos() * 2.0, (t * 0.7).sin() * 4.0];
            let brute = mesh
                .triangles
                .iter()
                .map(|triangle| distance_squared_to_triangle(p, &triangle.map(|index| mesh.vertices[index as usize].map(f64::from))))
                .fold(f64::INFINITY, f64::min)
                .sqrt();
            let (found, _) = grid.nearest(p);
            assert!((found - brute).abs() < 1e-9, "at {p:?}: grid {found} brute {brute}");
        }
    }
}
