//! Lossless friendly decomposition of the six scalar UV affine values.

const SINGULAR_EPSILON: f64 = 1.0e-8;
const SHEAR_EPSILON: f64 = 1.0e-6;

/// A friendly placement view of one UV affine transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UvDecomposition {
    pub offset: [f32; 2],
    pub rotation_radians: f32,
    pub scale: [f32; 2],
}

/// Decompose `[m00, m01, m10, m11, tx, ty]` when its linear part is
/// `R(rotation_radians) * diag(scale)`. Shear, singular axes, and non-finite
/// values return `None`; the original six values remain available to callers.
pub fn decompose_uv_affine(values: [f32; 6]) -> Option<UvDecomposition> {
    if values.iter().any(|value| !value.is_finite()) {
        return None;
    }

    let [m00, m01, m10, m11, tx, ty] = values.map(f64::from);
    let sx = m00.hypot(m10);
    let sy_abs = m01.hypot(m11);
    if sx <= SINGULAR_EPSILON || sy_abs <= SINGULAR_EPSILON {
        return None;
    }

    let dot = m00.mul_add(m01, m10 * m11);
    if dot.abs() > SHEAR_EPSILON * sx * sy_abs {
        return None;
    }

    let determinant = m00.mul_add(m11, -(m01 * m10));
    let sy = if determinant < 0.0 { -sy_abs } else { sy_abs };
    Some(UvDecomposition {
        offset: [tx as f32, ty as f32],
        rotation_radians: m10.atan2(m00) as f32,
        scale: [sx as f32, sy as f32],
    })
}

/// Compose `[m00, m01, m10, m11, tx, ty]` from a friendly decomposition.
pub fn compose_uv_affine(decomposition: UvDecomposition) -> [f32; 6] {
    let (sin, cos) = f64::from(decomposition.rotation_radians).sin_cos();
    let sx = f64::from(decomposition.scale[0]);
    let sy = f64::from(decomposition.scale[1]);
    [
        (cos * sx) as f32,
        (-sin * sy) as f32,
        (sin * sx) as f32,
        (cos * sy) as f32,
        decomposition.offset[0],
        decomposition.offset[1],
    ]
}

#[cfg(test)]
mod tests {
    use super::{UvDecomposition, compose_uv_affine, decompose_uv_affine};

    fn assert_values_close(actual: [f32; 6], expected: [f32; 6]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= 1.0e-6,
                "{actual} != {expected}"
            );
        }
    }

    #[test]
    fn material_inspector_uv_identity_round_trips_without_mutating_input() {
        let values = [1.0, 0.0, 0.0, 1.0, 0.25, -0.5];
        let decomposition = decompose_uv_affine(values).expect("identity is decomposable");
        assert_eq!(decomposition.offset, [0.25, -0.5]);
        assert_eq!(decomposition.scale, [1.0, 1.0]);
        assert_values_close(values, [1.0, 0.0, 0.0, 1.0, 0.25, -0.5]);
        assert_values_close(compose_uv_affine(decomposition), values);
    }

    #[test]
    fn material_inspector_uv_rotation_and_imported_matrix_round_trip() {
        let decomposition = UvDecomposition {
            offset: [1.25, -2.0],
            rotation_radians: 0.37,
            scale: [2.5, 0.75],
        };
        let values = compose_uv_affine(decomposition);
        let recovered = decompose_uv_affine(values).expect("rotation and scale are decomposable");
        assert_values_close(compose_uv_affine(recovered), values);
        assert_eq!(recovered.offset, decomposition.offset);
        assert!((recovered.rotation_radians - decomposition.rotation_radians).abs() <= 1.0e-6);
        assert!((recovered.scale[0] - decomposition.scale[0]).abs() <= 1.0e-6);
        assert!((recovered.scale[1] - decomposition.scale[1]).abs() <= 1.0e-6);
    }

    #[test]
    fn material_inspector_uv_reflection_is_representable() {
        let values = [-1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let decomposition = decompose_uv_affine(values).expect("reflection is decomposable");
        assert_values_close(compose_uv_affine(decomposition), values);
        assert!(decomposition.scale[1] < 0.0);
    }

    #[test]
    fn material_inspector_uv_shear_singular_and_nonfinite_are_advanced_only() {
        assert!(decompose_uv_affine([1.0, 0.25, 0.0, 1.0, 0.0, 0.0]).is_none());
        assert!(decompose_uv_affine([1.0, 0.0, 0.0, 0.0, 0.0, 0.0]).is_none());
        assert!(decompose_uv_affine([f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0]).is_none());
    }
}
