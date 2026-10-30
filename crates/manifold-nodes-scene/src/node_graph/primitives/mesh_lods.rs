//! Detail levels for an imported mesh: coarser index lists over the same
//! vertices, so `node.render_scene` can draw far copies with fewer triangles.
//!
//! The `lods` array is `u32` words. Word 0 is the level count n (0 to
//! `LOD_LEVELS`). For level k < n, words `1+3k`, `2+3k` and `3+3k` hold the
//! word where its indices start, its index count, and its simplification
//! error in the mesh's own units (`f32::to_bits`). The rest of the
//! `LOD_HEADER_WORDS`-word header is zero, and the levels' triangle indices
//! follow it, level 0 first.

/// Detail levels a mesh's `lods` array holds at most, past the full mesh.
pub(crate) const LOD_LEVELS: u32 = 4;
/// Header words at the front of a `lods` array.
pub(crate) const LOD_HEADER_WORDS: u32 = 16;

/// Smallest mesh worth simplifying, in indices.
const MIN_LOD_INDICES: usize = 3 * 64;

/// Words the `lods` array of a mesh of `index_count` indices can need.
pub(crate) fn lods_capacity(index_count: u32) -> u32 {
    index_count / 3 + LOD_HEADER_WORDS
}

/// Detail levels of an indexed mesh, laid out as `lods`.
///
/// Level k aims at a quarter of the previous target and is simplified from
/// the level before it. The chain stops at the first level that barely
/// shrinks, collapses, or would not fit `lods_capacity`.
pub(crate) fn build_lods(positions: &[[f32; 3]], indices: &[u32]) -> Vec<u32> {
    let mut out = vec![0u32; LOD_HEADER_WORDS as usize];
    if indices.len() < MIN_LOD_INDICES || indices.iter().any(|&i| i as usize >= positions.len()) {
        return out;
    }
    let Ok(adapter) = meshopt::VertexDataAdapter::new(
        bytemuck::cast_slice(positions),
        std::mem::size_of::<[f32; 3]>(),
        0,
    ) else {
        return out;
    };
    let capacity = lods_capacity(indices.len() as u32) as usize;
    // simplify reports its error relative to the mesh extent.
    let scale = meshopt::simplify_scale(&adapter);

    let mut prev = indices.to_vec();
    let mut error = 0.0f32;
    let mut levels = 0usize;
    for k in 0..LOD_LEVELS as usize {
        let target = (indices.len() >> (2 * (k + 1))) / 3 * 3;
        let mut step_error = 0.0f32;
        let mut level = meshopt::simplify(
            &prev,
            &adapter,
            target,
            1.0,
            meshopt::SimplifyOptions::empty(),
            Some(&mut step_error),
        );
        if level.len() < 3 || level.len() * 4 > prev.len() * 3 {
            break;
        }
        if out.len() + level.len() > capacity {
            break;
        }
        meshopt::optimize_vertex_cache_in_place(&mut level, positions.len());
        error += step_error * scale;
        let header = 1 + 3 * k;
        out[header] = out.len() as u32;
        out[header + 1] = level.len() as u32;
        out[header + 2] = error.to_bits();
        out.extend_from_slice(&level);
        prev = level;
        levels += 1;
    }
    out[0] = levels as u32;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_levels_shrink_and_stay_laid_out() {
        const N: usize = 65;
        let mut positions = Vec::with_capacity(N * N);
        for y in 0..N {
            for x in 0..N {
                let (fx, fy) = (x as f32 / (N - 1) as f32, y as f32 / (N - 1) as f32);
                let h = 0.2 * (-((fx - 0.5).powi(2) + (fy - 0.5).powi(2)) * 8.0).exp();
                positions.push([fx * 10.0, h, fy * 10.0]);
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
        assert_eq!(indices.len(), 64 * 64 * 6);

        let lods = build_lods(&positions, &indices);
        let n = lods[0] as usize;
        assert!(n >= 2 && n <= LOD_LEVELS as usize, "levels: {n}");
        assert!(lods.len() <= lods_capacity(indices.len() as u32) as usize);

        let mut prev_count = indices.len();
        let mut expected_start = LOD_HEADER_WORDS as usize;
        let mut prev_error = 0.0f32;
        for k in 0..n {
            let start = lods[1 + 3 * k] as usize;
            let count = lods[2 + 3 * k] as usize;
            let error = f32::from_bits(lods[3 + 3 * k]);
            assert_eq!(start, expected_start, "level {k} start");
            assert_eq!(count % 3, 0);
            assert!(
                count * 4 <= prev_count * 3,
                "level {k}: {count} of {prev_count}"
            );
            assert!(
                lods[start..start + count]
                    .iter()
                    .all(|&i| (i as usize) < positions.len())
            );
            assert!(error.is_finite() && error >= 0.0 && error >= prev_error);
            expected_start = start + count;
            prev_count = count;
            prev_error = error;
        }
        assert_eq!(lods.len(), expected_start);
        assert!(
            lods[1 + 3 * n..LOD_HEADER_WORDS as usize]
                .iter()
                .all(|&w| w == 0)
        );
    }
}
