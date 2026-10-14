//! Family curation of scene parameters used while constructing graphs.
use manifold_core::scene_exposure::NodeParamMetadata;

pub struct SceneExposureSource {
    pub metadata: fn(&str) -> Vec<NodeParamMetadata>,
}
inventory::collect!(SceneExposureSource);

fn source() -> &'static SceneExposureSource {
    let mut sources = inventory::iter::<SceneExposureSource>.into_iter();
    let source = sources.next().expect("scene exposure provider must be linked");
    assert!(sources.next().is_none(), "duplicate scene exposure provider");
    source
}

pub fn metadata_for_node_type(type_id: &str) -> Vec<NodeParamMetadata> {
    (source().metadata)(type_id)
}
