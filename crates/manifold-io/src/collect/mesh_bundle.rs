//! Copy glTF resources as one portable asset through the existing collector.

use super::{CollectError, path_is_inside, sha256_file};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

struct Resource {
    source: PathBuf,
    relative: PathBuf,
    hash: [u8; 32],
}

pub(super) struct MeshBundle {
    source: PathBuf,
    original: Vec<u8>,
    document: Value,
    // Preserve all chunks following JSON, including opaque extension chunks.
    binary_tail: Option<usize>,
    resources: Vec<Resource>,
    relative_uris: bool,
    pub hash: [u8; 32],
}

impl MeshBundle {
    /// Self-contained models retain the ordinary byte-for-byte file path.
    /// Only formats owned by the glTF importer are inspected here; the core
    /// file-loader inventory still decides which authored assets are meshes.
    pub fn read(source: &Path) -> Result<Option<Self>, CollectError> {
        if !source
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gltf") || ext.eq_ignore_ascii_case("glb"))
        {
            return Ok(None);
        }
        let error = |message: String| {
            CollectError::Io(format!("collect model {}: {message}", source.display()))
        };
        let original = std::fs::read(source).map_err(|e| error(e.to_string()))?;
        // Match Gltf::from_slice's magic-based container detection.
        let (json, binary_tail) = if original.starts_with(b"glTF") {
            // The pinned container parser subtracts its header length before
            // checking it. Reject short/invalid lengths before that subtraction.
            let length = original
                .get(8..12)
                .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four bytes")));
            if original.len() < 20 || length.map(|n| n as usize) != Some(original.len()) {
                return Err(error("GLB length does not match its contents".into()));
            }
            let glb = gltf::binary::Glb::from_slice(&original).map_err(|e| error(e.to_string()))?;
            let end = 20 + glb.json.len();
            (&original[20..end], Some(end))
        } else {
            (original.as_slice(), None)
        };
        let mut document: Value =
            serde_json::from_slice(json).map_err(|e| error(format!("invalid glTF JSON: {e}")))?;
        if !document.is_object() {
            return Err(error("glTF document must be an object".into()));
        }
        let base = source.parent().unwrap_or_else(|| Path::new(""));
        let mut resources: Vec<Resource> = Vec::new();
        let mut by_source: HashMap<PathBuf, usize> = HashMap::new();
        let mut relative_uris = true;
        let mut digest = Sha256::new();
        digest.update(b"manifold.collect.gltf-bundle.v1");
        digest.update((original.len() as u64).to_le_bytes());
        digest.update(&original);
        // These are glTF format members, not an alternate node/parameter table.
        // Texture extensions select entries in `images`, so those resources
        // travel with the model without interpreting or dropping extensions.
        for member in ["buffers", "images"] {
            let Some(entries) = document.get_mut(member) else {
                continue;
            };
            let entries = entries
                .as_array_mut()
                .ok_or_else(|| error(format!("{member} must be an array")))?;
            for (index, entry) in entries.iter_mut().enumerate() {
                let entry = entry
                    .as_object_mut()
                    .ok_or_else(|| error(format!("{member}[{index}] must be an object")))?;
                let Some(value) = entry.get_mut("uri") else {
                    continue;
                };
                let uri = value
                    .as_str()
                    .ok_or_else(|| error(format!("{member}[{index}].uri must be a string")))?;
                if uri.starts_with("data:") {
                    continue;
                }
                let (path, relative) = resource_path(base, uri).map_err(&error)?;
                relative_uris &= relative;
                let canonical = std::fs::canonicalize(&path)
                    .map_err(|e| error(format!("resource {uri:?} ({}): {e}", path.display())))?;
                if !canonical.is_file() {
                    return Err(error(format!("resource {uri:?} is not a file")));
                }
                let resource_index = if let Some(index) = by_source.get(&canonical) {
                    *index
                } else {
                    let index = resources.len();
                    let name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or_else(|| error(format!("resource {uri:?} has no UTF-8 filename")))?;
                    let relative = PathBuf::from("resources")
                        .join(index.to_string())
                        .join(name);
                    let hash = sha256_file(&canonical)?;
                    resources.push(Resource {
                        source: canonical.clone(),
                        relative,
                        hash,
                    });
                    by_source.insert(canonical, index);
                    index
                };
                let resource = &resources[resource_index];
                // Include every URI occurrence. Distinct alias arrangements
                // may have the same ordered set of unique resource hashes.
                digest.update(resource.hash);
                let name = resource
                    .relative
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("resource filename was checked above");
                *value = Value::String(format!(
                    "resources/{resource_index}/{}",
                    urlencoding::encode(name)
                ));
            }
        }
        if resources.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            source: source.to_path_buf(),
            original,
            document,
            binary_tail,
            resources,
            relative_uris,
            hash: digest.finalize().into(),
        }))
    }

    pub fn is_portable_in(&self, project: &Path) -> bool {
        self.relative_uris
            && self
                .resources
                .iter()
                .all(|r| path_is_inside(&r.source, project))
    }

    /// The caller reserves a new directory. Publish the new authored path only
    /// after every dependency and the rewritten document have been written.
    pub fn copy_to(&self, directory: &Path) -> Result<(PathBuf, u64), CollectError> {
        let error = |message: String| {
            CollectError::Io(format!(
                "collect model {}: {message}",
                self.source.display()
            ))
        };
        let mut bytes = 0;
        for resource in &self.resources {
            let target = directory.join(&resource.relative);
            std::fs::create_dir_all(target.parent().expect("resource directory"))
                .map_err(|e| error(format!("create {}: {e}", target.display())))?;
            bytes += std::fs::copy(&resource.source, &target)
                .map_err(|e| error(format!("copy {}: {e}", resource.source.display())))?;
            // A source edited during collection must not produce a bundle
            // associated with another snapshot's deduplication identity.
            if sha256_file(&target)? != resource.hash {
                return Err(error(format!(
                    "resource changed during collection: {}",
                    resource.source.display()
                )));
            }
        }
        let json = serde_json::to_vec(&self.document).map_err(|e| error(e.to_string()))?;
        let target = directory.join(self.source.file_name().expect("model is a file"));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| error(format!("create {}: {e}", target.display())))?;
        let written = if let Some(tail) = self.binary_tail {
            let padded_json_length = json
                .len()
                .checked_add(3)
                .map(|n| n & !3)
                .ok_or_else(|| error("GLB JSON length overflow".into()))?;
            let length = 20usize
                .checked_add(padded_json_length)
                .and_then(|n| n.checked_add(self.original.len() - tail))
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| error("collected GLB exceeds its 32-bit length limit".into()))?;
            let mut write = || -> std::io::Result<()> {
                file.write_all(&self.original[..8])?;
                file.write_all(&length.to_le_bytes())?;
                file.write_all(&(padded_json_length as u32).to_le_bytes())?;
                file.write_all(b"JSON")?;
                file.write_all(&json)?;
                file.write_all(&b"   "[..padded_json_length - json.len()])?;
                file.write_all(&self.original[tail..])
            };
            write().map_err(|e| error(format!("write {}: {e}", target.display())))?;
            u64::from(length)
        } else {
            file.write_all(&json)
                .map_err(|e| error(format!("write {}: {e}", target.display())))?;
            json.len() as u64
        };
        Ok((target, bytes + written))
    }
}

/// Keep the pinned glTF importer's URI semantics. It percent-decodes relative
/// URIs but reads `file:` paths literally and does not implement authorities.
/// Collected URIs are always relative, even for already-local file URLs.
fn resource_path(base: &Path, uri: &str) -> Result<(PathBuf, bool), String> {
    if uri.contains(':') {
        let file = uri
            .strip_prefix("file://")
            .or_else(|| uri.strip_prefix("file:"))
            .ok_or_else(|| format!("unsupported resource URI {uri:?}"))?;
        Ok((PathBuf::from(file), false))
    } else {
        let decoded =
            urlencoding::decode(uri).map_err(|e| format!("invalid resource URI {uri:?}: {e}"))?;
        let path = Path::new(decoded.as_ref());
        Ok((base.join(path), !path.is_absolute()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_resource_rejects_unpublished_bundle() {
        let root = std::env::temp_dir().join(format!(
            "manifold-collect-resource-change-{}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("model.gltf");
        let resource = root.join("mesh.bin");
        std::fs::write(
            &source,
            br#"{"asset":{"version":"2.0"},"buffers":[{"uri":"mesh.bin","byteLength":3}]}"#,
        )
        .unwrap();
        std::fs::write(&resource, b"old").unwrap();
        let bundle = MeshBundle::read(&source).unwrap().unwrap();
        std::fs::write(&resource, b"new").unwrap();
        let destination = root.join("collected");
        std::fs::create_dir(&destination).unwrap();
        let error = bundle.copy_to(&destination).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("resource changed during collection"),
            "{error}"
        );
        assert!(!destination.join("model.gltf").exists());
        assert_eq!(std::fs::read(resource).unwrap(), b"new");
        std::fs::remove_dir_all(root).unwrap();
    }
}
