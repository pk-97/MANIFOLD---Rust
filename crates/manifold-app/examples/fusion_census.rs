//! Print the fusion refusal census over every bundled preset plus the
//! project fixture's embedded presets, or write it to `--out <path>`.
//!
//! ```text
//! cargo run -p manifold-app --example fusion_census [-- --out <path>]
//! ```
//!
//! An example rather than a `graph-tool` verb because loading the `.manifold`
//! fixture needs `manifold-io`, which this crate only takes as a
//! dev-dependency.

use manifold_nodes as _;
use std::path::PathBuf;

use manifold_node_engine::freeze::region::census::{FixtureCorpus, build_census_report};

const FIXTURE: &str = "projects/Right Where I Need You - Music Video V5.manifold";

/// The fixtures are gitignored, so a worktree checkout lacks them; resolve
/// through `--git-common-dir` to the main checkout first.
fn fixture_path() -> Option<PathBuf> {
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        && out.status.success()
        && let Ok(common) =
            PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).canonicalize()
        && let Some(main_root) = common.parent()
    {
        let candidate = main_root.join("tests/fixtures").join(FIXTURE);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    let local = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(FIXTURE);
    local.exists().then_some(local)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .iter()
        .position(|a| a == "--out")
        .map(|i| PathBuf::from(args.get(i + 1).expect("--out needs a path")));

    let fixture = match fixture_path() {
        None => FixtureCorpus::NotFound,
        Some(path) => match manifold_io::loader::load_project(&path) {
            Err(e) => FixtureCorpus::LoadFailed(format!("{e:?}")),
            Ok(project) => FixtureCorpus::Loaded {
                embedded: project
                    .embedded_presets
                    .into_iter()
                    .map(|p| p.def)
                    .collect(),
            },
        },
    };

    let report = build_census_report(fixture);
    match out {
        Some(out) => {
            std::fs::write(&out, &report)
                .unwrap_or_else(|e| panic!("write {}: {e}", out.display()));
            eprintln!("[census] wrote {}", out.display());
        }
        None => print!("{report}"),
    }
}
