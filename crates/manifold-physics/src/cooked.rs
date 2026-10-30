//! Collision geometry built once for Box3D and shared by every body
//! installed from it: convex hulls cooked by Box3D's hull builder, or a
//! static triangle mesh with its tree. Copies of one prototype install from
//! one cook instead of cooking a hull each.

use std::sync::Arc;

use super::collision_shape::{TOO_THIN, validate_hull, validate_position};
use super::mesh::triangle_mesh_mass_properties;
use super::{PhysicsError, ffi, native_lock};

/// A cooked solid, local to its body and already scaled. Box3D copies a hull
/// into each world that uses it; a mesh is read in place, so every body made
/// from it holds it.
#[derive(Debug)]
pub struct CookedSolid {
    form: CookedForm,
}

#[derive(Debug)]
pub(crate) enum CookedForm {
    /// Native hulls.
    Hulls(Vec<usize>),
    /// A native mesh, with the surface's centre of mass and its inertia at
    /// unit mass (inertia grows with mass).
    Mesh {
        mesh: usize,
        center: [f32; 3],
        unit_inertia: [f32; 9],
    },
    /// A native height field, and where its grid's first point sits in the
    /// body's frame (scaled): the body is placed there.
    HeightField { field: usize, origin: [f32; 3] },
}

impl CookedSolid {
    /// Cook convex hulls, each kept to at most `max_vertex_count` vertices.
    /// Refuses a hull that is not at least 4 valid points, or one the builder
    /// cannot make because it is flat, a line or a point for its size.
    pub(crate) fn hulls(
        hulls: &[&[[f32; 3]]],
        max_vertex_count: i32,
    ) -> Result<Arc<Self>, PhysicsError> {
        if hulls.is_empty() {
            return Err(PhysicsError::InvalidInput("body needs at least one hull"));
        }
        if hulls.len() > i32::MAX as usize {
            return Err(PhysicsError::InvalidInput("too many hulls"));
        }
        let mut counts = Vec::with_capacity(hulls.len());
        for hull in hulls {
            validate_hull(hull)?;
            counts.push(
                i32::try_from(hull.len())
                    .map_err(|_| PhysicsError::InvalidInput("too many hull points"))?,
            );
        }
        let mut cooked = Vec::with_capacity(hulls.len());
        let _lock = native_lock();
        for (hull, count) in hulls.iter().zip(counts) {
            let native = unsafe {
                ffi::manifold_box3d_cook_hull(hull.as_ptr().cast::<f32>(), count, max_vertex_count)
            };
            if native == 0 {
                for hull in cooked {
                    unsafe { ffi::manifold_box3d_destroy_hull(hull) };
                }
                return Err(PhysicsError::InvalidInput(TOO_THIN));
            }
            cooked.push(native);
        }
        Ok(Arc::new(Self {
            form: CookedForm::Hulls(cooked),
        }))
    }

    /// Build a static triangle mesh from already scaled `vertices`. Leaves
    /// out the slivers Box3D drops (under 0.25 mm², far under its 5 mm
    /// contact skin, so no body touches them), measured at this scale. Refuses
    /// a vertex out of range, an index out of bounds, and a mesh with nothing
    /// left.
    pub(crate) fn mesh(
        vertices: &[[f32; 3]],
        triangles: &[[u32; 3]],
    ) -> Result<Arc<Self>, PhysicsError> {
        for vertex in vertices {
            validate_position(*vertex)?;
        }
        let (center, unit_inertia) = triangle_mesh_mass_properties(vertices, triangles, 1.0)?;
        if vertices.len() > i32::MAX as usize || triangles.len() > i32::MAX as usize {
            return Err(PhysicsError::InvalidInput(
                "too many mesh vertices or triangles",
            ));
        }
        let mut indices = Vec::with_capacity(triangles.len() * 3);
        for triangle in triangles {
            for &index in triangle {
                indices.push(
                    i32::try_from(index)
                        .map_err(|_| PhysicsError::InvalidInput("mesh index is too large"))?,
                );
            }
        }
        // Every index was checked against `vertices` by the mass properties.
        let kept = unsafe {
            ffi::manifold_box3d_keep_mesh_triangles(
                vertices.as_ptr().cast::<f32>(),
                indices.as_mut_ptr(),
                triangles.len() as i32,
            )
        };
        if kept < 1 {
            return Err(PhysicsError::InvalidInput(
                "every mesh triangle is too small to collide with",
            ));
        }
        let _lock = native_lock();
        let mesh = unsafe {
            ffi::manifold_box3d_cook_mesh(
                vertices.as_ptr().cast::<f32>(),
                vertices.len() as i32,
                indices.as_ptr(),
                kept,
            )
        };
        match mesh {
            usize::MAX => Err(PhysicsError::InvalidInput(
                "Box3D discarded degenerate mesh triangles",
            )),
            0 => Err(PhysicsError::NativeAllocation),
            mesh => Ok(Arc::new(Self {
                form: CookedForm::Mesh {
                    mesh,
                    center,
                    unit_inertia,
                },
            })),
        }
    }

    /// Cook a height field at `scale` (each axis above zero: Box3D's grid
    /// cannot mirror).
    pub(crate) fn height_field(
        field: &crate::collision_shape::HeightField,
        scale: [f32; 3],
    ) -> Result<Arc<Self>, PhysicsError> {
        let [count_x, count_z] = field.points;
        let count = count_x as usize * count_z as usize;
        if count_x < 2 || count_z < 2 || field.heights.len() != count || count > i32::MAX as usize {
            return Err(PhysicsError::InvalidInput(
                "a height field needs at least 2 by 2 points and one height each",
            ));
        }
        if !(field.spacing.is_finite() && field.spacing > 0.0) || scale.iter().any(|&s| s <= 0.0) {
            return Err(PhysicsError::InvalidInput(
                "a height field takes a positive spacing and scale",
            ));
        }
        let [low, high] = field.height_range();
        if !(low.is_finite() && high.is_finite()) {
            return Err(PhysicsError::InvalidInput(
                "a height field's heights must be finite",
            ));
        }
        let origin: [f32; 3] = std::array::from_fn(|axis| field.origin[axis] * scale[axis]);
        validate_position(origin)?;
        let _lock = native_lock();
        let native = unsafe {
            ffi::manifold_box3d_cook_height_field(
                field.heights.as_ptr(),
                count_x as i32,
                count_z as i32,
                field.spacing * scale[0],
                scale[1],
                field.spacing * scale[2],
                low,
                high,
            )
        };
        if native == 0 {
            return Err(PhysicsError::NativeAllocation);
        }
        Ok(Arc::new(Self {
            form: CookedForm::HeightField {
                field: native,
                origin,
            },
        }))
    }

    /// Where Box3D places a body authored at `position` and `rotation`: a
    /// height field's grid starts at its body's origin, so that body sits at
    /// the grid's first point; every other body where authored.
    pub(crate) fn placed(&self, position: [f32; 3], rotation: [f32; 4]) -> [f32; 3] {
        match &self.form {
            CookedForm::HeightField { origin, .. } => {
                let [x, y, z, w] = rotation;
                let cross = |a: [f32; 3], b: [f32; 3]| {
                    [
                        a[1] * b[2] - a[2] * b[1],
                        a[2] * b[0] - a[0] * b[2],
                        a[0] * b[1] - a[1] * b[0],
                    ]
                };
                let t = cross([x, y, z], *origin).map(|c| 2.0 * c);
                let u = cross([x, y, z], t);
                std::array::from_fn(|i| position[i] + origin[i] + w * t[i] + u[i])
            }
            _ => position,
        }
    }

    /// Whether this is a static surface (a mesh or a height field) rather
    /// than hulls.
    pub fn is_mesh(&self) -> bool {
        matches!(
            self.form,
            CookedForm::Mesh { .. } | CookedForm::HeightField { .. }
        )
    }

    pub(crate) fn form(&self) -> &CookedForm {
        &self.form
    }
}

impl Drop for CookedSolid {
    fn drop(&mut self) {
        let _lock = native_lock();
        match &self.form {
            CookedForm::Hulls(hulls) => {
                for hull in hulls {
                    unsafe { ffi::manifold_box3d_destroy_hull(*hull) };
                }
            }
            CookedForm::Mesh { mesh, .. } => unsafe { ffi::manifold_box3d_destroy_mesh(*mesh) },
            CookedForm::HeightField { field, .. } => unsafe {
                ffi::manifold_box3d_destroy_height_field(*field)
            },
        }
    }
}
