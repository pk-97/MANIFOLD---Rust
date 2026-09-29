//! Regenerate `docs/fusion_census.md`, the fusion refusal census
//! (FUSION_SOTA P3 / D4), over every bundled preset plus the Liveschool
//! fixture's embedded presets.
//!
//! ```text
//! cargo run -p manifold-renderer --example fusion_census [-- --out <path>]
//! ```
//!
//! An example rather than a `graph-tool` verb because loading the `.manifold`
//! fixture needs `manifold-io`, which this crate only takes as a
//! dev-dependency.

use std::path::PathBuf;

use manifold_renderer::node_graph::freeze::region::census::{FixtureCorpus, build_census_report};

const FIXTURE: &str = "Liveschool Live Show V6 LEDS.manifold";

/// The fixtures are gitignored, so a worktree checkout lacks them; resolve
/// through `--git-common-dir` to the main checkout first.
fn fixture_path() -> Option<PathBuf> {
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        && out.status.success()
        && let Ok(common) = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).canonicalize()
        && let Some(main_root) = common.parent()
    {
        let candidate = main_root.join("tests/fixtures").join(FIXTURE);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(FIXTURE);
    local.exists().then_some(local)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = match args.iter().position(|a| a == "--out") {
        Some(i) => PathBuf::from(args.get(i + 1).expect("--out needs a path")),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/fusion_census.md"),
    };

    let fixture = match fixture_path() {
        None => FixtureCorpus::NotFound,
        Some(path) => match manifold_io::loader::load_project(&path) {
            Err(e) => FixtureCorpus::LoadFailed(format!("{e:?}")),
            Ok(project) => FixtureCorpus::Loaded {
                embedded: project.embedded_presets.into_iter().map(|p| p.def).collect(),
            },
        },
    };

    let report = build_census_report(fixture);
    std::fs::write(&out, &report).unwrap_or_else(|e| panic!("write {}: {e}", out.display()));
    eprintln!("[census] wrote {}", out.display());
}
