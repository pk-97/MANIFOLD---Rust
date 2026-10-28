//! The wetness look's uniforms: where the water's wet map sits, so every mesh
//! shader can read it by world position (`look_wet_surface.wgsl`). The map's
//! own header says where it lies in the water's space and how wet it is.
use super::{FieldObject, mat4_inverse};

/// Field for field with `WetUniforms` in `look_wet_surface.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct WetUniforms {
    world_to_local: [[f32; 4]; 4],
    /// x 1 when a wet map is bound.
    bound: [u32; 4],
}

/// Uniforms for the scene's wet map: all zeros (the shader's off value) for
/// `None`, else the map's place in the water's object space.
pub(super) fn wet_uniforms(field: Option<&FieldObject>) -> Result<WetUniforms, &'static str> {
    match field {
        None => Ok(bytemuck::Zeroable::zeroed()),
        Some(field) => wet_map_uniforms(field.model),
    }
}

fn wet_map_uniforms(model: [[f32; 4]; 4]) -> Result<WetUniforms, &'static str> {
    let world_to_local = mat4_inverse(model).ok_or("Wetness: singular water model matrix")?;
    Ok(WetUniforms {
        world_to_local,
        bound: [1, 0, 0, 0],
    })
}

#[cfg(test)]
mod tests {
    use super::super::model_matrix;
    use super::*;
    use manifold_node_engine::mesh::pipeline::mat4_mul;

    #[test]
    fn wet_uniforms_are_80_bytes() {
        assert_eq!(std::mem::size_of::<WetUniforms>(), 80);
    }

    #[test]
    fn no_map_is_all_zeros() {
        let off = wet_uniforms(None).expect("off is not an error");
        assert!(bytemuck::bytes_of(&off).iter().all(|&b| b == 0));
    }

    #[test]
    fn a_map_is_bound_with_the_inverse_model() {
        let model = model_matrix([0.3, 0.2, -0.4], [0.0, 0.35, 0.0], [1.25, 1.25, 1.25]);
        let u = wet_map_uniforms(model).expect("valid model");
        assert_eq!(u.bound, [1, 0, 0, 0]);
        let product = mat4_mul(u.world_to_local, model);
        for (c, column) in product.iter().enumerate() {
            for (r, &value) in column.iter().enumerate() {
                let expected = if c == r { 1.0 } else { 0.0 };
                assert!(
                    (value - expected).abs() < 1e-4,
                    "inverse * model [{c}][{r}] = {value}, expected {expected}"
                );
            }
        }
    }

    #[test]
    fn a_singular_model_is_an_error() {
        let flat = model_matrix([0.0; 3], [0.0; 3], [1.0, 0.0, 1.0]);
        assert!(wet_map_uniforms(flat).is_err());
        let identity = model_matrix([0.0; 3], [0.0; 3], [1.0; 3]);
        assert!(wet_map_uniforms(identity).is_ok());
    }

    /// The soaked albedo of `look_wet_surface.wgsl`, in f64.
    fn soaked(albedo: f64) -> f64 {
        let (n, r_e) = (1.333f64, 0.066f64);
        let r_i = 1.0 - (1.0 - r_e) / (n * n);
        (1.0 - r_e) * (1.0 - r_i) * albedo / (1.0 - r_i * albedo)
    }

    /// Lekner and Dorf's own check: a film always darkens, and a darker
    /// surface darkens by more.
    #[test]
    fn wet_albedo_is_darker_and_more_so_for_darker_surfaces() {
        let mut last_ratio = 0.0;
        for i in 1..=100 {
            let albedo = f64::from(i) / 100.0;
            let ratio = soaked(albedo) / albedo;
            assert!(ratio < 1.0, "albedo {albedo} became {}", soaked(albedo));
            assert!(
                ratio > last_ratio,
                "albedo {albedo}: ratio {ratio} not rising"
            );
            last_ratio = ratio;
        }
        assert!(soaked(0.2) / 0.2 < soaked(0.8) / 0.8);
    }
}
