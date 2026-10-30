//! MPM's side of Box3D coupling: the reaction words' decoding and the body term of the substep rule.
//! The owner that steps Box3D is shared: `liquid::coupling`.

use manifold_physics::BodyImpulse;

use super::{MAX_SUBSTEPS, REACTION_WORDS, substeps_per_tick};
use manifold_water_liquid::bodies::LiquidBody;
use manifold_water_liquid::coupling::{LiquidRigidOwner, SharedSolids, takes_reaction};

/// How a pending tick's reaction words decode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReactionScale {
    /// The tick's momentum unit U: a word is value·2^24/U.
    pub unit: f32,
    pub cell_size: f32,
    /// Rows before the first coupled body's.
    pub offset: usize,
}

/// The reaction words of the pending tick as one centre-of-mass impulse per
/// coupled body: linear m·ΣΔv, angular m·dx·Σ(I·Δω/dx·(1/m)). Rows that take
/// no reaction are left alone.
pub fn decode(
    scale: ReactionScale,
    rows: &[LiquidBody],
    words: Option<&[i32]>,
    impulses: &mut [BodyImpulse],
) -> Result<(), String> {
    let words = words.ok_or("Matter coupling: the reaction words are not readable")?;
    let stride = REACTION_WORDS as usize;
    if words.len() < (scale.offset + rows.len()) * stride {
        return Err("Matter coupling: the reaction array is smaller than the bodies".into());
    }
    let unit = f64::from(scale.unit) / 16_777_216.0;
    for (index, (row, impulse)) in rows.iter().zip(impulses.iter_mut()).enumerate() {
        if !takes_reaction(row) {
            continue;
        }
        let base = (scale.offset + index) * stride;
        let mass = 1.0 / f64::from(row.position_inv_mass[3]);
        let at = |word: usize, factor: f64| (f64::from(words[base + word]) * unit * factor) as f32;
        let arm = f64::from(scale.cell_size);
        for axis in 0..3 {
            impulse.linear[axis] += at(axis, mass);
            impulse.angular[axis] += at(6 + axis, mass * arm);
        }
    }
    Ok(())
}

/// The D4 body term: the shortest `0.5·(dx/c)·√(m_b/(ρ0·A_b·dx))` over the
/// dynamic coupled bodies, with `c` the wave speed and ρ0 the material's
/// density. Errors, naming the body, when it needs more than
/// [`MAX_SUBSTEPS`] substeps with `dt_f`.
fn body_limit_details<'a>(
    bodies: impl Iterator<Item = (f32, &'a LiquidBody)>,
    cell_size: f32,
    wave: f32,
    density: f32,
) -> Option<(f32, usize, f32)> {
    let mut limit: Option<(f32, usize, f32)> = None;
    for (index, (face_area, row)) in bodies.enumerate() {
        if !takes_reaction(row) {
            continue;
        }
        let dt = body_substep(
            cell_size,
            wave,
            density,
            1.0 / row.position_inv_mass[3],
            face_area,
        );
        if limit.is_none_or(|(known, _, _)| dt < known) {
            limit = Some((dt, index, face_area));
        }
    }
    limit
}

/// Live retains the actual body CFL restriction even when it exceeds the
/// numerical cap; the frame planner reports the cap and covers the remainder.
pub fn live_body_limit(
    owner: &LiquidRigidOwner,
    cell_size: f32,
    wave: f32,
    density: f32,
) -> Option<f32> {
    body_limit_details(
        owner.face_areas().zip(owner.rows()),
        cell_size,
        wave,
        density,
    )
    .map(|(dt, _, _)| dt)
}

/// The same limit over the bodies a Matter that takes another liquid's
/// solids pushes.
pub fn taken_body_limit(
    solids: &SharedSolids,
    cell_size: f32,
    wave: f32,
    density: f32,
) -> Option<f32> {
    body_limit_details(
        solids.face_areas().iter().copied().zip(solids.rows()),
        cell_size,
        wave,
        density,
    )
    .map(|(dt, _, _)| dt)
}

pub fn body_limit(
    owner: &LiquidRigidOwner,
    cell_size: f32,
    wave: f32,
    density: f32,
    v_est: f32,
) -> Result<Option<f32>, String> {
    let Some((dt, index, face_area)) = body_limit_details(
        owner.face_areas().zip(owner.rows()),
        cell_size,
        wave,
        density,
    ) else {
        return Ok(None);
    };
    if substeps_per_tick(cell_size, wave, v_est, Some(dt), None) > MAX_SUBSTEPS {
        let row = &owner.rows()[index];
        return Err(format!(
            "Matter coupling: coupled body {index} ({:.3} kg, {:.4} m² face) is too light for this resolution: it needs more than {MAX_SUBSTEPS} substeps. Make it heavier or lower the Stiffness.",
            1.0 / row.position_inv_mass[3],
            face_area
        ));
    }
    Ok(Some(dt))
}

/// D4's `dt_b` for one body of `mass` kg and largest face `area` m² in a
/// material of `density` kg/m³.
pub fn body_substep(cell_size: f32, wave: f32, density: f32, mass: f32, area: f32) -> f32 {
    let dx = f64::from(cell_size);
    let ratio = f64::from(mass) / (f64::from(density) * f64::from(area) * dx);
    (0.5 * dx / f64::from(wave) * ratio.sqrt()) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::scene_impulse::RigidImpulseTargets;
    use manifold_node_engine::scene::transform::Transform;
    use manifold_physics::clock::TICK;
    use manifold_water_liquid::coupling::PendingTick;
    use manifold_water_rigid::physics::{RigidBody, RigidSceneInputs};

    /// A reaction of Δv = +1 m/s (encoded at U = 128) reaches Box3D as a
    /// linear impulse of m·Δv, on top of the tick's gravity.
    #[test]
    fn matter_coupled_reaction_decodes_to_body_impulse() {
        let mut scene = RigidSceneInputs {
            gravity: [0.0, -9.81, 0.0],
            ..RigidSceneInputs::default()
        };
        scene.bodies[0] = Some(RigidBody {
            transform: Transform {
                pos: [0.0, 1.0, 0.0],
                scale: [0.4; 3],
                ..Transform::default()
            },
            mass: 50.0,
            bounce: 0.0,
            ..RigidBody::default()
        });
        let colliders = RigidImpulseTargets::body(0).unwrap();
        let open = manifold_water_liquid::coupling::DomainWalls::default();
        let capacity = manifold_core::fluid_domain::MAX_LIQUID_BODIES;
        let mut owner =
            LiquidRigidOwner::new(&scene, open, &colliders, capacity, 3, None).expect("owner");
        let scale = ReactionScale {
            unit: 128.0,
            cell_size: 0.0625,
            offset: 0,
        };
        let mut words = [0i32; 16];
        words[1] = (16_777_216.0 / f64::from(scale.unit)) as i32;
        owner.set_pending(PendingTick {
            tick: 0,
            stamp: 0,
            interval: manifold_physics::stepping::StepInterval::new(
                manifold_core::Seconds((0) as f64 * TICK),
                manifold_core::Seconds(TICK),
            ),
            offline: true,
        });
        owner
            .settle_ready(
                Some(&scene),
                |_| true,
                |_, rows, impulses| decode(scale, rows, Some(&words[..]), impulses),
            )
            .unwrap();
        let v = owner.rows()[0].linear_velocity[1];
        assert!((v - (1.0 - 9.81 * TICK as f32)).abs() < 1e-3, "{v}");
        let short = [0i32; 8];
        owner.set_pending(PendingTick {
            tick: 1,
            stamp: 0,
            interval: manifold_physics::stepping::StepInterval::new(
                manifold_core::Seconds((1) as f64 * TICK),
                manifold_core::Seconds(2.0 * TICK),
            ),
            offline: true,
        });
        let error = owner
            .settle_ready(
                Some(&scene),
                |_| true,
                |_, rows, impulses| decode(scale, rows, Some(&short[..]), impulses),
            )
            .unwrap_err();
        assert!(error.contains("smaller than the bodies"), "{error}");
    }
}
