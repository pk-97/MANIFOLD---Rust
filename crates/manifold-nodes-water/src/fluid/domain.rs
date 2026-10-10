//! Native solver mapping for shared scene-space fluid geometry.

#[cfg(feature = "gpu-proofs")]
use manifold_fluids::{Bounds, Config};

use manifold_node_engine::scene::fluid_domain::FluidDomainLayout;
#[cfg(feature = "gpu-proofs")]
use super::FluidSettings;
#[cfg(feature = "gpu-proofs")]
use manifold_node_engine::scene::fluid_domain::domain_layout;
use manifold_node_engine::scene::transform::Transform;

#[cfg(feature = "gpu-proofs")]
impl FluidSettings {
    pub fn domain_layout(self) -> Result<FluidDomainLayout, String> {
        domain_layout(self.domain, self.domain_size, self.resolution)
    }
}

manifold_core::testkit_visible! {
/// Native solver conversions for the shared scene-space domain layout.
pub(crate) trait FluidDomainNative {
    #[cfg(feature = "gpu-proofs")]
    /// Convert shared authored geometry into the native solver config.
    fn config(self, settings: FluidSettings) -> Config;
    #[cfg(feature = "gpu-proofs")]
    fn to_native(self, point: [f32; 3]) -> [f32; 3];
    #[cfg(feature = "gpu-proofs")]
    fn to_scene(self, point: [f32; 3]) -> [f32; 3];
    #[cfg(any(test, feature = "gpu-proofs"))]
    /// Native origin including the solver's boundary padding.
    fn native_origin(self) -> [f32; 3];
    #[cfg(any(test, feature = "gpu-proofs"))]
    /// Scene box and node counts of the FLIP solid lattice.
    /// The padded native grid uses node `(i, j, k)` at
    /// `min + (i, j, k)·size/(nodes − 1)`.
    fn solid_lattice(self) -> (Transform, [u32; 3]);
    #[cfg(feature = "gpu-proofs")]
    /// Refuse grids exceeding the configured native budget.
    /// The padded grid over `budget_mcells` million cells is refused by name.
    fn admit_flip_grid(self, budget_mcells: f32) -> Result<(), String>;
    #[cfg(feature = "gpu-proofs")]
    fn bounds(self, pose: Transform) -> Bounds;
}
}

#[cfg(any(test, feature = "gpu-proofs"))]
impl FluidDomainNative for FluidDomainLayout {
    #[cfg(feature = "gpu-proofs")]
    fn config(self, settings: FluidSettings) -> Config {
        Config {
            // FLIP reserves 1.5 cells at each closed boundary. They belong
            // outside the authored domain, not inside its initial fill.
            cells: self.cells.map(|n| n + 3),
            cell_size: self.cell_size,
            surface_subdivisions: settings.surface_subdivisions,
            apic: settings.apic,
        }
    }

    #[cfg(feature = "gpu-proofs")]
    fn to_native(self, point: [f32; 3]) -> [f32; 3] {
        let origin = self.native_origin();
        std::array::from_fn(|i| point[i] - origin[i])
    }

    #[cfg(feature = "gpu-proofs")]
    fn to_scene(self, point: [f32; 3]) -> [f32; 3] {
        let origin = self.native_origin();
        std::array::from_fn(|i| point[i] + origin[i])
    }

    fn native_origin(self) -> [f32; 3] {
        self.min.map(|value| value - (1.5 * self.cell_size) as f32)
    }

    fn solid_lattice(self) -> (Transform, [u32; 3]) {
        let origin = self.native_origin();
        let size: [f32; 3] = std::array::from_fn(|axis| (f64::from(self.cells[axis] + 3) * self.cell_size) as f32);
        let bounds = Transform {
            pos: std::array::from_fn(|axis| origin[axis] + size[axis] * 0.5),
            scale: size,
            ..Transform::default()
        };
        (bounds, self.cells.map(|cells| cells + 4))
    }

    #[cfg(feature = "gpu-proofs")]
    fn admit_flip_grid(self, budget_mcells: f32) -> Result<(), String> {
        let cells = self.cells.into_iter().map(|n| u64::from(n) + 3).product::<u64>();
        if !budget_mcells.is_finite() || budget_mcells <= 0.0 || cells as f64 > f64::from(budget_mcells) * 1e6 {
            return Err(format!(
                "Fluid grid needs {:.3} million cells including boundary padding; Grid Budget is {budget_mcells:.3} million. Increase Grid Budget or lower Resolution. CPU time and memory grow with cell count.",
                cells as f64 / 1e6,
            ));
        }
        Ok(())
    }

    #[cfg(feature = "gpu-proofs")]
    fn bounds(self, pose: Transform) -> Bounds {
        let centre = self.to_native(pose.pos);
        Bounds {
            min: std::array::from_fn(|i| centre[i] - pose.scale[i] * 0.5),
            max: std::array::from_fn(|i| centre[i] + pose.scale[i] * 0.5),
        }
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
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
        let high_res = FluidSettings {
            resolution: 128,
            ..FluidSettings::default()
        };
        high_res.validate().unwrap();
        assert_eq!(high_res.domain_layout().unwrap().cells, [128; 3]);

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
