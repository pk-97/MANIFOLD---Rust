//! Smoke and fire in the scene: collecting the objects that carry a gas, with
//! the scene uniforms their volume pass shades under.

use super::*;
use manifold_node_engine::scene::scene_object::GasVolume;

/// One gas to draw this frame: the solver's packed fields and the scene
/// uniforms (camera, lights, exposure, fog) its march shades with.
pub(super) struct GasDraw<'ctx> {
    pub state: &'ctx manifold_gpu::GpuBuffer,
    pub velocity: &'ctx manifold_gpu::GpuBuffer,
    pub volume: GasVolume,
    pub uniforms: RenderSceneUniforms,
}

/// Bytes of one packed field: two words a cell.
fn packed_bytes(dims: [u32; 3]) -> u64 {
    dims.iter().map(|&n| u64::from(n)).product::<u64>() * 8
}

impl RenderScene {
    /// The gas of object `n`: `Some(None)` when its fields have not landed
    /// this frame (it drops out, the rest draws), `None` when its wiring is
    /// broken (the frame fails by name).
    pub(super) fn collect_gas<'ctx>(
        ctx: &mut EffectNodeContext<'ctx, '_>,
        pre: &FramePrelude<'ctx>,
        n: usize,
        gas: GasVolume,
    ) -> Option<Option<GasDraw<'ctx>>> {
        let inputs = ctx.inputs;
        if [gas.state, gas.velocity]
            .into_iter()
            .any(|slot| !inputs.slot_content_ready(slot))
        {
            return Some(None);
        }
        let bytes = packed_bytes(gas.dims);
        let (Some(state), Some(velocity)) = (
            inputs.array_slot(gas.state),
            inputs.array_slot(gas.velocity),
        ) else {
            return Self::fail_field(
                ctx,
                n,
                format!("object_{n}: the gas's state or velocity is unavailable"),
            );
        };
        if state.size < bytes || velocity.size < bytes {
            return Self::fail_field(
                ctx,
                n,
                format!(
                    "object_{n}: the gas's fields hold {} and {} bytes, its {:?} cells need {bytes}",
                    state.size, velocity.size, gas.dims
                ),
            );
        }
        // The march works in the world: the box is axis-aligned and placed
        // by its origin, so the model is the identity.
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let material = Material::default_unlit_white();
        let uniforms = build_uniforms(
            pre.view_proj,
            identity,
            &pre.cam,
            &material,
            1.0,
            pre.light_count as f32,
            &pre.atmosphere,
            pre.prev_view_proj,
            identity,
            1.0,
            false,
            1.0,
        );
        Some(Some(GasDraw {
            state,
            velocity,
            volume: gas,
            uniforms,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::packed_bytes;

    #[test]
    fn a_packed_field_is_two_words_a_cell() {
        assert_eq!(packed_bytes([32, 48, 32]), 32 * 48 * 32 * 8);
        assert_eq!(packed_bytes([0, 48, 32]), 0);
    }
}
