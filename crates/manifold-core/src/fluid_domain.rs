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

/// The smallest and largest side of a liquid domain box, in metres. The
/// solvers refuse a layout outside it, so the domain's scale handle stops
/// at it.
pub const DOMAIN_EXTENT_METRES: (f32, f32) = (0.5, 20.0);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidDomainLayout {
    pub min: [f32; 3],
    pub size: [f32; 3],
    pub cells: [u32; 3],
    pub cell_size: f64,
}

/// The scene box and uniform cells of a liquid domain, shared by every
/// liquid solver so they agree on cell size.
/// `domain` is the authored axis-aligned box (scale = full size); without
/// one, a `domain_size` cube centred in X/Z with its floor at Y = 0.
pub fn domain_layout(
    domain: Option<DomainBox>,
    domain_size: f32,
    resolution: u32,
) -> Result<FluidDomainLayout, String> {
    if resolution < 8 {
        return Err("Fluid: resolution must be at least 8 cells".into());
    }
    let pose = domain.unwrap_or(DomainBox {
        pos: [0.0, domain_size * 0.5, 0.0],
        scale: [domain_size; 3],
        rot_euler: [0.0; 3],
        billboard: false,
    });
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
        || pose.scale.iter().any(|v| {
            !v.is_finite() || !(DOMAIN_EXTENT_METRES.0..=DOMAIN_EXTENT_METRES.1).contains(v)
        })
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
        || cells
            .iter()
            .try_fold(1u64, |total, n| total.checked_mul(u64::from(*n) + 4))
            .is_none_or(|total| total > i32::MAX as u64)
    {
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
