//! Identity of parsed animation inputs, independent of URI spelling and JSON layout.

use super::gltf_anim_cache::{ChannelKind, GltfAnimSet};
use super::gltf_load::GltfInterp;
use sha2::{Digest, Sha256};

inventory::submit! {
    super::fluid::identity::PhysicsSourceIdentity {
        name: "gltf_animation",
        identity: env!("MANIFOLD_PHYSICS_FAMILY_IDENTITY"),
    }
}


pub(super) fn identity(set: &GltfAnimSet) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"manifold.loaded-gltf-animation.v1");
    len(&mut hash, set.clips.len());
    for clip in &set.clips {
        hash.update(clip.duration_s.to_le_bytes());
        len(&mut hash, clip.channels.len());
        for channel in &clip.channels {
            hash.update(channel.target_node.to_le_bytes());
            match channel.kind {
                ChannelKind::Translation => hash.update([0]),
                ChannelKind::Rotation => hash.update([1]),
                ChannelKind::Scale => hash.update([2]),
                ChannelKind::Weights { target_count } => {
                    hash.update([3]);
                    hash.update(target_count.to_le_bytes());
                }
            }
            hash.update([match channel.mode {
                GltfInterp::Linear => 0,
                GltfInterp::Step => 1,
                GltfInterp::CubicSpline => 2,
            }]);
            floats(&mut hash, &channel.times);
            floats(&mut hash, &channel.values);
            floats(&mut hash, &channel.in_tangents);
            floats(&mut hash, &channel.out_tangents);
        }
    }
    len(&mut hash, set.skins.len());
    for skin in &set.skins {
        len(&mut hash, skin.joint_node_indices.len());
        for value in &skin.joint_node_indices {
            hash.update(value.to_le_bytes());
        }
        len(&mut hash, skin.joint_parent.len());
        for value in &skin.joint_parent {
            hash.update(value.to_le_bytes());
        }
        for matrices in [&skin.joint_root_world, &skin.inverse_bind_matrices] {
            len(&mut hash, matrices.len());
            for matrix in matrices {
                for column in matrix {
                    floats(&mut hash, column);
                }
            }
        }
    }
    len(&mut hash, set.node_parents.len());
    for value in &set.node_parents {
        hash.update(value.to_le_bytes());
    }
    len(&mut hash, set.node_bind_trs.len());
    for bind in &set.node_bind_trs {
        floats(&mut hash, &bind.translation);
        floats(&mut hash, &bind.rotation);
        floats(&mut hash, &bind.scale);
    }
    hash.finalize().into()
}

fn len(hash: &mut Sha256, count: usize) {
    hash.update((count as u64).to_le_bytes());
}

fn floats(hash: &mut Sha256, values: &[f32]) {
    len(hash, values.len());
    for value in values {
        hash.update(value.to_le_bytes());
    }
}

#[cfg(test)]
mod source_identity_tests {
    #[test]
    fn physics_source_identity_registration_matches_family_build() {
        let entries: Vec<_> = inventory::iter::<super::super::fluid::identity::PhysicsSourceIdentity>
            .into_iter().filter(|entry| entry.name == "gltf_animation").collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].identity, env!("MANIFOLD_PHYSICS_FAMILY_IDENTITY"));
    }
}
