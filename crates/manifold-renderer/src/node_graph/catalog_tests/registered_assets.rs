use std::path::PathBuf;
use manifold_node_engine::load::preset_loader::PresetAssetsRoot;

mod tests {
    use super::*;

    #[test]
    fn registered_dev_assets_hold_catalog_preset_thumbnails() {
        assert_eq!(inventory::iter::<PresetAssetsRoot>.into_iter().count(), 1);
        let root = manifold_node_engine::load::preset_loader::registered_assets_root()
            .expect("renderer must register its development assets");
        assert_eq!(root, PathBuf::from(crate::testkit::assets::CATALOG_ASSETS_ROOT));
        let bloom = root.join("preset-thumbnails/effects/Bloom.png");
        assert!(bloom.is_file(), "catalog thumbnail missing: {}", bloom.display());
        assert_eq!(
            manifold_compositor::preset_thumbnail::factory_thumbnail_path(
                manifold_core::preset_def::PresetKind::Effect, "Bloom",
            ),
            Some(bloom),
        );
    }

}
