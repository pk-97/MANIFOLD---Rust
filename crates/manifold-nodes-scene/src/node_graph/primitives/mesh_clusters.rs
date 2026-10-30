//! Crack-free cluster hierarchy for one big imported mesh (meshoptimizer
//! clusterlod), so `node.render_scene` can draw each part of it at the
//! coarsest level that stays under a pixel.
//!
//! The `clusters` array is `u32` words, floats stored with `f32::to_bits`.
//! A `CLUSTER_HEADER_WORDS` header: w0 cluster count c, w1 the word the
//! records start (the header size), w2 the word the indices start, w3 the
//! total index words, w4..w6 the dispatch for one workgroup per cluster
//! (`[min(c, 32768), ceil(c / 32768), 1]`). Then c records of
//! `CLUSTER_RECORD_WORDS`, then every cluster's indices in record order.
//!
//! Record words: 0-2 box min, 3 first (absolute word of the cluster's
//! indices); 4-6 box max, 7 index count; 8-10 normal cone axis, 11 cone
//! sine; 12-14 centre, 15 radius, 16 error of the cluster's own group's
//! simplified bounds (`f32::MAX` for a terminal group); 17-20 and 21 the
//! same for the group it was refined from (all zero with error -1.0 for
//! original geometry); 22-23 zero.
//!
//! A mesh the hierarchy can't be built for, or that is too small for one,
//! is laid out as runs of [`CLUSTER_TRIANGLES`] triangles of its own
//! indices, each always on the cut, so its draw never depends on the build.

use manifold_node_engine::mesh::MeshVertex;

/// Header words at the front of a `clusters` array.
pub(crate) const CLUSTER_HEADER_WORDS: u32 = 16;
/// Words per cluster record.
pub(crate) const CLUSTER_RECORD_WORDS: u32 = 24;
/// Triangles per cluster at most.
pub(crate) const CLUSTER_TRIANGLES: usize = 128;

/// Largest dispatch dimension the header's workgroup counts use.
const DISPATCH_X_MAX: u32 = 32768;
/// Smallest mesh worth clustering, in indices.
const MIN_CLUSTER_INDICES: usize = 3 * CLUSTER_TRIANGLES * 8;

/// Whether a mesh of `count` indices is big enough for a hierarchy. Scene
/// compile passes the vertex count, known before the indices; a mesh that
/// turns out smaller still draws, as runs of its own indices.
pub(crate) fn worth_clusters(count: u32) -> bool {
    count as usize >= MIN_CLUSTER_INDICES
}

/// Words the `clusters` array of a mesh of `index_count` indices may take.
pub(crate) fn clusters_capacity(index_count: u32) -> u32 {
    CLUSTER_HEADER_WORDS + index_count / 2 * 5
}

fn empty_header() -> Vec<u32> {
    vec![0u32; CLUSTER_HEADER_WORDS as usize]
}

/// A group's simplified bounds that never coarsen: the error past every
/// threshold.
const ALWAYS: meshopt::ClodBounds = meshopt::ClodBounds {
    center: [0.0; 3],
    radius: 0.0,
    error: f32::MAX,
};

/// Normal cone of a cluster's triangles: unit axis and sine of the half
/// angle (2.0 when the faces do not share a hemisphere).
fn normal_cone(positions: &[[f32; 3]], indices: &[u32]) -> ([f32; 3], f32) {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let mut normals = Vec::with_capacity(indices.len() / 3);
    let mut sum = [0.0f32; 3];
    for tri in indices.chunks_exact(3) {
        let (a, b, c) = (
            positions[tri[0] as usize],
            positions[tri[1] as usize],
            positions[tri[2] as usize],
        );
        let n = cross(sub(b, a), sub(c, a));
        let len = dot(n, n).sqrt();
        if len > 1e-20 {
            let n = [n[0] / len, n[1] / len, n[2] / len];
            for k in 0..3 {
                sum[k] += n[k];
            }
            normals.push(n);
        }
    }
    let len = dot(sum, sum).sqrt();
    if len <= 1e-20 {
        return ([0.0; 3], 2.0);
    }
    let axis = [sum[0] / len, sum[1] / len, sum[2] / len];
    let least = normals.iter().map(|&n| dot(n, axis)).fold(1.0f32, f32::min);
    let sine = if least > 0.0 {
        (1.0 - least * least).max(0.0).sqrt()
    } else {
        2.0
    };
    (axis, sine)
}

/// The mesh's cluster hierarchy, laid out as `clusters`: runs of its own
/// indices when the hierarchy can't be built, fits nowhere, or the mesh is
/// too small; just a zeroed header for indices that don't fit the vertices.
pub(crate) fn build_clusters(vertices: &[MeshVertex], indices: &[u32]) -> Vec<u32> {
    if indices.iter().any(|&i| i as usize >= vertices.len()) {
        return empty_header();
    }
    let positions: Vec<[f32; 3]> = vertices.iter().map(|v| v.position).collect();
    let runs = || {
        let clusters: Vec<(usize, i32, Vec<u32>)> = indices
            .chunks(3 * CLUSTER_TRIANGLES)
            .map(|run| (0, -1, run.to_vec()))
            .collect();
        lay_out(&positions, &[ALWAYS], &clusters)
    };
    if !worth_clusters(indices.len() as u32) {
        return runs();
    }
    let Ok(adapter) = meshopt::VertexDataAdapter::new(
        bytemuck::cast_slice(&positions),
        std::mem::size_of::<[f32; 3]>(),
        0,
    ) else {
        return runs();
    };
    // UV seams stay put: the two UV attributes are protected.
    let attributes: Vec<[f32; 5]> = vertices
        .iter()
        .map(|v| [v.uv[0], v.uv[1], v.normal[0], v.normal[1], v.normal[2]])
        .collect();
    let weights = [1.0f32, 1.0, 0.5, 0.5, 0.5];
    let mesh = meshopt::ClodMesh {
        vertex_attributes: attributes.as_flattened(),
        vertex_attributes_stride: 5 * 4,
        attribute_weights: &weights,
        attribute_protect_mask: 0b11,
        ..meshopt::ClodMesh::new(indices, &adapter)
    };
    let config = meshopt::clod_default_config(CLUSTER_TRIANGLES);

    // Per group, its simplified bounds; per cluster, (own group, refined, indices).
    let mut groups: Vec<meshopt::ClodBounds> = Vec::new();
    let mut clusters: Vec<(usize, i32, Vec<u32>)> = Vec::new();
    meshopt::clod_build(config, &mesh, |group, out| {
        let id = groups.len();
        groups.push(group.simplified);
        for cluster in out {
            clusters.push((id, cluster.refined, cluster.indices.to_vec()));
        }
        id as i32
    });

    let laid_out = lay_out(&positions, &groups, &clusters);
    if laid_out.len() > clusters_capacity(indices.len() as u32) as usize {
        return runs();
    }
    laid_out
}

/// Header, records and indices for `clusters`, each (own group, refined
/// group or -1, indices) with `groups` holding each group's simplified
/// bounds.
fn lay_out(
    positions: &[[f32; 3]],
    groups: &[meshopt::ClodBounds],
    clusters: &[(usize, i32, Vec<u32>)],
) -> Vec<u32> {
    let c = clusters.len() as u32;
    let index_words: usize = clusters.iter().map(|(_, _, idx)| idx.len()).sum();
    let records_end = (CLUSTER_HEADER_WORDS + CLUSTER_RECORD_WORDS * c) as usize;
    let total = records_end + index_words;

    let mut out = Vec::with_capacity(total);
    out.resize(CLUSTER_HEADER_WORDS as usize, 0);
    out[0] = c;
    out[1] = CLUSTER_HEADER_WORDS;
    out[2] = records_end as u32;
    out[3] = index_words as u32;
    out[4] = c.min(DISPATCH_X_MAX);
    out[5] = c.div_ceil(DISPATCH_X_MAX);
    out[6] = 1;

    let mut first = records_end as u32;
    for (own, refined, idx) in clusters {
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for &i in idx {
            let p = positions[i as usize];
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let (axis, sine) = normal_cone(positions, idx);
        let own_b = groups[*own];
        let (ref_c, ref_r, ref_e) = if *refined < 0 {
            ([0.0; 3], 0.0, -1.0)
        } else {
            let b = groups[*refined as usize];
            (b.center, b.radius, b.error)
        };
        let record = [
            lo[0].to_bits(),
            lo[1].to_bits(),
            lo[2].to_bits(),
            first,
            hi[0].to_bits(),
            hi[1].to_bits(),
            hi[2].to_bits(),
            idx.len() as u32,
            axis[0].to_bits(),
            axis[1].to_bits(),
            axis[2].to_bits(),
            sine.to_bits(),
            own_b.center[0].to_bits(),
            own_b.center[1].to_bits(),
            own_b.center[2].to_bits(),
            own_b.radius.to_bits(),
            own_b.error.to_bits(),
            ref_c[0].to_bits(),
            ref_c[1].to_bits(),
            ref_c[2].to_bits(),
            ref_r.to_bits(),
            ref_e.to_bits(),
            0,
            0,
        ];
        out.extend_from_slice(&record);
        first += idx.len() as u32;
    }
    for (_, _, idx) in clusters {
        out.extend_from_slice(idx);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_hierarchy_is_consistent() {
        const N: usize = 129;
        let mut vertices = Vec::with_capacity(N * N);
        for y in 0..N {
            for x in 0..N {
                let (fx, fy) = (x as f32 / (N - 1) as f32, y as f32 / (N - 1) as f32);
                let h = 0.2 * (-((fx - 0.5).powi(2) + (fy - 0.5).powi(2)) * 8.0).exp();
                let mut v: MeshVertex = bytemuck::Zeroable::zeroed();
                v.position = [fx * 10.0, h, fy * 10.0];
                v.normal = [0.0, 1.0, 0.0];
                v.uv = [fx, fy];
                vertices.push(v);
            }
        }
        let mut indices = Vec::new();
        for y in 0..N - 1 {
            for x in 0..N - 1 {
                let a = (y * N + x) as u32;
                let (b, c, d) = (a + 1, a + N as u32, a + N as u32 + 1);
                indices.extend_from_slice(&[a, c, b, b, c, d]);
            }
        }
        let triangles = indices.len() / 3;
        assert_eq!(triangles, 128 * 128 * 2);

        let words = build_clusters(&vertices, &indices);
        let c = words[0];
        assert!(c > 8, "clusters: {c}");
        assert_eq!(words[1], CLUSTER_HEADER_WORDS);
        assert_eq!(words[2], CLUSTER_HEADER_WORDS + CLUSTER_RECORD_WORDS * c);
        assert_eq!(words[4], c.min(32768));
        assert_eq!(words[5], c.div_ceil(32768));
        assert_eq!(words[6], 1);
        assert_eq!(words[3] as usize, words.len() - words[2] as usize);
        assert!(words.len() <= clusters_capacity(indices.len() as u32) as usize);

        let mut original = 0usize;
        let mut coarser = 0usize;
        for k in 0..c as usize {
            let r = &words[(CLUSTER_HEADER_WORDS as usize + CLUSTER_RECORD_WORDS as usize * k)..];
            let (first, count) = (r[3] as usize, r[7] as usize);
            assert!(first >= words[2] as usize && first + count <= words.len());
            assert_eq!(count % 3, 0);
            assert!(count <= 3 * CLUSTER_TRIANGLES);
            assert!(words[first..first + count].iter().all(|&i| (i as usize) < vertices.len()));
            let refined_error = f32::from_bits(r[21]);
            if refined_error == -1.0 {
                original += count / 3;
            } else if refined_error >= 0.0 {
                coarser += 1;
            }
        }
        assert_eq!(original, triangles);
        assert!(coarser > 0);
    }
}
