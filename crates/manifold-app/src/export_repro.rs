//! Feature-gated reproduction of the production project export path.

use std::path::{Path, PathBuf};

use crossbeam_channel::unbounded;
use manifold_media::export_config::ExportConfig;

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::headless_harness::headless_content_thread;

#[derive(Debug, PartialEq)]
struct ExportReproArgs {
    project: PathBuf,
    output: PathBuf,
    start: f32,
    end: f32,
    width: u32,
    height: u32,
    fps: f32,
}

fn parse_args(args: &[String]) -> Result<ExportReproArgs, String> {
    let project = args
        .get(1)
        .filter(|s| !s.starts_with("--"))
        .map(PathBuf::from)
        .ok_or_else(|| "missing <project>".to_string())?;
    let value = |flag: &str| -> Result<String, String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .filter(|v| !v.starts_with("--"))
            .ok_or_else(|| format!("missing value for {flag}"))
    };
    let output = PathBuf::from(value("--output")?);
    let parse = |flag: &str| -> Result<f32, String> {
        value(flag)?
            .parse()
            .map_err(|_| format!("{flag} must be a number"))
    };
    let parse_u32 = |flag: &str| -> Result<u32, String> {
        value(flag)?
            .parse()
            .map_err(|_| format!("{flag} must be an integer"))
    };
    let result = ExportReproArgs {
        project,
        output,
        start: parse("--start")?,
        end: parse("--end")?,
        width: parse_u32("--width")?,
        height: parse_u32("--height")?,
        fps: parse("--fps")?,
    };
    if !result.start.is_finite() || !result.end.is_finite() {
        return Err("--start and --end must be finite beat values".into());
    }
    if !result.fps.is_finite() {
        return Err("--fps must be finite".into());
    }
    if result.start >= result.end {
        return Err("--start must be less than --end".into());
    }
    if result.width == 0 || result.height == 0 || result.fps <= 0.0 {
        return Err("width, height, and fps must be positive".into());
    }
    Ok(result)
}

pub fn run(args: &[String]) -> ! {
    let parsed = match parse_args(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("export-repro: {e}\nusage: export-repro <project> --output <path> --start <beat> --end <beat> --width N --height N --fps N");
            std::process::exit(2);
        }
    };
    match run_export(&parsed) {
        Ok(()) => {
            eprintln!("export-repro: success: {}", parsed.output.display());
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("export-repro: failed: {e}");
            std::process::exit(3);
        }
    }
}

fn run_export(args: &ExportReproArgs) -> Result<(), String> {
    let project = manifold_io::loader::load_project_with(
        Path::new(&args.project),
        crate::project_io::install_embedded_presets,
    )
    .map_err(|e| format!("failed to load project '{}': {e}", args.project.display()))?;
    let empty = manifold_core::project::Project::default();
    let mut ct = headless_content_thread(empty, args.width, args.height);
    let (state_tx, state_rx) = unbounded::<ContentState>();
    let output = args.output.clone();
    let drain = std::thread::Builder::new()
        .name("export-repro-drain".into())
        .spawn(move || {
            let mut finished = None;
            while let Ok(state) = state_rx.recv() {
                if let Some(event) = state.export_finished {
                    finished = Some(event);
                    break;
                }
            }
            finished
        })
        .map_err(|e| format!("failed to start export status drain: {e}"))?;
    ct.handle_command(ContentCommand::LoadProject(Box::new(project)));
    let (warm_tx, warm_rx) = unbounded::<ContentCommand>();
    ct.run_warmup(&warm_rx, &warm_tx, &state_tx);
    drop(warm_tx);
    let (cmd_tx, cmd_rx) = unbounded::<ContentCommand>();
    ct.run_export(
        ExportConfig {
            output_path: output.to_string_lossy().into_owned(),
            width: args.width,
            height: args.height,
            fps: args.fps,
            hdr: false,
            start_beat: args.start,
            end_beat: args.end,
            audio_path: None,
            audio_start_beat: 0.0,
            audio_encoder_delay: 0.0,
            split_at_markers: false,
        },
        &cmd_rx,
        &state_tx,
    );
    drop(cmd_tx);
    drop(state_tx);
    match drain
        .join()
        .map_err(|_| "export status drain panicked".to_string())?
    {
        Some(event) if event.success => Ok(()),
        Some(event) => Err(event.message),
        None => Err("export produced no completion status".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_reproduction_config() {
        let args = [
            "export-repro",
            "scene.manifold",
            "--output",
            "out.mp4",
            "--start",
            "56",
            "--end",
            "128",
            "--width",
            "1080",
            "--height",
            "1920",
            "--fps",
            "60",
        ]
        .map(String::from);
        assert_eq!(
            parse_args(&args).unwrap(),
            ExportReproArgs {
                project: "scene.manifold".into(),
                output: "out.mp4".into(),
                start: 56.0,
                end: 128.0,
                width: 1080,
                height: 1920,
                fps: 60.0
            }
        );
    }

    #[test]
    fn rejects_reversed_range() {
        let args = [
            "export-repro",
            "scene.manifold",
            "--output",
            "out.mp4",
            "--start",
            "128",
            "--end",
            "56",
            "--width",
            "1",
            "--height",
            "1",
            "--fps",
            "60",
        ]
        .map(String::from);
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn requires_output() {
        let args = [
            "export-repro",
            "scene.manifold",
            "--start",
            "56",
            "--end",
            "128",
            "--width",
            "1",
            "--height",
            "1",
            "--fps",
            "60",
        ]
        .map(String::from);
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn rejects_non_finite_numeric_flags() {
        let base = vec![
            "export-repro", "scene.manifold", "--output", "out.mp4", "--start", "1",
            "--end", "2", "--width", "1", "--height", "1", "--fps", "60",
        ];
        for (flag, values) in [("--start", ["NaN", "inf", "-inf"]),
            ("--end", ["NaN", "inf", "-inf"]),
            ("--fps", ["NaN", "inf", "-inf"])] {
            for value in values {
                let mut args = base.iter().map(|s| s.to_string()).collect::<Vec<_>>();
                let index = args.iter().position(|s| s == flag).unwrap() + 1;
                args[index] = value.to_string();
                assert!(parse_args(&args).is_err(), "{flag}={value} should fail");
            }
        }
    }
}
