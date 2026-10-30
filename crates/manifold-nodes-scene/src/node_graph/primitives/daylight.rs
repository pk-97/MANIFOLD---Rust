//! The Earth's air for the Daylight sky (`shaders/daylight.wgsl`): its
//! constants, packed as the shader reads them, and the share of sunlight
//! that gets through it to the viewer, which tints the Sun light the sky
//! passes on. Distances are in km.

/// `Atmosphere` in `daylight.wgsl`, field for field.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Atmosphere {
    /// Ground radius, top of the air, the viewer's height, ground albedo.
    pub radii: [f32; 4],
    /// Rayleigh scattering per km (rgb) and its scale height.
    pub rayleigh: [f32; 4],
    /// Mie scattering and extinction per km, scale height, phase g.
    pub mie: [f32; 4],
    /// Ozone absorption per km (rgb) and the height of its peak.
    pub ozone: [f32; 4],
}

/// Bruneton's Earth (2017), as Hillaire (2020) uses it, seen from half a
/// kilometre up over ground of albedo 0.3.
pub(crate) const EARTH: Atmosphere = Atmosphere {
    radii: [6360.0, 6460.0, 0.5, 0.3],
    rayleigh: [5.802e-3, 13.558e-3, 33.1e-3, 8.0],
    mie: [3.996e-3, 4.44e-3, 1.2, 0.8],
    ozone: [0.650e-3, 1.881e-3, 0.085e-3, 25.0],
};

/// `OZONE_HALF_WIDTH` and `TRANSMITTANCE_STEPS` in `daylight.wgsl`.
const OZONE_HALF_WIDTH: f32 = 15.0;
const TRANSMITTANCE_STEPS: u32 = 24;

/// What the air at height `h` takes away per km, as `air_at` reads it.
fn extinction(a: &Atmosphere, h: f32) -> [f32; 3] {
    let h = h.max(0.0);
    let gas = (-h / a.rayleigh[3]).exp();
    let haze = (-h / a.mie[2]).exp();
    let ozone = (1.0 - (h - a.ozone[3]).abs() / OZONE_HALF_WIDTH).max(0.0);
    [0, 1, 2].map(|c| a.rayleigh[c] * gas + a.mie[1] * haze + a.ozone[c] * ozone)
}

/// The share of light from straight along `toward_sun` (a unit vector, y up)
/// that reaches the viewer through the air, per colour: none once the sun
/// is under the ground. `transmittance_to_space` in `daylight.wgsl`.
pub(crate) fn sunlight_through_air(a: &Atmosphere, toward_sun: [f32; 3]) -> [f32; 3] {
    let r = a.radii[0] + a.radii[2];
    let mu = toward_sun[1].clamp(-1.0, 1.0);
    let ground = r * r * (mu * mu - 1.0) + a.radii[0] * a.radii[0];
    if mu < 0.0 && ground >= 0.0 {
        return [0.0; 3];
    }
    let top = r * r * (mu * mu - 1.0) + a.radii[1] * a.radii[1];
    let reach = (-r * mu + top.max(0.0).sqrt()).max(0.0);
    let mut depth = [0.0f32; 3];
    let mut t0 = 0.0;
    for i in 0..TRANSMITTANCE_STEPS {
        let s = (i + 1) as f32 / TRANSMITTANCE_STEPS as f32;
        let t1 = reach * s * s;
        let t = 0.5 * (t0 + t1);
        let h = (t * t + 2.0 * r * mu * t + r * r).sqrt() - a.radii[0];
        let e = extinction(a, h);
        for c in 0..3 {
            depth[c] += e[c] * (t1 - t0);
        }
        t0 = t1;
    }
    depth.map(|d| (-d).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Straight up, the light crosses the air's whole depth once: the
    /// Rayleigh and haze columns (scattering times scale height) and the
    /// ozone tent's area. Low down it crosses far more air, and the blue goes
    /// first, so the sun reddens; under the ground it is gone.
    #[test]
    fn sunlight_reddens_toward_the_horizon() {
        let a = EARTH;
        let up = sunlight_through_air(&a, [0.0, 1.0, 0.0]);
        let above = (-a.radii[2] / a.rayleigh[3]).exp();
        let haze = (-a.radii[2] / a.mie[2]).exp();
        let expected = [0, 1, 2].map(|c| {
            let depth = a.rayleigh[c] * a.rayleigh[3] * above
                + a.mie[1] * a.mie[2] * haze
                + a.ozone[c] * OZONE_HALF_WIDTH;
            (-depth).exp()
        });
        for (got, want) in up.iter().zip(expected) {
            assert!((got - want).abs() < 0.02 * want, "{got} vs {want}");
        }
        let elevation = 3.0f32.to_radians();
        let low = sunlight_through_air(&a, [elevation.cos(), elevation.sin(), 0.0]);
        assert!(low[0] < up[0] && low[2] < up[2]);
        assert!(low[0] / low[2] > 3.0 * up[0] / up[2], "{low:?} vs {up:?}");
        let set = sunlight_through_air(&a, [0.0, -0.2, -0.98]);
        assert_eq!(set, [0.0; 3]);
    }
}
