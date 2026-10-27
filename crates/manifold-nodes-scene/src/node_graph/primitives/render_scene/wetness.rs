//! The wetness look's uniforms: where the water's wetness field sits, so every
//! mesh shader can read it by world position (`look_wet_surface.wgsl`).
use manifold_node_engine::scene::world_tiles::table_entries;

use super::{FieldObject, mat4_inverse};

/// Field for field with `WetUniforms` in `look_wet_surface.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct WetUniforms {
    world_to_local: [[f32; 4]; 4],
    /// x pool tiles, y table entries, z 1 when a wetness field is bound.
    tiles: [u32; 4],
    /// x display cell size in metres.
    cell: [f32; 4],
}

/// Uniforms for the scene's wetness field: all zeros (the shader's off value)
/// for `None`, else the field's table size, cell and place in the water's
/// object space.
pub(super) fn wet_uniforms(field: Option<&FieldObject>) -> Result<WetUniforms, &'static str> {
    match field {
        None => Ok(bytemuck::Zeroable::zeroed()),
        Some(field) => wet_field(field.pool_tiles, field.cell_size, field.model),
    }
}

fn wet_field(
    pool_tiles: u32,
    cell_size: f32,
    model: [[f32; 4]; 4],
) -> Result<WetUniforms, &'static str> {
    let world_to_local = mat4_inverse(model).ok_or("Wetness: singular water model matrix")?;
    if !(cell_size > 0.0 && cell_size.is_finite()) || pool_tiles == 0 {
        return Err("Wetness: the field needs a cell size above zero and a pool of tiles");
    }
    Ok(WetUniforms {
        world_to_local,
        tiles: [pool_tiles, table_entries(pool_tiles), 1, 0],
        cell: [cell_size, 0.0, 0.0, 0.0],
    })
}

#[cfg(test)]
mod tests {
    use super::super::model_matrix;
    use super::*;
    use manifold_node_engine::mesh::pipeline::mat4_mul;

    #[test]
    fn wet_uniforms_are_96_bytes() {
        assert_eq!(std::mem::size_of::<WetUniforms>(), 96);
    }

    #[test]
    fn no_field_is_all_zeros() {
        let off = wet_uniforms(None).expect("off is not an error");
        assert!(bytemuck::bytes_of(&off).iter().all(|&b| b == 0));
    }

    #[test]
    fn a_field_gives_its_table_size_cell_and_inverse_model() {
        let model = model_matrix([0.3, 0.2, -0.4], [0.0, 0.35, 0.0], [1.25, 1.25, 1.25]);
        let u = wet_field(37, 0.045, model).expect("valid field");
        assert_eq!(u.tiles, [37, 256, 1, 0]);
        assert_eq!(u.cell, [0.045, 0.0, 0.0, 0.0]);
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
    fn a_singular_model_or_a_field_without_cells_is_an_error() {
        let flat = model_matrix([0.0; 3], [0.0; 3], [1.0, 0.0, 1.0]);
        assert!(wet_field(5, 0.05, flat).is_err());
        let identity = model_matrix([0.0; 3], [0.0; 3], [1.0; 3]);
        assert!(wet_field(5, 0.05, identity).is_ok());
        assert!(wet_field(0, 0.05, identity).is_err());
        assert!(wet_field(5, 0.0, identity).is_err());
        assert!(wet_field(5, f32::NAN, identity).is_err());
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
