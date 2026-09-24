use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Fragment {
    pub(super) vertices: Vec<[f32; 3]>,
    pub(super) triangles: Vec<[u32; 3]>,
    pub(super) triangle_ids: Vec<usize>,
    pub(super) center: [f32; 3],
    pub(super) area: f32,
}

#[derive(Clone, Copy)]
struct TriangleInfo {
    centroid: [f64; 3],
    area: f64,
}

struct Group {
    triangle_ids: Vec<usize>,
}

pub(super) fn partition(
    vertices: &[[f32; 3]],
    triangles: &[[u32; 3]],
    count: usize,
) -> Result<Vec<Fragment>, Box<dyn std::error::Error>> {
    if count == 0 {
        return Err("Fragment count must be nonzero".into());
    }
    if triangles.is_empty() {
        return Err("Cannot partition an empty mesh".into());
    }
    if count > triangles.len() {
        return Err("Fragment count cannot exceed triangle count".into());
    }
    if vertices.iter().flatten().any(|value| !value.is_finite()) {
        return Err("Mesh vertices must be finite".into());
    }

    let mut info = Vec::with_capacity(triangles.len());
    for triangle in triangles {
        let [a, b, c] = *triangle;
        let a = *vertices
            .get(usize::try_from(a)?)
            .ok_or("Triangle index outside vertex array")?;
        let b = *vertices
            .get(usize::try_from(b)?)
            .ok_or("Triangle index outside vertex array")?;
        let c = *vertices
            .get(usize::try_from(c)?)
            .ok_or("Triangle index outside vertex array")?;
        let a = a.map(f64::from);
        let b = b.map(f64::from);
        let c = c.map(f64::from);
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cross = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let area = 0.5
            * (cross[0].mul_add(cross[0], cross[1].mul_add(cross[1], cross[2] * cross[2]))).sqrt();
        if !area.is_finite() || area <= 0.0 {
            return Err("Triangles must have finite, nonzero area".into());
        }
        let centroid = [
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ];
        if centroid.iter().any(|value| !value.is_finite()) {
            return Err("Triangle centroids must be finite".into());
        }
        info.push(TriangleInfo { centroid, area });
    }

    let mut groups = vec![Group {
        triangle_ids: (0..triangles.len()).collect(),
    }];
    while groups.len() < count {
        let group_index = largest_group(&groups);
        let mut group = groups.remove(group_index);
        if group.triangle_ids.len() < 2 {
            return Err("Cannot create the requested nonempty fragments".into());
        }
        let axis = longest_centroid_axis(&group.triangle_ids, &info);
        group.triangle_ids.sort_by(|&left, &right| {
            info[left].centroid[axis]
                .partial_cmp(&info[right].centroid[axis])
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.cmp(&right))
        });
        let right_ids = group.triangle_ids.split_off(group.triangle_ids.len() / 2);
        groups.push(Group {
            triangle_ids: group.triangle_ids,
        });
        groups.push(Group {
            triangle_ids: right_ids,
        });
    }

    groups.sort_by_key(|group| group.triangle_ids[0]);
    groups
        .into_iter()
        .map(|group| make_fragment(&group.triangle_ids, vertices, triangles, &info))
        .collect()
}

fn largest_group(groups: &[Group]) -> usize {
    let mut largest = 0;
    for index in 1..groups.len() {
        let current = &groups[index];
        let candidate = &groups[largest];
        if current.triangle_ids.len() > candidate.triangle_ids.len()
            || (current.triangle_ids.len() == candidate.triangle_ids.len()
                && current.triangle_ids[0] < candidate.triangle_ids[0])
        {
            largest = index;
        }
    }
    largest
}

fn longest_centroid_axis(ids: &[usize], info: &[TriangleInfo]) -> usize {
    let mut minimum = info[ids[0]].centroid;
    let mut maximum = minimum;
    for &id in &ids[1..] {
        for axis in 0..3 {
            minimum[axis] = minimum[axis].min(info[id].centroid[axis]);
            maximum[axis] = maximum[axis].max(info[id].centroid[axis]);
        }
    }
    let extents = [
        maximum[0] - minimum[0],
        maximum[1] - minimum[1],
        maximum[2] - minimum[2],
    ];
    (1..3).fold(0, |longest, axis| {
        if extents[axis] > extents[longest] {
            axis
        } else {
            longest
        }
    })
}

fn make_fragment(
    triangle_ids: &[usize],
    source_vertices: &[[f32; 3]],
    source_triangles: &[[u32; 3]],
    info: &[TriangleInfo],
) -> Result<Fragment, Box<dyn std::error::Error>> {
    let mut vertices = Vec::new();
    let mut triangles = Vec::with_capacity(triangle_ids.len());
    let mut remap = vec![u32::MAX; source_vertices.len()];
    let mut center_sum = [0.0; 3];
    let mut area = 0.0;

    for &triangle_id in triangle_ids {
        let source_triangle = source_triangles[triangle_id];
        let mut triangle = [0; 3];
        for (slot, &source_index) in source_triangle.iter().enumerate() {
            let source_index = usize::try_from(source_index)?;
            if remap[source_index] == u32::MAX {
                remap[source_index] = u32::try_from(vertices.len())?;
                vertices.push(source_vertices[source_index]);
            }
            triangle[slot] = remap[source_index];
        }
        triangles.push(triangle);

        let triangle_info = info[triangle_id];
        area += triangle_info.area;
        for (axis, sum) in center_sum.iter_mut().enumerate() {
            *sum += triangle_info.area * triangle_info.centroid[axis];
        }
    }
    if !area.is_finite() || area <= 0.0 || center_sum.iter().any(|value| !value.is_finite()) {
        return Err("Fragment area and centroid must be finite".into());
    }
    let center = std::array::from_fn(|axis| {
        let value = center_sum[axis] / area;
        value as f32
    });
    let area_f32 = area as f32;
    if center.iter().any(|value| !value.is_finite()) || !area_f32.is_finite() || area_f32 <= 0.0 {
        return Err("Fragment area and centroid must fit in f32".into());
    }

    Ok(Fragment {
        vertices,
        triangles,
        triangle_ids: triangle_ids.to_vec(),
        center,
        area: area_f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERTICES: [[f32; 3]; 8] = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [5.0, 0.0, 0.0],
        [6.0, 0.0, 0.0],
        [6.0, 1.0, 0.0],
        [5.0, 1.0, 0.0],
    ];
    const TRIANGLES: [[u32; 3]; 4] = [[0, 1, 2], [0, 2, 3], [4, 5, 6], [4, 6, 7]];

    #[test]
    fn partitions_with_exact_coverage_and_compacted_vertices() {
        let fragments = partition(&VERTICES, &TRIANGLES, 2).expect("valid mesh");
        assert_eq!(fragments.len(), 2);
        let mut ids: Vec<usize> = fragments
            .iter()
            .flat_map(|fragment| fragment.triangle_ids.iter().copied())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![0, 1, 2, 3]);
        for fragment in &fragments {
            assert_eq!(fragment.triangles.len(), fragment.triangle_ids.len());
            assert!(!fragment.triangles.is_empty());
            assert_eq!(fragment.vertices.len(), 4);
            for (triangle, &source_id) in fragment.triangles.iter().zip(&fragment.triangle_ids) {
                assert!(
                    triangle
                        .iter()
                        .all(|&index| (index as usize) < fragment.vertices.len())
                );
                for corner in 0..3 {
                    assert_eq!(
                        fragment.vertices[triangle[corner] as usize],
                        VERTICES[TRIANGLES[source_id][corner] as usize]
                    );
                }
            }
        }
    }

    #[test]
    fn uses_area_weighted_center_and_is_deterministic() {
        let first = partition(&VERTICES, &TRIANGLES, 4).expect("valid mesh");
        let second = partition(&VERTICES, &TRIANGLES, 4).expect("valid mesh");
        assert_eq!(first, second);
        assert_eq!(first[0].center, [2.0 / 3.0, 1.0 / 3.0, 0.0]);
        assert!((first[0].area - 0.5).abs() < 1.0e-6);
        let unequal = partition(
            &[
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [10.0, 0.0, 0.0],
                [12.0, 0.0, 0.0],
                [10.0, 2.0, 0.0],
            ],
            &[[0, 1, 2], [3, 4, 5]],
            1,
        )
        .unwrap();
        assert_eq!(unequal[0].area, 2.5);
        assert!((unequal[0].center[0] - 8.6).abs() < 1e-6);
        assert!((unequal[0].center[1] - 0.6).abs() < 1e-6);
    }

    #[test]
    fn rejects_invalid_meshes_and_counts() {
        assert!(partition(&VERTICES, &TRIANGLES, 0).is_err());
        assert!(partition(&VERTICES, &TRIANGLES, 5).is_err());
        assert!(partition(&VERTICES, &[], 1).is_err());
        assert!(partition(&VERTICES, &[[0, 1, 9]], 1).is_err());
        assert!(
            partition(
                &[[0.0, f32::NAN, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                &[[0, 1, 2]],
                1
            )
            .is_err()
        );
        assert!(
            partition(
                &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]],
                &[[0, 1, 2]],
                1
            )
            .is_err()
        );
    }
}
