//! The Materials covering a draw's own (`cover_surface` in
//! `render_scene.wgsl`): the uniform its draw binds at 69 and each cover's
//! base colour, normal and metallic-roughness maps at 73..78. Where a cover
//! lies is the draw's own material's cover rules; how it looks is the
//! covering material's own values and maps.

use manifold_node_engine::bindings::NodeInputs;
use manifold_node_engine::scene::material::{CoverRule, Material};
use manifold_node_engine::scene::scene_object::Cover;

/// One cover; matches `MaterialCover` in `render_scene.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct CoverLook {
    /// x 1 / Tile Size, y Break Repeats, z normal strength, w metallic.
    pub surface: [f32; 4],
    /// rgb base colour, a roughness.
    pub color: [f32; 4],
    /// x, y, z: 1 where the base colour, normal and metallic-roughness maps
    /// are wired; w Amount, 0 for no cover.
    pub maps: [f32; 4],
    /// Where it lies: `CoverRule::packed`.
    pub rule: [[f32; 4]; 3],
}

/// Matches `MaterialCovers` in `render_scene.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MaterialCovers {
    pub covers: [CoverLook; 2],
}

fn look(rule: &CoverRule, cover: &Material, maps: [bool; 3]) -> CoverLook {
    let flag = |on: bool| if on { 1.0 } else { 0.0 };
    CoverLook {
        surface: [
            1.0 / cover.tile_size,
            cover.break_repeats,
            cover.normal_scale,
            cover.metallic,
        ],
        color: [
            cover.base_color[0],
            cover.base_color[1],
            cover.base_color[2],
            cover.roughness,
        ],
        maps: [flag(maps[0]), flag(maps[1]), flag(maps[2]), rule.amount],
        rule: rule.packed(),
    }
}

/// The draw's cover maps (Cover 1's base colour, normal, metallic-roughness,
/// then Cover 2's) and its uniform. A cover with no material, or Amount 0,
/// is off and reads nothing.
pub(super) fn resolve<'ctx>(
    own: &Material,
    covers: &[Option<Cover>; 2],
    inputs: NodeInputs<'ctx>,
) -> ([Option<&'ctx manifold_gpu::GpuTexture>; 6], MaterialCovers) {
    let mut textures = [None; 6];
    let mut uniform = MaterialCovers::default();
    for (slot, cover) in covers.iter().enumerate() {
        let Some(cover) = cover else {
            continue;
        };
        let rule = &own.covers[slot];
        if rule.amount <= 0.0 {
            continue;
        }
        let maps = cover
            .maps
            .map(|map| map.and_then(|s| inputs.texture_2d_slot(s)));
        textures[slot * 3..slot * 3 + 3].copy_from_slice(&maps);
        uniform.covers[slot] = look(rule, &cover.material, maps.map(|m| m.is_some()));
    }
    (textures, uniform)
}
