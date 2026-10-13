//! The four material-inspector flows (groups, looks, textures, features) run
//! through the real flow `Runner` on the CPU: same fixture, same `step`, same
//! per-frame rebuild decision (`advance_frame`), with no `RenderState`. The
//! flow JSONs live beside this file in `flow_data/` and are compiled in.
//! Their `Snapshot` steps are pixel evidence only and are dropped here,
//! because a `Snapshot` with no render state fails by contract.

use super::*;

fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("manifold-cpu-flows").join(name);
    std::fs::create_dir_all(&dir).expect("create cpu flow out dir");
    dir
}

fn run_cpu_flow(name: &str, json: &str) {
    let actions: Vec<AutomationAction> = serde_json::from_str(json).expect("flow parses");
    let mut data = super::super::fixtures::build("gltfscene").expect("gltfscene fixture");
    let (mut ui, zoom_ppb) = prepare_ui("gltfscene", &data);
    let mut runner = Runner::for_scene(&data);
    let dir = out_dir(name);
    let mut ran = 0;
    for (index, action) in actions.iter().enumerate() {
        if matches!(action, AutomationAction::Snapshot) {
            continue;
        }
        let r = runner.step(&mut ui, &mut data, zoom_ppb, index, action, &dir, &mut None);
        assert_ne!(r.status, "fail", "{name} step {index} {}: {}", r.action, r.detail);
        ran += 1;
    }
    assert!(ran > 0, "{name}: no steps ran");
}

#[test]
fn material_inspector_groups_flow_runs_on_cpu() {
    run_cpu_flow("groups", include_str!("flow_data/material-inspector-groups.json"));
}

#[test]
fn material_inspector_looks_flow_runs_on_cpu() {
    run_cpu_flow("looks", include_str!("flow_data/material-inspector-looks.json"));
}

#[test]
fn material_inspector_textures_flow_runs_on_cpu() {
    run_cpu_flow("textures", include_str!("flow_data/material-inspector-textures.json"));
}

#[test]
fn material_inspector_features_flow_runs_on_cpu() {
    run_cpu_flow("features", include_str!("flow_data/material-inspector-features.json"));
}

#[test]
fn snapshot_without_render_state_fails() {
    let mut data = super::super::fixtures::build("gltfscene").expect("gltfscene fixture");
    let (mut ui, zoom_ppb) = prepare_ui("gltfscene", &data);
    let mut runner = Runner::for_scene(&data);
    let dir = out_dir("snapshot");
    let r = runner.step(&mut ui, &mut data, zoom_ppb, 0, &AutomationAction::Snapshot, &dir, &mut None);
    assert_eq!(r.status, "fail", "{}", r.detail);
}
