// node.shape_particle_blobs — fusable BUFFER body, GATHER. One anisotropic
// surface kernel per sorted particle (Yu & Turk 2010, GPU_FLUID_SURFACE_DESIGN.md
// D14): neighbours within the kernel radius in the 27 surrounding bins give a
// weighted mean (centre smoothing) and a covariance whose principal axes shape
// a volume-preserving ellipsoid. Too few neighbours → isotropic. A particle
// with no neighbour within two physical radii shrinks toward isolated_scale by
// three radii (the slot-9 droplet rule on physical radii).
//
// ABI (buffer standalone codegen): `sorted` (FluidParticle → Element) and
// `cell_ranges` (CellRange → Element2) are gathered through `buf_sorted` /
// `buf_cell_ranges`; the output FluidBlob is Element3. The bin grid is the
// sort's (`bins_x/y/z`), never ceil(size / cell_size) again: fast-math
// division can land one bin past the ranges the sort wrote.

struct SpbEigen {
    values: vec3<f32>,
    // Columns are the unit eigenvectors.
    vectors: mat3x3<f32>,
}

// Cyclic Jacobi on a symmetric 3×3 (Numerical Recipes 11.1).
fn spb_eigen(covariance: mat3x3<f32>) -> SpbEigen {
    var a = covariance;
    var v = mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0));
    for (var sweep = 0; sweep < 6; sweep = sweep + 1) {
        for (var pair = 0; pair < 3; pair = pair + 1) {
            var p = 0;
            var q = 1;
            if pair == 1 {
                q = 2;
            } else if pair == 2 {
                p = 1;
                q = 2;
            }
            let apq = a[q][p];
            if abs(apq) <= 1e-20 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * apq);
            var t = 1.0 / (abs(theta) + sqrt(theta * theta + 1.0));
            if theta < 0.0 {
                t = -t;
            }
            let c = 1.0 / sqrt(t * t + 1.0);
            let s = t * c;
            var r = mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0));
            r[p][p] = c;
            r[q][q] = c;
            r[q][p] = s;
            r[p][q] = -s;
            a = transpose(r) * a * r;
            v = v * r;
        }
    }
    return SpbEigen(vec3<f32>(a[0][0], a[1][1], a[2][2]), v);
}

fn body(
    idx: u32,
    count: u32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    cell_size: f32,
    particle_scale: f32,
    stretch: f32,
    smoothing: f32,
    isolated_scale: f32,
    min_neighbours: i32,
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
) -> Element3 {
    let inactive = Element3(vec4<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0));
    let self_particle = buf_sorted[idx].position_radius;
    let physical = self_particle.w;
    let bins = vec3<i32>(bins_x, bins_y, bins_z);
    if !(physical > 0.0) || !(cell_size > 0.0) || any(bins < vec3<i32>(1)) {
        return inactive;
    }
    let x = self_particle.xyz;
    let size = vec3<f32>(size_x, size_y, size_z);
    let lattice_min = vec3<f32>(center_x, center_y, center_z) - 0.5 * size;
    let home = clamp(vec3<i32>(floor((x - lattice_min) / cell_size)), vec3<i32>(0), bins - vec3<i32>(1));
    // One home for the search radius: the kernel never reaches past one bin.
    let radius = min(particle_scale * physical, cell_size);

    var weight_sum = 0.0;
    var mean = vec3<f32>(0.0);
    var neighbours = 0;
    var nearest = 1e30;
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let b = home + vec3<i32>(dx, dy, dz);
                if any(b < vec3<i32>(0)) || any(b >= bins) {
                    continue;
                }
                let range = buf_cell_ranges[u32(b.x + bins.x * (b.y + bins.y * b.z))];
                for (var k = range.start; k < range.start + range.count; k = k + 1u) {
                    let other = buf_sorted[k].position_radius;
                    let d = length(other.xyz - x);
                    if k != idx {
                        nearest = min(nearest, d);
                    }
                    if d < radius {
                        let q = d / radius;
                        let w = 1.0 - q * q * q;
                        weight_sum = weight_sum + w;
                        mean = mean + w * other.xyz;
                        neighbours = neighbours + 1;
                    }
                }
            }
        }
    }
    // The particle itself is always a neighbour at weight 1.
    mean = mean / weight_sum;
    let centre = mix(x, mean, smoothing);

    // Isolated droplets: full size within two physical radii of a neighbour,
    // smoothstep to isolated_scale by three.
    let apart = smoothstep(2.0 * physical, 3.0 * physical, nearest);
    let size_scale = mix(1.0, isolated_scale, apart);
    let blob_radius = radius * size_scale;

    var axes = vec3<f32>(blob_radius);
    var basis = mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0));
    // stretch ≤ 1 caps every axis ratio at 1: the ellipsoid is the sphere
    // whatever the covariance says, so the second sweep and the eigensolve
    // are skipped and the sphere is written exactly (no rounding through
    // V · diag · Vᵀ).
    if neighbours >= min_neighbours && stretch > 1.0 {
        var covariance = mat3x3<f32>(vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0));
        for (var dz = -1; dz <= 1; dz = dz + 1) {
            for (var dy = -1; dy <= 1; dy = dy + 1) {
                for (var dx = -1; dx <= 1; dx = dx + 1) {
                    let b = home + vec3<i32>(dx, dy, dz);
                    if any(b < vec3<i32>(0)) || any(b >= bins) {
                        continue;
                    }
                    let range = buf_cell_ranges[u32(b.x + bins.x * (b.y + bins.y * b.z))];
                    for (var k = range.start; k < range.start + range.count; k = k + 1u) {
                        let other = buf_sorted[k].position_radius.xyz;
                        let d = length(other - x);
                        if d < radius {
                            let q = d / radius;
                            let w = 1.0 - q * q * q;
                            let offset = other - mean;
                            covariance = covariance + w * mat3x3<f32>(offset * offset.x, offset * offset.y, offset * offset.z);
                        }
                    }
                }
            }
        }
        covariance = covariance * (1.0 / weight_sum);
        let eigen = spb_eigen(covariance);
        let largest = max(max(eigen.values.x, eigen.values.y), eigen.values.z);
        if largest > 1e-20 {
            // Axis lengths ∝ sqrt(variance), the ratio capped at `stretch`,
            // rescaled to keep the isotropic kernel's volume.
            let limit = max(stretch, 1.0);
            let floor_variance = largest / (limit * limit);
            let spread = sqrt(max(eigen.values, vec3<f32>(floor_variance)));
            let norm = pow(spread.x * spread.y * spread.z, 1.0 / 3.0);
            axes = blob_radius * spread / norm;
            basis = eigen.vectors;
        }
    }
    // Reach at most (1 − band) of a bin from the particle, so any blob a lattice
    // node's ±1-bin search misses is at least band·bin away: node.particle_volume
    // caps its distance field at that band, which keeps the cap exact.
    // The band (0.1) is shared with particle_volume_body.wgsl.
    axes = min(axes, vec3<f32>(max(0.9 * cell_size - length(centre - x), 1e-6 * cell_size)));
    let inverse_axes = mat3x3<f32>(
        vec3<f32>(1.0 / axes.x, 0.0, 0.0),
        vec3<f32>(0.0, 1.0 / axes.y, 0.0),
        vec3<f32>(0.0, 0.0, 1.0 / axes.z),
    );
    let g = basis * inverse_axes * transpose(basis);
    let det = 1.0 / (axes.x * axes.y * axes.z);
    let bound = max(max(axes.x, axes.y), axes.z);
    return Element3(
        vec4<f32>(centre, bound),
        vec4<f32>(g[0][0], g[1][1], g[2][2], det),
        vec4<f32>(g[1][0], g[2][0], g[2][1], 0.0),
    );
}
