//! `graph_tool` — CLI verbs over the `validate_def`/fusion seams for agents
//! authoring graph JSON (GRAPH_TOOLING_DESIGN D2).
//!
//! ```text
//! graph_tool validate <file.json> --kind effect|generator [--json]
//! graph_tool fusion <file.json> [--json]
//! graph_tool migrate <file.json> [--in-place]
//! ```
//!
//! `validate` runs the file through the exact load + compile pipeline
//! the runtime loader takes ([`validate_def`] — the same function
//! `check_presets` walks bundled dirs with). Default output is
//! human-readable; `--json` prints the [`ValidationReport`] as
//! `serde_json` for machine consumers (an agent, or later the MCP
//! `validate_graph` tool per D7).
//!
//! `fusion` runs the def through the exact flatten + partition the freeze
//! pipeline itself uses ([`fusion_report`] — GRAPH_TOOLING_DESIGN P3, D10)
//! and prints per-node classification, region membership, cut reasons, and
//! an estimated dispatch count.
//!
//! `migrate` runs [`manifold_core::scene_object_migration::migrate_scene_object_wires`]
//! (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D5) over the file — the same
//! structural, version-gate-free rewrite `instantiate_def` applies at every
//! load path — and prints the (possibly unchanged) def to stdout, or writes
//! it back to `<file.json>` with `--in-place`. Used to regenerate the
//! in-repo bundled/reference preset JSON so the checked-in files carry the
//! modern `object_k` wiring instead of relying on the load-time migration
//! to paper over the legacy shape forever.
//!
//! Exit codes: `0` valid / report produced, `1` invalid graph (`validate`
//! only — errors present), `2` usage / file-read / parse failure.

use std::path::PathBuf;
use std::process::ExitCode;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_gpu::GpuDevice;
use manifold_renderer::node_graph::{PrimitiveRegistry, ValidateKind, fusion_report, validate_def};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some((verb, rest)) = args.split_first() else {
        print_usage();
        return ExitCode::from(2);
    };

    match verb.as_str() {
        "validate" => run_validate(rest),
        "fusion" => run_fusion(rest),
        "migrate" => run_migrate(rest),
        "render" => run_render(rest),
        "-h" | "--help" | "help" => {
            print_usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("error: unknown verb '{other}'\n");
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn print_usage() {
    eprintln!(
        "usage: graph_tool validate <file.json> --kind effect|generator [--json]\n\
         \x20\x20\x20\x20\x20\x20\x20graph_tool fusion <file.json> [--json]\n\
         \x20\x20\x20\x20\x20\x20\x20graph_tool migrate <file.json> [--in-place]\n\
         \x20\x20\x20\x20\x20\x20\x20graph_tool render <file.json> --kind effect|generator [--size N] [--out out.png]\n\
         \n\
         validate: runs a graph document JSON file through the same load +\n\
         compile pipeline the runtime loader takes. Exit codes: 0 valid,\n\
         1 invalid, 2 usage/parse error.\n\
         \n\
         fusion: reports per-node fusion classification, region membership,\n\
         cut reasons, and an estimated dispatch count, using the exact\n\
         flatten + partition the freeze pipeline itself runs.\n\
         \n\
         migrate: rewrites node.render_scene's legacy per-object port wiring\n\
         into node.scene_object nodes feeding object_k ports (structural,\n\
         idempotent, no version gate). Prints the migrated JSON to stdout;\n\
         --in-place overwrites <file.json>. A def with nothing to migrate is\n\
         printed/written unchanged."
    );
}

fn run_validate(args: &[String]) -> ExitCode {
    let mut file: Option<PathBuf> = None;
    let mut kind: Option<ValidateKind> = None;
    let mut json_output = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--kind" => {
                let Some(val) = args.get(i + 1) else {
                    eprintln!("error: --kind requires a value (effect|generator)");
                    return ExitCode::from(2);
                };
                kind = match val.as_str() {
                    "effect" => Some(ValidateKind::Effect),
                    "generator" => Some(ValidateKind::Generator),
                    other => {
                        eprintln!("error: unknown --kind '{other}' (want effect|generator)");
                        return ExitCode::from(2);
                    }
                };
                i += 2;
            }
            "--json" => {
                json_output = true;
                i += 1;
            }
            positional if file.is_none() => {
                file = Some(PathBuf::from(positional));
                i += 1;
            }
            other => {
                eprintln!("error: unexpected argument '{other}'");
                return ExitCode::from(2);
            }
        }
    }

    let Some(file) = file else {
        eprintln!("error: missing <file.json>\n");
        print_usage();
        return ExitCode::from(2);
    };
    let Some(kind) = kind else {
        eprintln!("error: missing --kind effect|generator\n");
        print_usage();
        return ExitCode::from(2);
    };

    let bytes = match std::fs::read_to_string(&file) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", file.display());
            return ExitCode::from(2);
        }
    };
    let def: EffectGraphDef = match serde_json::from_str(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {}: parse failed: {e}", file.display());
            return ExitCode::from(2);
        }
    };

    let registry = PrimitiveRegistry::with_builtin();
    let device = std::sync::Arc::new(GpuDevice::new());
    let report = validate_def(&def, &registry, kind, &device);

    if json_output {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("error: failed to serialize report: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        print_human(&file, &report);
    }

    if report.is_valid() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `render <file.json> --kind effect|generator [--size N] [--out out.png]`
/// — headless warmed-up render of a graph document through the exact
/// `preset_thumbnail` path the browser thumbnails use (60 warmup frames,
/// state committed per frame, effects fed the standard source fixture).
/// The look-probe verb: author JSON, render it, Read the PNG.
fn run_render(args: &[String]) -> ExitCode {
    let mut file: Option<PathBuf> = None;
    let mut kind: Option<manifold_core::preset_def::PresetKind> = None;
    let mut size: u32 = 512;
    let mut out: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--kind" => {
                let Some(val) = args.get(i + 1) else {
                    eprintln!("error: --kind requires a value (effect|generator)");
                    return ExitCode::from(2);
                };
                kind = match val.as_str() {
                    "effect" => Some(manifold_core::preset_def::PresetKind::Effect),
                    "generator" => Some(manifold_core::preset_def::PresetKind::Generator),
                    other => {
                        eprintln!("error: unknown --kind '{other}' (want effect|generator)");
                        return ExitCode::from(2);
                    }
                };
                i += 2;
            }
            "--size" => {
                let Some(val) = args.get(i + 1).and_then(|v| v.parse::<u32>().ok()) else {
                    eprintln!("error: --size requires an integer pixel value");
                    return ExitCode::from(2);
                };
                size = val;
                i += 2;
            }
            "--out" => {
                let Some(val) = args.get(i + 1) else {
                    eprintln!("error: --out requires a path");
                    return ExitCode::from(2);
                };
                out = Some(PathBuf::from(val));
                i += 2;
            }
            positional if file.is_none() => {
                file = Some(PathBuf::from(positional));
                i += 1;
            }
            other => {
                eprintln!("error: unexpected argument '{other}'");
                return ExitCode::from(2);
            }
        }
    }

    let (Some(file), Some(kind)) = (file, kind) else {
        eprintln!("error: render needs <file.json> and --kind effect|generator");
        return ExitCode::from(2);
    };
    let out = out.unwrap_or_else(|| file.with_extension("png"));

    let bytes = match std::fs::read_to_string(&file) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", file.display());
            return ExitCode::from(2);
        }
    };
    let def: EffectGraphDef = match serde_json::from_str(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {}: parse failed: {e}", file.display());
            return ExitCode::from(2);
        }
    };

    let device = std::sync::Arc::new(GpuDevice::new());
    match manifold_renderer::preset_thumbnail::render_preset_thumbnail_to_file(
        &device, kind, &def, size, &out,
    ) {
        Ok(()) => {
            println!("rendered {} -> {}", file.display(), out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: render failed: {e}");
            ExitCode::from(1)
        }
    }
}

fn print_human(file: &std::path::Path, report: &manifold_renderer::node_graph::ValidationReport) {
    if report.is_valid() && report.warnings.is_empty() {
        println!("OK {}", file.display());
        return;
    }
    if report.is_valid() {
        println!("OK {} (with warnings)", file.display());
    } else {
        println!("FAIL {}", file.display());
    }
    for issue in &report.errors {
        println!("  ERROR {}", format_issue(issue));
    }
    for issue in &report.warnings {
        println!("  WARN  {}", format_issue(issue));
    }
}

fn format_issue(issue: &manifold_renderer::node_graph::ValidationIssue) -> String {
    let mut loc = String::new();
    if let Some(node_id) = issue.node_id {
        loc.push_str(&format!("node {node_id}"));
    }
    if let Some(type_id) = &issue.type_id {
        if !loc.is_empty() {
            loc.push(' ');
        }
        loc.push_str(&format!("({type_id})"));
    }
    if let Some(port) = &issue.port {
        if !loc.is_empty() {
            loc.push('.');
        }
        loc.push_str(port);
    }
    if loc.is_empty() {
        issue.message.clone()
    } else {
        format!("{loc}: {}", issue.message)
    }
}

fn run_fusion(args: &[String]) -> ExitCode {
    let mut file: Option<PathBuf> = None;
    let mut json_output = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                json_output = true;
                i += 1;
            }
            positional if file.is_none() => {
                file = Some(PathBuf::from(positional));
                i += 1;
            }
            other => {
                eprintln!("error: unexpected argument '{other}'");
                return ExitCode::from(2);
            }
        }
    }

    let Some(file) = file else {
        eprintln!("error: missing <file.json>\n");
        print_usage();
        return ExitCode::from(2);
    };

    let bytes = match std::fs::read_to_string(&file) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", file.display());
            return ExitCode::from(2);
        }
    };
    let def: EffectGraphDef = match serde_json::from_str(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {}: parse failed: {e}", file.display());
            return ExitCode::from(2);
        }
    };

    let registry = PrimitiveRegistry::with_builtin();
    let report = fusion_report(&def, &registry);

    if json_output {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("error: failed to serialize report: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        print_fusion_human(&file, &report);
    }

    ExitCode::SUCCESS
}

fn print_fusion_human(
    file: &std::path::Path,
    report: &manifold_renderer::node_graph::FusionReport,
) {
    println!(
        "{}: {} node(s), {} region(s), estimated {} dispatch(es)",
        file.display(),
        report.nodes.len(),
        report.regions.len(),
        report.estimated_dispatch_count
    );
    for (idx, region) in report.regions.iter().enumerate() {
        println!(
            "  region {idx}: members={:?} externals={} outputs={}",
            region.member_node_ids, region.external_count, region.output_count
        );
    }
    for node in &report.nodes {
        let membership = match node.region_index {
            Some(idx) => format!("region {idx}"),
            None => "unfused".to_string(),
        };
        println!(
            "  node {} ({}) [{}] — {membership}",
            node.node_id, node.type_id, node.kind
        );
        if let Some(reason) = &node.cut_reason {
            println!("    cut: {reason}");
        }
    }
}

fn run_migrate(args: &[String]) -> ExitCode {
    let mut file: Option<PathBuf> = None;
    let mut in_place = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--in-place" => {
                in_place = true;
                i += 1;
            }
            positional if file.is_none() => {
                file = Some(PathBuf::from(positional));
                i += 1;
            }
            other => {
                eprintln!("error: unexpected argument '{other}'");
                return ExitCode::from(2);
            }
        }
    }

    let Some(file) = file else {
        eprintln!("error: missing <file.json>\n");
        print_usage();
        return ExitCode::from(2);
    };

    let bytes = match std::fs::read_to_string(&file) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", file.display());
            return ExitCode::from(2);
        }
    };
    let mut def: EffectGraphDef = match serde_json::from_str(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {}: parse failed: {e}", file.display());
            return ExitCode::from(2);
        }
    };

    let changed = manifold_core::scene_object_migration::migrate_scene_object_wires(&mut def);

    let out = match serde_json::to_string_pretty(&def) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to serialize migrated def: {e}");
            return ExitCode::from(2);
        }
    };

    if in_place {
        if let Err(e) = std::fs::write(&file, format!("{out}\n")) {
            eprintln!("error: cannot write {}: {e}", file.display());
            return ExitCode::from(2);
        }
        eprintln!(
            "{}: {}",
            file.display(),
            if changed { "migrated" } else { "no legacy wires — unchanged" }
        );
    } else {
        println!("{out}");
    }

    ExitCode::SUCCESS
}
