//! Allocation free CPU geometry for clipping a reference triangle by planes.
//!
//! The returned vertices retain their position in the reference triangle as
//! barycentric coordinates. Callers can therefore reconstruct all of their
//! vertex attributes without introducing a second, independently generated
//! topology.

const MAX_PLANES: usize = 6;
const MAX_VERTICES: usize = 3 + MAX_PLANES;

/// A vertex generated from a reference triangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CutVertex {
    /// Weights for the reference triangle's vertices, in order.
    pub barycentric: [f32; 3],
}

/// Errors returned before or during clipping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutError {
    /// More than six clipping planes were supplied.
    TooManyPlanes,
    /// A reference vertex or plane coefficient was not finite.
    NonFiniteInput,
    /// A plane has no normal and therefore does not define a halfspace.
    InvalidPlane,
    /// The fixed internal polygon buffer was exceeded.
    ScratchOverflow,
}

#[derive(Clone, Copy)]
struct ScratchVertex {
    position: [f32; 3],
    barycentric: [f32; 3],
}

/// Clip `reference` against all supplied halfspaces and append its triangles
/// to `output`.
///
/// A plane contains a point when `dot(plane.xyz, point) + plane.w >= 0`.
/// The output is a deterministic, winding-preserving fan of the clipped
/// convex polygon. Empty intersections append no triangles. Scratch storage
/// is fixed-size; only the caller-owned output vector may allocate.
pub fn clip_triangle_to_planes(
    reference: [[f32; 3]; 3],
    planes: &[[f32; 4]],
    output: &mut Vec<[CutVertex; 3]>,
) -> Result<(), CutError> {
    if planes.len() > MAX_PLANES {
        return Err(CutError::TooManyPlanes);
    }
    if reference.iter().flatten().any(|value| !value.is_finite()) {
        return Err(CutError::NonFiniteInput);
    }
    for plane in planes {
        if plane.iter().any(|value| !value.is_finite()) {
            return Err(CutError::NonFiniteInput);
        }
        if plane[0] == 0.0 && plane[1] == 0.0 && plane[2] == 0.0 {
            return Err(CutError::InvalidPlane);
        }
    }

    let mut polygon_a = [
        ScratchVertex {
            position: reference[0],
            barycentric: [1.0, 0.0, 0.0],
        },
        ScratchVertex {
            position: reference[1],
            barycentric: [0.0, 1.0, 0.0],
        },
        ScratchVertex {
            position: reference[2],
            barycentric: [0.0, 0.0, 1.0],
        },
        ScratchVertex {
            position: [0.0; 3],
            barycentric: [0.0; 3],
        },
        ScratchVertex {
            position: [0.0; 3],
            barycentric: [0.0; 3],
        },
        ScratchVertex {
            position: [0.0; 3],
            barycentric: [0.0; 3],
        },
        ScratchVertex {
            position: [0.0; 3],
            barycentric: [0.0; 3],
        },
        ScratchVertex {
            position: [0.0; 3],
            barycentric: [0.0; 3],
        },
        ScratchVertex {
            position: [0.0; 3],
            barycentric: [0.0; 3],
        },
    ];
    let mut polygon_b = [polygon_a[0]; MAX_VERTICES];
    let mut polygon_len = 3;

    for plane in planes {
        let mut next_len = 0;
        for index in 0..polygon_len {
            let previous = polygon_a[(index + polygon_len - 1) % polygon_len];
            let current = polygon_a[index];
            let previous_distance = distance(*plane, previous.position)?;
            let current_distance = distance(*plane, current.position)?;
            let previous_inside = previous_distance >= 0.0;
            let current_inside = current_distance >= 0.0;

            if previous_inside != current_inside {
                let denominator = previous_distance as f64 - current_distance as f64;
                if denominator == 0.0 || !denominator.is_finite() {
                    return Err(CutError::NonFiniteInput);
                }
                let t = (previous_distance as f64 / denominator) as f32;
                if !t.is_finite() {
                    return Err(CutError::NonFiniteInput);
                }
                let intersection = if t <= 0.0 {
                    previous
                } else if t >= 1.0 {
                    current
                } else {
                    interpolate(previous, current, t)?
                };
                push_unique(&mut polygon_b, &mut next_len, intersection)?;
            }
            if current_inside {
                push_unique(&mut polygon_b, &mut next_len, current)?;
            }
        }
        if next_len > 1 && same_vertex(polygon_b[0], polygon_b[next_len - 1]) {
            next_len -= 1;
        }
        polygon_len = next_len;
        std::mem::swap(&mut polygon_a, &mut polygon_b);
        if polygon_len < 3 {
            break;
        }
    }

    if polygon_len < 3 {
        return Ok(());
    }
    for vertex in &polygon_a[..polygon_len] {
        if !vertex.position.iter().all(|value| value.is_finite())
            || !vertex.barycentric.iter().all(|value| value.is_finite())
        {
            return Err(CutError::NonFiniteInput);
        }
    }
    for index in 1..polygon_len - 1 {
        output.push([
            CutVertex {
                barycentric: polygon_a[0].barycentric,
            },
            CutVertex {
                barycentric: polygon_a[index].barycentric,
            },
            CutVertex {
                barycentric: polygon_a[index + 1].barycentric,
            },
        ]);
    }
    Ok(())
}

fn distance(plane: [f32; 4], position: [f32; 3]) -> Result<f32, CutError> {
    let value = plane[0] * position[0] + plane[1] * position[1] + plane[2] * position[2] + plane[3];
    value
        .is_finite()
        .then_some(value)
        .ok_or(CutError::NonFiniteInput)
}

fn interpolate(
    previous: ScratchVertex,
    current: ScratchVertex,
    t: f32,
) -> Result<ScratchVertex, CutError> {
    let mut position = [0.0; 3];
    let mut barycentric = [0.0; 3];
    for index in 0..3 {
        position[index] = (previous.position[index] as f64 * (1.0 - t as f64)
            + current.position[index] as f64 * t as f64) as f32;
        barycentric[index] = (previous.barycentric[index] as f64 * (1.0 - t as f64)
            + current.barycentric[index] as f64 * t as f64) as f32;
    }
    let result = ScratchVertex {
        position,
        barycentric,
    };
    if result.position.iter().all(|value| value.is_finite())
        && result.barycentric.iter().all(|value| value.is_finite())
    {
        Ok(result)
    } else {
        Err(CutError::NonFiniteInput)
    }
}

fn same_vertex(left: ScratchVertex, right: ScratchVertex) -> bool {
    left.barycentric == right.barycentric
}

fn push_unique(
    polygon: &mut [ScratchVertex; MAX_VERTICES],
    length: &mut usize,
    vertex: ScratchVertex,
) -> Result<(), CutError> {
    if *length > 0 && same_vertex(polygon[*length - 1], vertex) {
        return Ok(());
    }
    if *length == polygon.len() {
        return Err(CutError::ScratchOverflow);
    }
    polygon[*length] = vertex;
    *length += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIANGLE: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];

    fn clipped(reference: [[f32; 3]; 3], planes: &[[f32; 4]]) -> Vec<[CutVertex; 3]> {
        let mut output = Vec::new();
        clip_triangle_to_planes(reference, planes, &mut output).unwrap();
        output
    }

    fn point(reference: [[f32; 3]; 3], vertex: CutVertex) -> [f32; 3] {
        let mut result = [0.0; 3];
        for (weight, corner) in vertex.barycentric.iter().zip(reference) {
            for axis in 0..3 {
                result[axis] += weight * corner[axis];
            }
        }
        result
    }

    fn signed_area_xy(triangle: [CutVertex; 3], reference: [[f32; 3]; 3]) -> f32 {
        let a = point(reference, triangle[0]);
        let b = point(reference, triangle[1]);
        let c = point(reference, triangle[2]);
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    }

    fn total_area(reference: [[f32; 3]; 3], triangles: &[[CutVertex; 3]]) -> f32 {
        triangles
            .iter()
            .map(|triangle| signed_area_xy(*triangle, reference).abs() * 0.5)
            .sum()
    }

    #[test]
    fn uncut_identity_preserves_corners_exactly() {
        let output = clipped(TRIANGLE, &[]);
        assert_eq!(output.len(), 1);
        assert_eq!(output[0][0].barycentric, [1.0, 0.0, 0.0]);
        assert_eq!(output[0][1].barycentric, [0.0, 1.0, 0.0]);
        assert_eq!(output[0][2].barycentric, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn plane_through_edge_and_vertex_has_no_duplicate_vertices() {
        let edge = clipped(TRIANGLE, &[[1.0, 0.0, 0.0, 0.0]]);
        assert_eq!(edge.len(), 1);
        assert_eq!(edge[0][0].barycentric, [1.0, 0.0, 0.0]);
        assert_eq!(edge[0][1].barycentric, [0.0, 1.0, 0.0]);
        assert_eq!(edge[0][2].barycentric, [0.0, 0.0, 1.0]);

        let vertex = clipped(TRIANGLE, &[[1.0, 1.0, 0.0, 0.0]]);
        assert_eq!(vertex.len(), 1);
        assert!((total_area(TRIANGLE, &vertex) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn complementary_halfspaces_reconstruct_original_area_without_overlap() {
        let left = clipped(TRIANGLE, &[[1.0, 0.0, 0.0, -0.5]]);
        let right = clipped(TRIANGLE, &[[-1.0, 0.0, 0.0, 0.5]]);
        assert!((total_area(TRIANGLE, &left) + total_area(TRIANGLE, &right) - 0.5).abs() < 1e-6);
        assert!((total_area(TRIANGLE, &left) - 0.125).abs() < 1e-6);
        assert!((total_area(TRIANGLE, &right) - 0.375).abs() < 1e-6);
    }

    #[test]
    fn clipping_preserves_winding() {
        let output = clipped(TRIANGLE, &[[1.0, 1.0, 0.0, -0.25]]);
        assert!(!output.is_empty());
        assert!(output
            .iter()
            .all(|triangle| signed_area_xy(*triangle, TRIANGLE) > 0.0));
    }

    #[test]
    fn multiple_planes_clip_to_box() {
        let reference = [[-2.0, -2.0, 0.0], [2.0, -2.0, 0.0], [0.0, 2.0, 0.0]];
        let output = clipped(
            reference,
            &[
                [1.0, 0.0, 0.0, 1.0],
                [-1.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 1.0],
                [0.0, -1.0, 0.0, 1.0],
            ],
        );
        assert_eq!(output.len(), 4);
        for triangle in output {
            for vertex in triangle {
                let position = point(reference, vertex);
                assert!((-1.0 - 1e-6..=1.0 + 1e-6).contains(&position[0]));
                assert!((-1.0 - 1e-6..=1.0 + 1e-6).contains(&position[1]));
            }
        }
    }

    #[test]
    fn barycentrics_reconstruct_cut_vertices() {
        let output = clipped(TRIANGLE, &[[1.0, 1.0, 0.0, -0.75]]);
        for triangle in output {
            for vertex in triangle {
                let weights = vertex.barycentric;
                assert!((weights.iter().sum::<f32>() - 1.0).abs() < 1e-6);
                assert!(weights.iter().all(|weight| *weight >= -1e-6));
                let position = point(TRIANGLE, vertex);
                let on_boundary = (position[0] + position[1] - 0.75).abs() < 1e-6;
                let is_corner = ((position[0] - 1.0).abs() < 1e-6 && position[1].abs() < 1e-6)
                    || (position[0].abs() < 1e-6 && (position[1] - 1.0).abs() < 1e-6);
                assert!(on_boundary || is_corner);
            }
        }
    }

    #[test]
    fn large_finite_edges_produce_finite_intersections() {
        let maximum = f32::MAX;
        let reference = [[maximum, 0.0, 0.0], [-maximum, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let output = clipped(reference, &[[1.0, 0.0, 0.0, 0.0]]);
        assert_eq!(output.len(), 1);
        for vertex in output[0] {
            assert!(vertex.barycentric.iter().all(|value| value.is_finite()));
            let position = point(reference, vertex);
            assert!(position.iter().all(|value| value.is_finite()));
        }
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        let mut output = Vec::new();
        assert_eq!(
            clip_triangle_to_planes([[f32::NAN, 0.0, 0.0]; 3], &[], &mut output),
            Err(CutError::NonFiniteInput)
        );
        assert_eq!(
            clip_triangle_to_planes(TRIANGLE, &[[0.0, 0.0, 0.0, 1.0]], &mut output),
            Err(CutError::InvalidPlane)
        );
        let planes = [[1.0, 0.0, 0.0, 0.0]; MAX_PLANES + 1];
        assert_eq!(
            clip_triangle_to_planes(TRIANGLE, &planes, &mut output),
            Err(CutError::TooManyPlanes)
        );
    }
}
