//! How a liquid's setup shapes fit its domain: the one geometric rule the
//! solvers fill from and the setup check reports from
//! (SCENE_SETUP_PROGRAMME.md section 3 B3, BUG-gm927 parts 1 and 2). A shape
//! that does not fit gets a defined result here (capped, clipped, ignored),
//! never a refusal or a silent clamp: the check names whatever this changed.

use crate::fluid_domain::FluidDomainLayout;

/// A shape edge within this of a domain wall counts as on it.
pub const FIT_TOLERANCE: f32 = 1e-4;

/// A column's low and high corner, in world units.
pub type ColumnEdges = ([f64; 3], [f64; 3]);

/// What a solver did with the initial column this evaluation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColumnOutcome {
    /// It fits, or there is none.
    #[default]
    Fits,
    /// Part of it was past a wall; the part inside filled.
    Clipped,
    /// It was wholly outside and filled nothing.
    Outside,
}

/// What a solver changed in the fill it ran, on the value it ran: wired,
/// driven, modulated or authored alike. A domain reports it every frame so
/// nothing is capped or clipped without a warning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SetupFit {
    pub fill_capped: bool,
    pub column: ColumnOutcome,
    /// The particle pool is full and an inflow wanted more: the inflow is
    /// held back until water drains. Reported by the solver as it runs.
    pub pool_full: bool,
}

impl SetupFit {
    pub fn is_clean(self) -> bool {
        self == Self::default()
    }
}

/// The Fill Height a solver fills to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FillFit {
    /// The height fits the domain.
    Within(f32),
    /// The height reaches the top or past it; the pool fills to the top.
    Capped(f32),
}

impl FillFit {
    /// The height the pool fills to, in metres.
    pub fn height(self) -> f32 {
        match self {
            Self::Within(height) | Self::Capped(height) => height,
        }
    }
}

/// Fit a Fill Height to the domain: above the domain's top it is capped
/// there; at the top it already fills to it. A height that is not a finite number of at least zero has
/// no defined fill.
pub fn fit_fill_height(layout: &FluidDomainLayout, height: f32) -> Result<FillFit, &'static str> {
    if !height.is_finite() || height < 0.0 {
        return Err("Fill Height must be a number of at least zero");
    }
    let top = layout.size[1];
    Ok(if height > top + FIT_TOLERANCE {
        FillFit::Capped(top)
    } else {
        FillFit::Within(height.min(top))
    })
}

/// Cells of padding every liquid lattice holds outside the domain box
/// (taichi's `padding = 3`): solid within it still acts on the water.
pub const LATTICE_PADDING_CELLS: u32 = 3;

/// Fill Height when none is authored: the water node's own default.
pub const DEFAULT_FILL_HEIGHT: f32 = 0.4;

/// A world-space axis-aligned box against a domain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BoxFit {
    /// Wholly inside the domain.
    Inside,
    /// Partly past a wall: the part inside, as a box.
    Clipped { min: [f32; 3], max: [f32; 3] },
    /// Nothing of it lies inside the domain.
    Outside,
}

/// One axis of a box against the domain's extent on that axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisFit {
    Inside,
    /// Reaches past a wall; some of it is inside.
    Clipped,
    /// No overlap, or it only touches a wall from outside.
    Outside,
}

/// Each axis of the box `min`..`max` against the domain: the one test
/// [`fit_box`] and the setup check's "which control" both read.
pub fn axis_fits(layout: &FluidDomainLayout, min: [f32; 3], max: [f32; 3]) -> [AxisFit; 3] {
    std::array::from_fn(|d| {
        let (lo, hi) = (layout.min[d], layout.min[d] + layout.size[d]);
        let (a, b) = (min[d].max(lo), max[d].min(hi));
        if b <= a {
            AxisFit::Outside
        } else if min[d] < lo - FIT_TOLERANCE || max[d] > hi + FIT_TOLERANCE {
            AxisFit::Clipped
        } else {
            AxisFit::Inside
        }
    })
}

/// Fit the box `min`..`max` to the domain. A box that only touches a wall
/// from outside has no volume inside and is [`BoxFit::Outside`].
pub fn fit_box(layout: &FluidDomainLayout, min: [f32; 3], max: [f32; 3]) -> BoxFit {
    let fits = axis_fits(layout, min, max);
    if fits.contains(&AxisFit::Outside) {
        return BoxFit::Outside;
    }
    if fits.iter().all(|fit| *fit == AxisFit::Inside) {
        return BoxFit::Inside;
    }
    let clipped = |d: usize| {
        let (lo, hi) = (layout.min[d], layout.min[d] + layout.size[d]);
        (
            if min[d] < lo - FIT_TOLERANCE {
                lo
            } else {
                min[d]
            },
            if max[d] > hi + FIT_TOLERANCE {
                hi
            } else {
                max[d]
            },
        )
    };
    BoxFit::Clipped {
        min: std::array::from_fn(|d| clipped(d).0),
        max: std::array::from_fn(|d| clipped(d).1),
    }
}

/// The initial column centred on `pos` of size `scale` as a world box, in
/// f32: what [`fit_column`] classifies and what a warning outlines.
pub fn column_box(pos: [f32; 3], scale: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    (
        std::array::from_fn(|d| pos[d] - scale[d] * 0.5),
        std::array::from_fn(|d| pos[d] + scale[d] * 0.5),
    )
}

/// The initial column's edges as the solvers site them: `centre ± half` in
/// f64, so a column that fits sites exactly as it always did, with the wall
/// value on a side that was clipped. `None` when the column is wholly
/// outside and fills nothing; an `Err` when it is not a finite box. The
/// outcome says which of the three it was.
pub fn fit_column(
    layout: &FluidDomainLayout,
    pos: [f32; 3],
    scale: [f32; 3],
) -> Result<(Option<ColumnEdges>, ColumnOutcome), &'static str> {
    if pos.iter().chain(&scale).any(|v| !v.is_finite()) || scale.iter().any(|v| *v <= 0.0) {
        return Err("the initial volume must be a finite box");
    }
    let (lo32, hi32) = column_box(pos, scale);
    let (clip_lo, clip_hi, outcome) = match fit_box(layout, lo32, hi32) {
        BoxFit::Outside => return Ok((None, ColumnOutcome::Outside)),
        BoxFit::Inside => (lo32, hi32, ColumnOutcome::Fits),
        BoxFit::Clipped { min, max } => (min, max, ColumnOutcome::Clipped),
    };
    let edge = |d: usize, sign: f64, clipped: f32, raw: f32| {
        if clipped != raw {
            f64::from(clipped)
        } else {
            f64::from(pos[d]) + sign * f64::from(scale[d]) * 0.5
        }
    };
    let edges = (
        std::array::from_fn(|d| edge(d, -1.0, clip_lo[d], lo32[d])),
        std::array::from_fn(|d| edge(d, 1.0, clip_hi[d], hi32[d])),
    );
    Ok((Some(edges), outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> FluidDomainLayout {
        FluidDomainLayout {
            min: [-2.0, 0.0, -2.0],
            size: [4.0, 4.0, 4.0],
            cells: [32; 3],
            cell_size: 0.125,
        }
    }

    #[test]
    fn fill_height_caps_at_the_top_and_refuses_no_number() {
        let layout = layout();
        assert_eq!(fit_fill_height(&layout, 1.5), Ok(FillFit::Within(1.5)));
        assert_eq!(fit_fill_height(&layout, 4.0), Ok(FillFit::Within(4.0)));
        assert_eq!(fit_fill_height(&layout, 4.5), Ok(FillFit::Capped(4.0)));
        assert_eq!(fit_fill_height(&layout, 9.0).map(FillFit::height), Ok(4.0));
        assert!(fit_fill_height(&layout, -0.1).is_err());
        assert!(fit_fill_height(&layout, f32::NAN).is_err());
    }

    #[test]
    fn a_box_is_inside_clipped_or_outside() {
        let layout = layout();
        assert_eq!(
            fit_box(&layout, [-1.0, 0.0, -1.0], [1.0, 2.0, 1.0]),
            BoxFit::Inside
        );
        assert_eq!(
            fit_box(&layout, [1.0, 1.0, -1.0], [3.0, 2.0, 1.0]),
            BoxFit::Clipped {
                min: [1.0, 1.0, -1.0],
                max: [2.0, 2.0, 1.0]
            }
        );
        assert_eq!(
            fit_box(&layout, [2.5, 0.0, 0.0], [3.5, 1.0, 1.0]),
            BoxFit::Outside
        );
        // Touching a wall from outside leaves no volume.
        assert_eq!(
            fit_box(&layout, [2.0, 0.0, 0.0], [3.0, 1.0, 1.0]),
            BoxFit::Outside
        );
        // On the wall from inside is inside.
        assert_eq!(
            fit_box(&layout, [1.0, 0.0, 0.0], [2.0, 1.0, 1.0]),
            BoxFit::Inside
        );
    }

    #[test]
    fn a_column_that_fits_keeps_its_f64_edges() {
        // Sea Wall: an 8 m domain at resolution 112, a column at x = -2.83,
        // 0.41 wide. The f64 edges are what the site range was always built
        // from; an f32 subtraction would round them.
        let layout = FluidDomainLayout {
            min: [-4.0, 0.0, -4.0],
            size: [8.0; 3],
            cells: [112; 3],
            cell_size: 8.0 / 112.0,
        };
        let (lo, hi) = fit_column(&layout, [-2.83, 1.0, 0.0], [0.41, 1.0, 1.0])
            .unwrap()
            .0
            .unwrap();
        assert_eq!(lo[0], f64::from(-2.83f32) - f64::from(0.41f32) * 0.5);
        assert_eq!(hi[0], f64::from(-2.83f32) + f64::from(0.41f32) * 0.5);
    }

    #[test]
    fn a_column_past_a_wall_takes_the_wall_on_that_side_only() {
        let layout = layout();
        let (edges, outcome) = fit_column(&layout, [2.0, 1.0, 0.0], [2.0, 1.0, 1.0]).unwrap();
        assert_eq!(outcome, ColumnOutcome::Clipped);
        let (lo, hi) = edges.unwrap();
        assert_eq!((lo[0], hi[0]), (1.0, 2.0));
        assert_eq!((lo[1], hi[1]), (0.5, 1.5));
        assert_eq!(
            fit_column(&layout, [9.0, 1.0, 0.0], [1.0; 3]),
            Ok((None, ColumnOutcome::Outside))
        );
        assert!(fit_column(&layout, [0.0; 3], [0.0, 1.0, 1.0]).is_err());
    }

    #[test]
    fn axes_name_what_leaves_the_domain() {
        let layout = layout();
        assert_eq!(
            axis_fits(&layout, [1.0, 1.0, 9.0], [3.0, 2.0, 10.0]),
            [AxisFit::Clipped, AxisFit::Inside, AxisFit::Outside]
        );
    }
}
