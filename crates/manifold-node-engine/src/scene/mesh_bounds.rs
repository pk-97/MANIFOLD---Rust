//! The axis-aligned box around a mesh's vertices, in the mesh's own space.
//!
//! A producer whose vertices are known on the CPU publishes it next to its
//! `vertices` output as `Option<MeshBounds>`; `None` says the box is unknown
//! right now (it overwrites whatever the slot held before). A consumer that
//! finds no value treats the mesh as unbounded. Meshes written on the GPU
//! never publish one.

/// Tight min and max corners of a vertex set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

inventory::submit! { crate::exec::cpu_values::CpuWireRegistration::new::<Option<MeshBounds>>() }

impl MeshBounds {
    /// The box around `positions`. `None` for an empty set or any coordinate
    /// that is NaN or infinite: such a mesh has no box to trust.
    pub fn of_positions(positions: impl IntoIterator<Item = [f32; 3]>) -> Option<Self> {
        let mut positions = positions.into_iter();
        let first = positions.next()?;
        let (mut min, mut max) = (first, first);
        let mut finite = first.iter().all(|c| c.is_finite());
        for p in positions {
            for axis in 0..3 {
                finite &= p[axis].is_finite();
                min[axis] = min[axis].min(p[axis]);
                max[axis] = max[axis].max(p[axis]);
            }
        }
        finite.then_some(Self { min, max })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_around_positions_is_exact() {
        let bounds =
            MeshBounds::of_positions([[1.0, -2.0, 3.0], [-4.0, 5.0, 0.5], [0.0, 0.0, -7.0]])
                .expect("finite");
        assert_eq!(bounds.min, [-4.0, -2.0, -7.0]);
        assert_eq!(bounds.max, [1.0, 5.0, 3.0]);
        let point = MeshBounds::of_positions([[2.0, 3.0, 4.0]]).expect("one point");
        assert_eq!((point.min, point.max), ([2.0, 3.0, 4.0], [2.0, 3.0, 4.0]));
    }

    #[test]
    fn empty_or_non_finite_positions_have_no_box() {
        assert_eq!(MeshBounds::of_positions([]), None);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(MeshBounds::of_positions([[0.0; 3], [0.0, bad, 1.0]]), None);
            assert_eq!(MeshBounds::of_positions([[bad, 0.0, 0.0], [1.0; 3]]), None);
        }
    }
}
