//! A Terrain's layered ground (`look_terrain.wgsl`): the uniform its draw
//! binds at 69 and the six layer maps at 70..75.

use manifold_node_engine::bindings::NodeInputs;
use manifold_node_engine::scene::scene_object::TerrainLayers;

/// Matches `TerrainLook` in `look_terrain.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct TerrainLook {
    /// x on, y cliff tile size, z peak tile size (metres).
    pub on_tiles: [f32; 4],
    /// x steepness where the cliff starts, y where it is whole.
    pub cliff: [f32; 4],
    /// x snow line (world metres), y its softness, z steepness past which
    /// snow slides off.
    pub snow: [f32; 4],
    /// x, y, z: whether the cliff's base colour, normal and
    /// metallic-roughness maps are wired.
    pub cliff_maps: [f32; 4],
    /// The same for the peak.
    pub peak_maps: [f32; 4],
}

/// The draw's layer maps (cliff colour, normal, metallic-roughness, then the
/// peak's) and its uniform. Off, with no maps, for every object but a
/// Terrain.
pub(super) fn resolve<'ctx>(
    layers: Option<&TerrainLayers>,
    inputs: NodeInputs<'ctx>,
) -> ([Option<&'ctx manifold_gpu::GpuTexture>; 6], TerrainLook) {
    let Some(layers) = layers else {
        return ([None; 6], TerrainLook::default());
    };
    let maps = [
        layers.maps[0][0],
        layers.maps[0][1],
        layers.maps[0][2],
        layers.maps[1][0],
        layers.maps[1][1],
        layers.maps[1][2],
    ]
    .map(|slot| slot.and_then(|s| inputs.texture_2d_slot(s)));
    let present = |i: usize| if maps[i].is_some() { 1.0 } else { 0.0 };
    let look = TerrainLook {
        on_tiles: [1.0, layers.tile[0].max(0.01), layers.tile[1].max(0.01), 0.0],
        cliff: [layers.cliff_slope[0], layers.cliff_slope[1], 0.0, 0.0],
        snow: [
            layers.snow_line,
            layers.snow_softness.max(0.01),
            layers.snow_slope,
            0.0,
        ],
        cliff_maps: [present(0), present(1), present(2), 0.0],
        peak_maps: [present(3), present(4), present(5), 0.0],
    };
    (maps, look)
}
