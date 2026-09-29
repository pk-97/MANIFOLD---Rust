//! One scene/native mapping for simulation, roles, whitewater and editor bounds.
use manifold_fluids::{Bounds, Config};

use super::{FluidSettings, Transform};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidDomainLayout {
    pub min: [f32; 3],
    pub size: [f32; 3],
    pub cells: [u32; 3],
    pub cell_size: f64,
}

impl FluidSettings {
    pub fn domain_layout(self) -> Result<FluidDomainLayout, String> {
        domain_layout(self.domain, self.domain_size, self.resolution)
    }
}

/// The scene box and uniform cells of a liquid domain, shared by every
/// liquid solver so they agree on cell size (GPU_MPM_SOLVER_DESIGN.md D17).
/// `domain` is the authored axis-aligned box (scale = full size); without
/// one, a `domain_size` cube centred in X/Z with its floor at Y = 0.
pub fn domain_layout(
    domain: Option<Transform>,
    domain_size: f32,
    resolution: u32,
) -> Result<FluidDomainLayout, String> {
    if resolution < 8 {
        return Err("Fluid: resolution must be at least 8 cells".into());
    }
    let pose = domain.unwrap_or(Transform {
        pos: [0.0, domain_size * 0.5, 0.0],
        scale: [domain_size; 3],
        ..Transform::default()
    });
    if pose.billboard
        || pose
            .rot_euler
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1e-6)
    {
        return Err("Fluid: the domain must be axis-aligned; rotation and billboarding are not supported".into());
    }
    if pose.pos.iter().any(|v| !v.is_finite())
        || pose
            .scale
            .iter()
            .any(|v| !v.is_finite() || !(0.5..=20.0).contains(v))
    {
        return Err("Fluid: domain position must be finite and each dimension must be between 0.5 and 20 metres".into());
    }
    let shortest = pose.scale.into_iter().fold(f32::INFINITY, f32::min) as f64;
    let longest = pose.scale.into_iter().fold(0.0_f32, f32::max) as f64;
    let cell_size = (longest / f64::from(resolution)).min(shortest / 8.0);
    let cells: [u32; 3] = std::array::from_fn(|i| {
        let ratio = f64::from(pose.scale[i]) / cell_size;
        // Division can place an exact grid multiple a few ULPs above its
        // integer. Do not add a whole cell for that arithmetic roundoff.
        let count = if (ratio - ratio.round()).abs() < 1e-9 {
            ratio.round()
        } else {
            ratio.ceil()
        };
        count as u32
    });
    if cells.iter().any(|n| *n < 8)
        || cells.iter().try_fold(1u64, |total, n| total.checked_mul(u64::from(*n) + 4))
            .is_none_or(|total| total > i32::MAX as u64) {
        return Err("Fluid: padded grid exceeds the native solver's 32-bit grid indexing".into());
    }
    let size = cells.map(|n| (f64::from(n) * cell_size) as f32);
    let min = std::array::from_fn(|i| pose.pos[i] - size[i] * 0.5);
    if min
        .iter()
        .any(|v| !v.is_finite() || *v + cell_size as f32 <= *v)
    {
        return Err(
            "Fluid: domain position is too far from the scene origin for its cell size".into(),
        );
    }
    Ok(FluidDomainLayout {
        min,
        size,
        cells,
        cell_size,
    })
}

impl FluidDomainLayout {
    pub fn transform(self) -> Transform {
        Transform {
            pos: std::array::from_fn(|i| self.min[i] + self.size[i] * 0.5),
            scale: self.size,
            ..Transform::default()
        }
    }

    pub(super) fn config(self, settings: FluidSettings) -> Config {
        Config {
            // FLIP reserves 1.5 cells at each closed boundary. They belong
            // outside the authored domain, not inside its initial fill.
            cells: self.cells.map(|n| n + 3),
            cell_size: self.cell_size,
            surface_subdivisions: settings.surface_subdivisions,
            apic: settings.apic,
        }
    }

    pub(super) fn to_native(self, point: [f32; 3]) -> [f32; 3] {
        let origin = self.native_origin();
        std::array::from_fn(|i| point[i] - origin[i])
    }

    pub(super) fn to_scene(self, point: [f32; 3]) -> [f32; 3] {
        let origin = self.native_origin();
        std::array::from_fn(|i| point[i] + origin[i])
    }

    pub(super) fn native_origin(self) -> [f32; 3] {
        self.min.map(|value| value - (1.5 * self.cell_size) as f32)
    }

    pub(super) fn bounds(self, pose: Transform) -> Bounds {
        let centre = self.to_native(pose.pos);
        Bounds {
            min: std::array::from_fn(|i| centre[i] - pose.scale[i] * 0.5),
            max: std::array::from_fn(|i| centre[i] + pose.scale[i] * 0.5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_physics_domain_padding_preserves_authored_cube_bounds() {
        let layout = FluidSettings::default().domain_layout().unwrap();
        assert_eq!(layout.min, [-2.0, 0.0, -2.0]);
        assert_eq!(layout.size, [4.0; 3]);
        assert_eq!(layout.cells, [24; 3]);
        assert_eq!(layout.to_native([1.0, 2.0, 3.0]), [3.25, 2.25, 5.25]);
        assert_eq!(layout.config(FluidSettings::default()).cells, [27; 3]);
    }

    #[test]
    fn scene_physics_domain_accepts_resolution_above_old_96_limit() {
        let settings = FluidSettings { resolution: 128, ..FluidSettings::default() };
        settings.validate().unwrap();
        assert_eq!(settings.domain_layout().unwrap().cells, [128; 3]);
    }

    #[test]
    fn scene_physics_domain_rectangular_grid_is_uniform_and_snaps_outward() {
        let pose = Transform {
            pos: [5.0, -1.0, 2.0],
            scale: [6.0, 2.1, 1.0],
            ..Transform::default()
        };
        let layout = FluidSettings {
            domain: Some(pose),
            ..FluidSettings::default()
        }
        .domain_layout()
        .unwrap();
        assert_eq!(layout.cells, [48, 17, 8]);
        assert_eq!(layout.cell_size, 0.125);
        assert_eq!(layout.transform().pos, pose.pos);
        for i in 0..3 {
            assert!(layout.size[i] >= pose.scale[i]);
            assert!(layout.size[i] - pose.scale[i] < layout.cell_size as f32);
        }
        assert_eq!(
            layout.to_scene(layout.to_native([4.0, -1.0, 2.0])),
            [4.0, -1.0, 2.0]
        );
    }

    #[test]
    fn scene_physics_domain_thin_grid_keeps_eight_cells() {
        let settings = FluidSettings {
            domain: Some(Transform {
                scale: [20.0, 0.5, 20.0],
                ..Transform::default()
            }),
            ..FluidSettings::default()
        };
        assert_eq!(settings.domain_layout().unwrap().cells, [320, 8, 320]);
    }

    #[test]
    fn scene_physics_domain_rejects_invalid_geometry_and_validates_local_fill() {
        let domain = Transform {
            pos: [10.0, -5.0, 3.0],
            scale: [6.0, 2.0, 1.0],
            ..Transform::default()
        };
        let mut settings = FluidSettings {
            domain: Some(domain),
            ..FluidSettings::default()
        };
        settings.initial_volume = Some(Transform {
            pos: domain.pos,
            scale: [1.0, 1.0, 0.5],
            ..Transform::default()
        });
        settings.validate().unwrap();
        settings.initial_volume.as_mut().unwrap().pos[1] += 2.0;
        assert!(settings.validate().unwrap_err().contains("contained"));
        for invalid in [
            Transform {
                rot_euler: [0.0, 1.0, 0.0],
                ..domain
            },
            Transform {
                billboard: true,
                ..domain
            },
            Transform {
                pos: [f32::NAN, 0.0, 0.0],
                ..domain
            },
            Transform {
                pos: [f32::MAX, 0.0, 0.0],
                ..domain
            },
            Transform {
                scale: [0.0, 2.0, 2.0],
                ..domain
            },
        ] {
            assert!(
                FluidSettings {
                    domain: Some(invalid),
                    ..FluidSettings::default()
                }
                .domain_layout()
                .is_err()
            );
        }
    }
}
