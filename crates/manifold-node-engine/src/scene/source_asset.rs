//! Read-only provenance of the asset snapshot actually held by a node.
//! Loading and hashing belong to existing source workers, never frame sampling.

use crate::{parameters::ParamValue, exec::effect_node::ParamValues};

/// Engine implementation inputs that invalidate recorded simulation results.
#[cfg(feature = "gpu-proofs")]
pub const ENGINE_SOURCE_IDENTITY: &str = env!("MANIFOLD_PHYSICS_INTEGRATION_IDENTITY");

/// Family implementation inputs that invalidate recorded simulation results.
#[cfg(feature = "gpu-proofs")]
pub struct SourceImplementationIdentity {
    pub name: &'static str,
    pub identity: &'static str,
}

#[cfg(feature = "gpu-proofs")]
inventory::collect!(SourceImplementationIdentity);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAssetIdentity<'a> {
    /// This loader does not yet expose verifiable content provenance.
    Unsupported,
    Pending,
    Failed(&'a str),
    Ready([u8; 32]),
    /// Native take preflight compares this node's prepared geometry directly.
    PreparedGeometry,
}

/// Keep a decoded value and its identity together across a loader handoff.
/// Deref is read-only so a resident asset cannot outgrow its fingerprint.
#[derive(Debug)]
pub struct LoadedAsset<T> {
    value: T,
    identity: [u8; 32],
}

impl<T> LoadedAsset<T> {
    pub fn new(value: T, identity: [u8; 32]) -> Self {
        Self { value, identity }
    }

    pub fn identity(&self) -> [u8; 32] {
        self.identity
    }

    pub fn into_value(self) -> T {
        self.value
    }
}

impl<T> std::ops::Deref for LoadedAsset<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

pub fn loaded_identity<'a, T>(
    params: &ParamValues,
    path_param: &str,
    loaded_path: &str,
    loaded: Option<&LoadedAsset<T>>,
    error: Option<&'a str>,
) -> SourceAssetIdentity<'a> {
    let requested = match params.get(path_param) {
        Some(ParamValue::String(path)) => path.as_str(),
        _ => "",
    };
    if requested != loaded_path {
        return SourceAssetIdentity::Pending;
    }
    if let Some(error) = error {
        return SourceAssetIdentity::Failed(error);
    }
    if let Some(loaded) = loaded {
        SourceAssetIdentity::Ready(loaded.identity())
    } else if requested.is_empty() {
        SourceAssetIdentity::Failed("No source asset selected")
    } else {
        SourceAssetIdentity::Pending
    }
}

pub fn mesh_identity(vertices: &[crate::mesh::MeshVertex]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"manifold.loaded-mesh.v1");
    hash.update((vertices.len() as u64).to_le_bytes());
    // MeshVertex is Pod and consists solely of f32 fields. Encode each scalar
    // explicitly to keep the fingerprint independent of host byte order.
    for value in bytemuck::cast_slice::<_, f32>(vertices) {
        hash.update(value.to_le_bytes());
    }
    hash.finalize().into()
}
