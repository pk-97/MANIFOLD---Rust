//! Shared scene-space geometry and state for liquid domains.

/// The authored pose of a liquid domain box. Engine scene transforms convert
/// into it; core has no scene transform type of its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DomainBox {
    pub pos: [f32; 3],
    pub rot_euler: [f32; 3],
    pub scale: [f32; 3],
    pub billboard: bool,
}

/// The smallest side of a liquid domain box, in metres: 8 cells of the
/// smallest Cell Size, the fewest a layout takes. The domain's scale handle
/// stops at it; a box too small for its own Cell Size is refused by name.
pub const MIN_DOMAIN_SIDE_METRES: f32 = 8.0 * CELL_SIZE_METRES.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidDomainLayout {
    pub min: [f32; 3],
    pub size: [f32; 3],
    pub cells: [u32; 3],
    pub cell_size: f64,
}

/// The cell size a new Water starts with, in metres.
pub const DEFAULT_CELL_SIZE: f32 = 0.0625;

/// The smallest and largest cell size a liquid domain accepts, in metres.
pub const CELL_SIZE_METRES: (f32, f32) = (0.001, 1.0);

/// The box a domain without one places: a `domain_size` cube centred in X/Z
/// with its floor at Y = 0.
fn default_box(domain_size: f32) -> DomainBox {
    DomainBox {
        pos: [0.0, domain_size * 0.5, 0.0],
        scale: [domain_size; 3],
        rot_euler: [0.0; 3],
        billboard: false,
    }
}

/// The box a liquid's cells start from with Bounds off, where no box holds
/// the water: the Fill's box `fill` (min, max) with a cell to spare round
/// it, else its own hidden box (or the `domain_size` cube without one), at
/// least 8 cells a side and unturned. Any Cell Size and any Fill then lay
/// out, so with Bounds off neither is refused nor clipped by a box no one
/// sees, and a Fill far from the hidden box costs no cells between them.
pub fn unbounded_seed(
    domain: Option<DomainBox>,
    domain_size: f32,
    fill: Option<[[f32; 3]; 2]>,
    cell_size: f64,
) -> DomainBox {
    let h = cell_size as f32;
    let [lo, hi] = match fill {
        Some([lo, hi]) => [lo.map(|v| v - h), hi.map(|v| v + h)],
        None => {
            let pose = domain.unwrap_or(default_box(domain_size));
            [
                std::array::from_fn(|i| pose.pos[i] - 0.5 * pose.scale[i]),
                std::array::from_fn(|i| pose.pos[i] + 0.5 * pose.scale[i]),
            ]
        }
    };
    DomainBox {
        pos: std::array::from_fn(|i| 0.5 * (lo[i] + hi[i])),
        scale: std::array::from_fn(|i| (hi[i] - lo[i]).max(9.0 * h)),
        rot_euler: [0.0; 3],
        billboard: false,
    }
}

/// The scene box and uniform cells of a liquid domain, shared by every
/// liquid solver so they agree on cell size.
/// `domain` is the authored axis-aligned box (scale = full size); without
/// one, a `domain_size` cube centred in X/Z with its floor at Y = 0.
/// `cell_size` is the cell edge in metres; each axis gets as many whole
/// cells as cover the box.
pub fn domain_layout(
    domain: Option<DomainBox>,
    domain_size: f32,
    cell_size: f64,
) -> Result<FluidDomainLayout, String> {
    if !cell_size.is_finite() || cell_size <= 0.0 {
        return Err("Fluid: Cell Size must be a positive number of metres".into());
    }
    let pose = domain.unwrap_or(default_box(domain_size));
    if pose.billboard
        || pose
            .rot_euler
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1e-6)
    {
        return Err(
            "Fluid: the domain must be axis-aligned; rotation and billboarding are not supported"
                .into(),
        );
    }
    if pose.pos.iter().any(|v| !v.is_finite())
        || pose.scale.iter().any(|v| !v.is_finite() || *v <= 0.0)
    {
        return Err(
            "Fluid: the domain's position must be finite and each side a positive length".into(),
        );
    }
    let cells: [u32; 3] = std::array::from_fn(|i| {
        let ratio = f64::from(pose.scale[i]) / cell_size;
        // The cell size arrives as an f32 param, so a box an exact multiple
        // of it divides to within f32 rounding of its whole count, on either
        // side. Do not add a whole cell for that roundoff.
        let count = if (ratio - ratio.round()).abs() <= 1e-6 * ratio {
            ratio.round()
        } else {
            ratio.ceil()
        };
        // Saturating cast: an absurdly fine cell size trips the grid check.
        count as u32
    });
    if cells.iter().any(|n| *n < 8) {
        return Err(format!(
            "Fluid: a Cell Size of {cell_size:.3} m leaves fewer than 8 cells across the Bounds box; lower Cell Size or make the box larger"
        ));
    }
    if cells
        .iter()
        .try_fold(1u64, |total, n| total.checked_mul(u64::from(*n) + 4))
        .is_none_or(|total| total > i32::MAX as u64)
    {
        return Err(
            "Fluid: padded grid exceeds the native solver's 32-bit grid indexing. Raise Cell Size."
                .into(),
        );
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
    pub fn transform(self) -> DomainBox {
        DomainBox {
            pos: std::array::from_fn(|i| self.min[i] + self.size[i] * 0.5),
            scale: self.size,
            rot_euler: [0.0; 3],
            billboard: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FluidDomainState {
    Initializing,
    Ready,
    PendingInputs,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FluidDomainSnapshot {
    pub epoch: u64,
    pub state: FluidDomainState,
    pub accepted_layout: Option<FluidDomainLayout>,
    /// What the solver capped or clipped in the fill it ran this frame.
    pub fit: crate::liquid_setup::SetupFit,
    /// The initial column as the solver ran it, when it did not fit: where
    /// a driven column's warning box draws.
    pub column_box: Option<([f32; 3], [f32; 3])>,
    /// Why the solver refused the setup it was handed, when it did.
    pub refusal: Option<std::sync::Arc<str>>,
    /// Bodies of the coupled world the liquid could not take: past
    /// [`MAX_LIQUID_BODIES`] rows the water neither pushes nor flows round them.
    pub uncoupled_bodies: u32,
}

/// The role inputs (`role_0`, `role_1`, ...) a liquid domain starts with; its
/// `roles` param grows them, one per source.
pub const DEFAULT_ROLE_PORTS: usize = 4;

/// The most body rows one GPU liquid holds per tick: the coupled Physics
/// World's Colliders and Bodies. GPU FLIP marks each solid face
/// with its body in one byte, 0 for none, three bytes in a float's exact
/// integers. Bodies past it are not coupled, and the domain reports how many.
pub const MAX_LIQUID_BODIES: usize = 255;

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(pos: [f32; 3], scale: [f32; 3]) -> Option<DomainBox> {
        Some(DomainBox {
            pos,
            rot_euler: [0.0; 3],
            scale,
            billboard: false,
        })
    }

    #[test]
    fn cells_follow_the_cell_size_per_axis() {
        let layout = domain_layout(boxed([0.0, 1.0, 0.0], [4.0, 2.0, 3.0]), 0.0, 0.0625).unwrap();
        assert_eq!(layout.cells, [64, 32, 48]);
        assert_eq!(layout.cell_size, 0.0625);
    }

    #[test]
    fn a_box_with_fewer_than_eight_cells_refuses() {
        let err = domain_layout(boxed([0.0; 3], [1.0, 1.0, 1.0]), 0.0, 0.3).unwrap_err();
        assert!(err.contains("fewer than 8 cells"), "{err}");
    }

    /// With Bounds off a Fill outside the hidden box, near or 500 m off, and
    /// a Cell Size too coarse for that box all lay out: the cells cover the
    /// Fill alone, at least 8 a side.
    #[test]
    fn an_unbounded_seed_takes_in_the_fill_at_any_cell_size() {
        for x in [5.5, 500.0] {
            let fill = [[x, 0.5, -1.0], [x + 1.0, 2.5, 1.0]];
            for cell in [0.02, 0.23, 0.9] {
                let hidden = boxed([0.0, 2.0, 0.0], [4.0, 4.0, 4.0]);
                let seed = unbounded_seed(hidden, 0.0, Some(fill), cell);
                let layout = domain_layout(Some(seed), 0.0, cell).unwrap();
                let covers = (0..3).all(|i| {
                    layout.min[i] <= fill[0][i] && fill[1][i] <= layout.min[i] + layout.size[i]
                });
                let snug = (0..3).all(|i| {
                    layout.size[i]
                        <= (fill[1][i] - fill[0][i]).max(9.0 * cell as f32) + 3.0 * cell as f32
                });
                assert!(covers && snug, "{x} {cell}: {layout:?}");
                assert!(
                    layout.cells.iter().all(|&n| n >= 8),
                    "{x} {cell}: {layout:?}"
                );
            }
        }
        let tiny = unbounded_seed(None, 0.1, None, 0.5);
        assert_eq!(domain_layout(Some(tiny), 0.0, 0.5).unwrap().cells, [9; 3]);
    }

    #[test]
    fn a_bad_cell_size_refuses() {
        for bad in [0.0, -1.0, f64::NAN] {
            let err = domain_layout(None, 4.0, bad).unwrap_err();
            assert!(err.contains("positive number of metres"), "{err}");
        }
    }

    #[test]
    fn a_far_origin_refuses() {
        let err =
            domain_layout(boxed([1.0e7, 1.0, 0.0], [4.0, 4.0, 4.0]), 0.0, 0.0625).unwrap_err();
        assert!(err.contains("too far from the scene origin"), "{err}");
    }
}
