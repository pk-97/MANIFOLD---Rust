//! How a liquid's setup shapes fit its domain: the one geometric rule the
//! solvers fill from and the setup check reports from
//! (SCENE_SETUP_PROGRAMME.md section 3 B3, BUG-gm927 parts 1 and 2). A shape
//! that does not fit gets a defined result here (capped, clipped, ignored),
//! never a refusal or a silent clamp: the check names whatever this changed.

use crate::fluid_domain::FluidDomainLayout;

/// A shape edge within this of a domain wall counts as on it.
pub const FIT_TOLERANCE: f32 = 1e-4;

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

/// Fit a Fill Height to the domain: at or above the domain's top it is
/// capped there. A height that is not a finite number of at least zero has
/// no defined fill.
pub fn fit_fill_height(layout: &FluidDomainLayout, height: f32) -> Result<FillFit, &'static str> {
    if !height.is_finite() || height < 0.0 {
        return Err("Fill Height must be a number of at least zero");
    }
    let top = layout.size[1];
    Ok(if height >= top { FillFit::Capped(top) } else { FillFit::Within(height) })
}

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

/// Fit the box `min`..`max` to the domain. A box that only touches a wall
/// from outside has no volume inside and is [`BoxFit::Outside`].
pub fn fit_box(layout: &FluidDomainLayout, min: [f32; 3], max: [f32; 3]) -> BoxFit {
    let mut clipped_min = min;
    let mut clipped_max = max;
    let mut past = false;
    for d in 0..3 {
        let (lo, hi) = (layout.min[d], layout.min[d] + layout.size[d]);
        if min[d] < lo - FIT_TOLERANCE {
            clipped_min[d] = lo;
            past = true;
        }
        if max[d] > hi + FIT_TOLERANCE {
            clipped_max[d] = hi;
            past = true;
        }
        if clipped_max[d] <= clipped_min[d] {
            return BoxFit::Outside;
        }
    }
    if past { BoxFit::Clipped { min: clipped_min, max: clipped_max } } else { BoxFit::Inside }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> FluidDomainLayout {
        FluidDomainLayout { min: [-2.0, 0.0, -2.0], size: [4.0, 4.0, 4.0], cells: [32; 3], cell_size: 0.125 }
    }

    #[test]
    fn fill_height_caps_at_the_top_and_refuses_no_number() {
        let layout = layout();
        assert_eq!(fit_fill_height(&layout, 1.5), Ok(FillFit::Within(1.5)));
        assert_eq!(fit_fill_height(&layout, 4.0), Ok(FillFit::Capped(4.0)));
        assert_eq!(fit_fill_height(&layout, 9.0).map(FillFit::height), Ok(4.0));
        assert!(fit_fill_height(&layout, -0.1).is_err());
        assert!(fit_fill_height(&layout, f32::NAN).is_err());
    }

    #[test]
    fn a_box_is_inside_clipped_or_outside() {
        let layout = layout();
        assert_eq!(fit_box(&layout, [-1.0, 0.0, -1.0], [1.0, 2.0, 1.0]), BoxFit::Inside);
        assert_eq!(
            fit_box(&layout, [1.0, 1.0, -1.0], [3.0, 2.0, 1.0]),
            BoxFit::Clipped { min: [1.0, 1.0, -1.0], max: [2.0, 2.0, 1.0] }
        );
        assert_eq!(fit_box(&layout, [2.5, 0.0, 0.0], [3.5, 1.0, 1.0]), BoxFit::Outside);
        // Touching a wall from outside leaves no volume.
        assert_eq!(fit_box(&layout, [2.0, 0.0, 0.0], [3.0, 1.0, 1.0]), BoxFit::Outside);
        // On the wall from inside is inside.
        assert_eq!(fit_box(&layout, [1.0, 0.0, 0.0], [2.0, 1.0, 1.0]), BoxFit::Inside);
    }
}
