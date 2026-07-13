//! `graph_tool` — CLI verbs over the `validate_def` seam for agents
//! authoring graph JSON (GRAPH_TOOLING_DESIGN D2).
//!
//! P1 ships one verb:
//!
//! ```text
//! graph_tool validate <file.json> --kind effect|generator [--json]
//! ```
//!
//! `validate` runs the file through the exact load + compile pipeline
//! the runtime loader takes ([`validate_def`] — the same function
//! `check_presets` walks bundled dirs with). Default output is
//! human-readable; `--json` prints the [`ValidationReport`] as
//! `serde_json` for machine consumers (an agent, or later the MCP
//! `validate_graph` tool per D7).
//!
//! Exit codes: `0` valid, `1` invalid graph (errors present), `2`
//! usage / file-read / parse failure.

use std::path::PathBuf;
use std::process::ExitCode;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_gpu::GpuDevice;
use manifold_renderer::node_graph::{PrimitiveRegistry, ValidateKind, validate_def};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some((verb, rest)) = args.split_first() else {
        print_usage();
        return ExitCode::from(2);
    };

    match verb.as_str() {
        "validate" => run_validate(rest),
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
         \n\
         Validates a graph document JSON file through the same load +\n\
         compile pipeline the runtime loader takes. Exit codes: 0 valid,\n\
         1 invalid, 2 usage/parse error."
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
    let device = GpuDevice::new();
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
