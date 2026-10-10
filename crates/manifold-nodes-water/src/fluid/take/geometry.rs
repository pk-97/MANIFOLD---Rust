//! Compare current prepared physics assets with the authenticated take setup.
//! This is worker preflight, never a render-thread scan or filesystem watcher.

use super::{FluidTakeReplay, Request, coupled, roles};

/// Borrow the immutable setup already carried by the worker request. Paths,
/// render materials and Arc addresses are not geometry identities.
pub(crate) struct PreparedGeometry<'a> {
    roles: &'a roles::Setup,
    rigid: Option<&'a coupled::Setup>,
}

impl<'a> PreparedGeometry<'a> {
    pub(in crate::fluid) fn from_request(request: &'a Request) -> Self {
        Self {
            roles: &request.role_setup,
            rigid: request.coupled.as_ref().map(|rigid| rigid.setup.as_ref()),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.roles.len() == 0 && self.rigid.is_none()
    }
}

impl FluidTakeReplay {
    /// A graph identity alone cannot detect a mesh replaced at the same path.
    /// Compare the actual prepared mesh/hull data stored in the take header.
    /// Live poses and other sampled controls belong to the recorded input
    /// history and must not be compared with their values at the current seek.
    pub(crate) fn validate_prepared_geometry(
        &self,
        current: &PreparedGeometry<'_>,
    ) -> Result<(), String> {
        if !self.reader.header.role_setup.same_geometry(current.roles) {
            return Err("Physics take: fluid role geometry changed".into());
        }
        match (self.reader.header.coupled_setup.as_deref(), current.rigid) {
            (None, None) => Ok(()),
            (Some(recorded), Some(current)) if recorded.same_geometry(current) => Ok(()),
            _ => Err("Physics take: rigid collision geometry or membership changed".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fluid::take::{Reader, Writer, tests::request};
    use crate::fluid_cache::{CacheReader, CacheWriter};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "manifold-take-geometry-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn open(directory: Arc<PathBuf>, input: &Request) -> Result<CacheReader, String> {
        CacheReader::open_for_project(
            directory,
            input.settings,
            None,
            None,
            Some(PreparedGeometry::from_request(input)),
        )
    }

    #[test]
    fn fluid_take_geometry_preflight_survives_relocation_and_fresh_preparation() {
        let directory = Directory::new();
        let old_path = Arc::new(directory.0.join("original"));
        let mut input = request();
        let mut cache =
            CacheWriter::create_for_take(Arc::clone(&old_path), input.settings).unwrap();
        let take = Writer::create(Arc::clone(&old_path), &input).unwrap();
        cache.publish_take_prefix(take.identity()).unwrap();
        drop(cache);
        drop(take);

        let moved = Arc::new(directory.0.join("collected"));
        std::fs::rename(old_path.as_ref(), moved.as_ref()).unwrap();
        assert!(!old_path.exists());
        // Independent preparation has new allocations, as on project reload.
        input.role_setup = Arc::new(
            serde_json::from_slice(&serde_json::to_vec(input.role_setup.as_ref()).unwrap())
                .unwrap(),
        );
        let rigid = input.coupled.as_mut().unwrap();
        rigid.setup = Arc::new(
            serde_json::from_slice(&serde_json::to_vec(rigid.setup.as_ref()).unwrap()).unwrap(),
        );
        assert!(open(Arc::clone(&moved), &input).is_ok());

        // Changing only the current geometry, with the same graph/settings,
        // rejects the old take. The authenticated recording is never rewritten.
        let mut setup = serde_json::to_value(input.role_setup.as_ref()).unwrap();
        setup["roles"][0]["geometry"]["meshes"][0]["triangles"][0]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
        input.role_setup = Arc::new(serde_json::from_value(setup).unwrap());
        assert!(
            open(Arc::clone(&moved), &input)
                .err()
                .unwrap()
                .contains("fluid role geometry changed")
        );
        assert!(Reader::open(moved).is_ok());
    }

    #[test]
    fn fluid_take_geometry_requires_provenance_for_scene_assets_in_legacy_caches() {
        let directory = Directory::new();
        let path = Arc::new(directory.0.join("cache"));
        let mut input = request();
        CacheWriter::create(Arc::clone(&path), input.settings).unwrap();
        assert!(
            open(Arc::clone(&path), &input)
                .err()
                .unwrap()
                .contains("committed take")
        );
        input.role_setup = Arc::default();
        assert!(open(Arc::clone(&path), &input).is_err());
        input.coupled = None;
        assert!(open(path, &input).is_ok());
    }
}
