use manifold_io::loader;

fn fixture_path(name: &str) -> std::path::PathBuf {
    // The `.manifold` fixtures are gitignored (large personal projects), so a
    // `git worktree` checkout doesn't contain them. Resolve to the MAIN working
    // tree: `--git-common-dir` points at the primary repo's `.git`, whose parent
    // is the main checkout where the fixtures live — so these tests RUN from a
    // worktree instead of panicking with a confusing file-not-found. Falls back
    // to the crate-relative path (the main checkout, or if git isn't reachable).
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .output()
        && out.status.success()
        && let Ok(common) =
            std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).canonicalize()
        && let Some(main_root) = common.parent()
    {
        let candidate = main_root.join("tests/fixtures").join(name);
        if candidate.exists() {
            return candidate;
        }
    }
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../tests/fixtures");
    p.push(name);
    p
}

// Right Where I Need You - Music Video V5, a recent music video (18 layers, 1110
// clips) saved with 8-char ids before session mode existed. Local and gitignored,
// like every personal fixture. It is the one real project that pins what only an
// old saved file can: a re-save keeps every id it read and adds no empty session
// field.
const CANONICAL_FIXTURE: &str = "projects/Right Where I Need You - Music Video V5.manifold";

/// Every id-like string (an `id` key or a key ending in `Id`), keyed by its
/// path with array indices dropped, values sorted. Index-free so a loader that
/// re-sorts clips still compares equal; sorted so the multiset is exact.
fn id_strings(root: &serde_json::Value) -> std::collections::BTreeMap<String, Vec<String>> {
    fn walk(
        v: &serde_json::Value,
        path: &str,
        out: &mut std::collections::BTreeMap<String, Vec<String>>,
    ) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, child) in map {
                    let p = format!("{path}/{k}");
                    match child {
                        serde_json::Value::String(s) if k == "id" || k.ends_with("Id") => {
                            out.entry(p).or_default().push(s.clone());
                        }
                        _ => walk(child, &p, out),
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, &format!("{path}/#"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, "", &mut out);
    for values in out.values_mut() {
        values.sort();
    }
    out
}

fn archive_project_json(path: &std::path::Path) -> serde_json::Value {
    use std::io::Read;
    let file = std::fs::File::open(path).expect("open archive");
    let mut archive = zip::ZipArchive::new(file).expect("read archive");
    let mut json = String::new();
    archive
        .by_name("project.json")
        .expect("archive has project.json")
        .read_to_string(&mut json)
        .expect("read project.json");
    serde_json::from_str(&json).expect("parse project.json")
}

/// One load of the canonical fixture serves two re-save contracts.
///
/// Clip ids: new ids are 64-bit, old files keep their 32-bit ids. Saving the
/// fixture through the real saver must write back every existing id exactly as
/// it was read: re-keying would break the references between clips, layers and
/// effects.
///
/// Session mode added `Project.session:
/// SessionGrid`, skipped on serialize when empty. This fixture predates
/// session mode, so re-saving it must emit no `"session"` key at all — not an
/// empty one — and a project lacking the field on disk defaults it cleanly.
#[test]
fn music_video_resave_keeps_old_ids_and_omits_empty_session() {
    let path = fixture_path(CANONICAL_FIXTURE);
    assert!(path.exists(), "Test fixture not found: {}", path.display());

    let before = id_strings(&archive_project_json(&path));
    let clip_ids = &before["/timeline/layers/#/clips/#/id"];
    assert_eq!(clip_ids.len(), 1110);
    assert!(
        clip_ids.iter().all(|id| id.len() == 8),
        "the fixture is meant to carry old 8-char ids"
    );

    let mut project = loader::load_project(&path).expect("load music video project");

    assert!(
        project.session.is_empty(),
        "legacy fixture has no session data; session grid must default empty on load"
    );
    let json = serde_json::to_string(&project).expect("serialize music video project");
    assert!(
        !json.contains("\"session\""),
        "empty SessionGrid must be skipped on serialize, not emitted as {{}}"
    );
    // Re-parsing the re-saved JSON must still round-trip to an empty grid —
    // confirms the field defaults cleanly when entirely absent from disk.
    let project2 =
        loader::load_project_from_json(&json).expect("reload re-saved music video project");
    assert!(project2.session.is_empty());

    let new_layer = manifold_core::layer::Layer::new(
        "wide id".into(),
        manifold_core::types::LayerType::Video,
        project.timeline.layers.len() as i32,
    );
    let new_id = new_layer.layer_id.to_string();
    assert_eq!(new_id.len(), 16, "a freshly minted id carries 64 bits");
    project.timeline.layers.push(new_layer);

    let dir = std::env::temp_dir().join(format!("manifold-wide-ids-{}", std::process::id()));
    let out = dir.join("music_video.manifold");
    manifold_io::saver::save_project(&mut project, &out, None, false).expect("save");
    let mut after = id_strings(&archive_project_json(&out));
    let reloaded = loader::load_project(&out).expect("reload saved project");
    let _ = std::fs::remove_dir_all(&dir);

    let layer_ids = after
        .get_mut("/timeline/layers/#/layerId")
        .expect("layer ids");
    let pos = layer_ids
        .iter()
        .position(|id| *id == new_id)
        .expect("new layer id saved");
    layer_ids.remove(pos);
    for (key, ids) in &before {
        assert_eq!(after.get(key), Some(ids), "ids under {key} changed on save");
    }
    assert!(
        reloaded
            .timeline
            .layers
            .iter()
            .any(|l| l.layer_id.as_str() == new_id),
        "a 16-char id reloads as itself"
    );
}
