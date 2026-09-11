//! Offline particle-density surface reconstruction.

use std::f32::consts::PI;

use manifold_renderer::generators::mesh_common::MeshVertex;

const TETRA_EDGES: [(usize, usize); 6] = [(0, 1), (1, 2), (2, 0), (0, 3), (1, 3), (2, 3)];
const TETRAHEDRA: [[usize; 4]; 6] = [
    [0, 1, 3, 7],
    [0, 3, 2, 7],
    [0, 2, 6, 7],
    [0, 6, 4, 7],
    [0, 4, 5, 7],
    [0, 5, 1, 7],
];

/// Reconstruct a triangle surface from particle positions and masses.
///
/// The input particles are `[x, y, z, mass_kg]`. Density uses the same
/// normalized poly6 kernel as the live water shader, including its `mass /`
/// `1000` factor.
pub(super) fn reconstruct_surface(
    particles: &[[f32; 4]],
    origin: [f32; 3],
    dims: [usize; 3],
    spacing: f32,
    radius: f32,
    iso: f32,
) -> Result<Vec<MeshVertex>, String> {
    validate_inputs(particles, origin, dims, spacing, radius, iso)?;

    let total = dims[0]
        .checked_mul(dims[1])
        .and_then(|n| n.checked_mul(dims[2]))
        .ok_or_else(|| "density grid dimensions overflow usize".to_owned())?;
    let mut samples = vec![GridSample::default(); total];
    scatter_density_and_gradient(&mut samples, particles, origin, dims, spacing, radius);

    Ok(extract_grid(&samples, origin, dims, spacing, iso))
}

fn validate_inputs(
    particles: &[[f32; 4]],
    origin: [f32; 3],
    dims: [usize; 3],
    spacing: f32,
    radius: f32,
    iso: f32,
) -> Result<(), String> {
    if dims.iter().any(|&dim| dim < 2) {
        return Err("density grid dimensions must each be at least 2".to_owned());
    }
    if !spacing.is_finite() || spacing <= 0.0 {
        return Err("grid spacing must be finite and positive".to_owned());
    }
    if !radius.is_finite() || radius <= 0.0 {
        return Err("kernel radius must be finite and positive".to_owned());
    }
    if !iso.is_finite() || origin.iter().any(|value| !value.is_finite()) {
        return Err("origin and iso value must be finite".to_owned());
    }
    for (index, particle) in particles.iter().enumerate() {
        if particle.iter().any(|value| !value.is_finite()) {
            return Err(format!("particle {index} contains a non-finite value"));
        }
        if particle[3] < 0.0 {
            return Err(format!("particle {index} has a negative mass"));
        }
    }
    Ok(())
}

fn grid_index(dims: [usize; 3], x: usize, y: usize, z: usize) -> usize {
    x + dims[0] * (y + dims[1] * z)
}

#[derive(Clone, Copy, Default)]
struct GridSample {
    density: f32,
    gradient: [f32; 3],
}

fn scatter_density_and_gradient(
    samples: &mut [GridSample],
    particles: &[[f32; 4]],
    origin: [f32; 3],
    dims: [usize; 3],
    spacing: f32,
    radius: f32,
) {
    let radius2 = radius * radius;
    let normalizer = 315.0 / (64.0 * PI * radius * radius * radius);
    let gradient_scale = -6.0 * normalizer / radius2;

    for particle in particles {
        let mut ranges = [(0usize, 0usize); 3];
        let mut supported = true;
        for axis in 0..3 {
            let lower = ((particle[axis] - radius - origin[axis]) / spacing).floor() as isize - 1;
            let upper = ((particle[axis] + radius - origin[axis]) / spacing).ceil() as isize + 1;
            if upper < 0 || lower >= dims[axis] as isize {
                supported = false;
                break;
            }
            ranges[axis] = (
                lower.max(0) as usize,
                upper.min(dims[axis] as isize - 1) as usize,
            );
        }
        if !supported {
            continue;
        }

        let mass = particle[3] / 1000.0;
        for z in ranges[2].0..=ranges[2].1 {
            for y in ranges[1].0..=ranges[1].1 {
                for x in ranges[0].0..=ranges[0].1 {
                    let position = [
                        origin[0] + x as f32 * spacing,
                        origin[1] + y as f32 * spacing,
                        origin[2] + z as f32 * spacing,
                    ];
                    let delta = sub(position, [particle[0], particle[1], particle[2]]);
                    let distance2 = dot(delta, delta);
                    if distance2 >= radius2 {
                        continue;
                    }
                    let q = 1.0 - distance2 / radius2;
                    let q2 = q * q;
                    let sample = &mut samples[grid_index(dims, x, y, z)];
                    sample.density += mass * normalizer * q2 * q;
                    let scale = mass * gradient_scale * q2;
                    for (gradient, component) in sample.gradient.iter_mut().zip(delta) {
                        *gradient += scale * component;
                    }
                }
            }
        }
    }
}

/// Extract a grid whose node gradients were accumulated with the density.
fn extract_grid(
    samples: &[GridSample],
    origin: [f32; 3],
    dims: [usize; 3],
    spacing: f32,
    iso: f32,
) -> Vec<MeshVertex> {
    let mut vertices = Vec::new();
    for z in 0..dims[2] - 1 {
        for y in 0..dims[1] - 1 {
            for x in 0..dims[0] - 1 {
                let corner_positions = cube_positions(origin, spacing, x, y, z);
                let corner_samples = [
                    samples[grid_index(dims, x, y, z)],
                    samples[grid_index(dims, x + 1, y, z)],
                    samples[grid_index(dims, x, y + 1, z)],
                    samples[grid_index(dims, x + 1, y + 1, z)],
                    samples[grid_index(dims, x, y, z + 1)],
                    samples[grid_index(dims, x + 1, y, z + 1)],
                    samples[grid_index(dims, x, y + 1, z + 1)],
                    samples[grid_index(dims, x + 1, y + 1, z + 1)],
                ];
                let signed = corner_samples.map(|sample| sample.density - iso);
                if signed.iter().all(|&v| v >= 0.0) || signed.iter().all(|&v| v < 0.0) {
                    continue;
                }
                for tetra in TETRAHEDRA {
                    let mut points = Vec::with_capacity(4);
                    for &(a, b) in &TETRA_EDGES {
                        let ca = tetra[a];
                        let cb = tetra[b];
                        let va = signed[ca];
                        let vb = signed[cb];
                        if (va >= 0.0) == (vb >= 0.0) {
                            continue;
                        }
                        let denominator = va - vb;
                        if denominator == 0.0 {
                            continue;
                        }
                        let t = (va / denominator).clamp(0.0, 1.0);
                        let position = lerp(corner_positions[ca], corner_positions[cb], t);
                        let gradient =
                            lerp(corner_samples[ca].gradient, corner_samples[cb].gradient, t);
                        let outward = normalize([-gradient[0], -gradient[1], -gradient[2]]);
                        if !points.iter().any(|point: &SurfacePoint| {
                            distance2(point.position, position) < 1.0e-12
                        }) {
                            points.push(SurfacePoint { position, outward });
                        }
                    }
                    if points.len() < 3 {
                        continue;
                    }
                    order_points(&mut points);
                    let outward = points
                        .iter()
                        .fold([0.0; 3], |sum, point| add(sum, point.outward));
                    if points.len() == 3 {
                        append_triangle(&mut vertices, points[0], points[1], points[2], outward);
                    } else {
                        append_triangle(&mut vertices, points[0], points[1], points[2], outward);
                        append_triangle(&mut vertices, points[0], points[2], points[3], outward);
                    }
                }
            }
        }
    }
    vertices
}

#[derive(Clone, Copy)]
struct SurfacePoint {
    position: [f32; 3],
    outward: [f32; 3],
}

fn cube_positions(origin: [f32; 3], spacing: f32, x: usize, y: usize, z: usize) -> [[f32; 3]; 8] {
    let base = [
        origin[0] + x as f32 * spacing,
        origin[1] + y as f32 * spacing,
        origin[2] + z as f32 * spacing,
    ];
    [
        base,
        [base[0] + spacing, base[1], base[2]],
        [base[0], base[1] + spacing, base[2]],
        [base[0] + spacing, base[1] + spacing, base[2]],
        [base[0], base[1], base[2] + spacing],
        [base[0] + spacing, base[1], base[2] + spacing],
        [base[0], base[1] + spacing, base[2] + spacing],
        [base[0] + spacing, base[1] + spacing, base[2] + spacing],
    ]
}

fn order_points(points: &mut [SurfacePoint]) {
    let center = points
        .iter()
        .fold([0.0; 3], |sum, point| add(sum, point.position));
    let inverse_len = 1.0 / points.len() as f32;
    let center = [
        center[0] * inverse_len,
        center[1] * inverse_len,
        center[2] * inverse_len,
    ];
    let mut normal = points
        .iter()
        .fold([0.0; 3], |sum, point| add(sum, point.outward));
    if length2(normal) <= 1.0e-12 {
        normal = cross(
            sub(points[1].position, points[0].position),
            sub(points[2].position, points[0].position),
        );
    }
    normal = normalize(normal);
    let axis = if normal[0].abs() < 0.8 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u = normalize(cross(normal, axis));
    let v = cross(normal, u);
    points.sort_by(|a, b| {
        let aa = sub(a.position, center);
        let bb = sub(b.position, center);
        let angle_a = dot(aa, v).atan2(dot(aa, u));
        let angle_b = dot(bb, v).atan2(dot(bb, u));
        angle_a.total_cmp(&angle_b)
    });
    let winding = cross(
        sub(points[1].position, points[0].position),
        sub(points[2].position, points[0].position),
    );
    if dot(winding, normal) < 0.0 {
        points.swap(1, points.len() - 1);
    }
}

fn append_triangle(
    vertices: &mut Vec<MeshVertex>,
    a: SurfacePoint,
    b: SurfacePoint,
    c: SurfacePoint,
    outward: [f32; 3],
) {
    let mut points = [a, b, c];
    let winding = cross(
        sub(points[1].position, points[0].position),
        sub(points[2].position, points[0].position),
    );
    if length2(winding) <= 1.0e-12 {
        return;
    }
    if length2(outward) > 1.0e-12 && dot(winding, outward) < 0.0 {
        points.swap(1, 2);
    }
    let fallback = normalize(winding);
    for point in points {
        let normal = if length2(point.outward) > 1.0e-12 {
            point.outward
        } else {
            fallback
        };
        vertices.push(MeshVertex {
            position: point.position,
            _pad0: 0.0,
            normal,
            _pad1: 0.0,
            uv: [0.0; 2],
            _pad2: [0.0; 2],
            tangent: [0.0; 4],
        });
    }
}

fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length2(a: [f32; 3]) -> f32 {
    dot(a, a)
}

fn distance2(a: [f32; 3], b: [f32; 3]) -> f32 {
    length2(sub(a, b))
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    let len2 = length2(a);
    if len2 <= 1.0e-20 || !len2.is_finite() {
        [0.0; 3]
    } else {
        let inverse_len = len2.sqrt().recip();
        [a[0] * inverse_len, a[1] * inverse_len, a[2] * inverse_len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planar_field_has_outward_normals_and_winding() {
        let dims = [3, 3, 3];
        let mut samples = vec![GridSample::default(); dims[0] * dims[1] * dims[2]];
        for z in 0..dims[2] {
            for y in 0..dims[1] {
                for x in 0..dims[0] {
                    samples[grid_index(dims, x, y, z)] = GridSample {
                        density: x as f32,
                        gradient: [1.0, 0.0, 0.0],
                    };
                }
            }
        }
        let vertices = extract_grid(&samples, [0.0; 3], dims, 1.0, 0.5);
        assert!(!vertices.is_empty());
        for triangle in vertices.chunks_exact(3) {
            assert!(triangle.iter().all(|vertex| vertex.position[0] == 0.5));
            assert!(
                triangle
                    .iter()
                    .all(|vertex| vertex.normal == [-1.0, 0.0, 0.0])
            );
            let winding = cross(
                sub(triangle[1].position, triangle[0].position),
                sub(triangle[2].position, triangle[0].position),
            );
            assert!(dot(winding, triangle[0].normal) > 0.0);
        }
    }

    #[test]
    fn poly6_density_is_normalized_and_linear_in_mass() {
        let position = [0.0, 0.0, 0.0];
        let radius = 1.0;
        let unit = [[0.0, 0.0, 0.0, 1.0]];
        let double = [[0.0, 0.0, 0.0, 2.0]];
        let expected = 315.0 / (64.0 * PI * 1000.0);
        let mut one_grid = [GridSample::default(); 1];
        let mut two_grid = [GridSample::default(); 1];
        scatter_density_and_gradient(&mut one_grid, &unit, position, [1, 1, 1], 1.0, radius);
        scatter_density_and_gradient(&mut two_grid, &double, position, [1, 1, 1], 1.0, radius);
        let one = one_grid[0].density;
        let two = two_grid[0].density;
        assert!((one - expected).abs() < 1.0e-6);
        assert!((two - 2.0 * one).abs() < 1.0e-6);
        let mut boundary_grid = [GridSample::default(); 1];
        scatter_density_and_gradient(
            &mut boundary_grid,
            &unit,
            [1.0, 0.0, 0.0],
            [1, 1, 1],
            1.0,
            radius,
        );
        assert_eq!(boundary_grid[0].density, 0.0);
    }
}
