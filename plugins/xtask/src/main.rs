//! Custom xtask entry point.
//!
//! `nih_plug_xtask::main()` picks the leftmost ancestor directory that contains a
//! `Cargo.toml`, which breaks when the plugins workspace is nested inside another
//! workspace (MANIFOLD's root). We set the CWD ourselves and call the library's
//! public `build` / `bundle` functions directly.

use std::path::{Path, PathBuf};

fn main() -> anyhow::Result<()> {
    let xtask_manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = xtask_manifest_dir
        .parent()
        .expect("xtask must be nested inside the plugins workspace");
    std::env::set_current_dir(workspace_root)?;

    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path("./Cargo.toml")
        .exec()?;
    let target_dir = metadata.target_directory.as_std_path();

    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing command (expected 'bundle' or 'install')"))?;

    match command.as_str() {
        "bundle" => {
            let (packages, other_args) = split_packages_and_args(args);
            if packages.is_empty() {
                anyhow::bail!("bundle requires at least one package name");
            }
            nih_plug_xtask::build(&packages, &other_args)?;
            for package in &packages {
                nih_plug_xtask::bundle(target_dir, package, &other_args, false)?;
            }
            Ok(())
        }
        "install" => {
            let (packages, other_args) = split_packages_and_args(args);
            if packages.is_empty() {
                anyhow::bail!("install requires at least one package name");
            }
            nih_plug_xtask::build(&packages, &other_args)?;
            for package in &packages {
                nih_plug_xtask::bundle(target_dir, package, &other_args, false)?;
                install_vst3(target_dir, package)?;
            }
            Ok(())
        }
        cmd => anyhow::bail!("unknown command '{cmd}' (expected 'bundle' or 'install')"),
    }
}

fn split_packages_and_args(args: impl Iterator<Item = String>) -> (Vec<String>, Vec<String>) {
    let mut packages = Vec::new();
    let mut other = Vec::new();
    let mut seen_flag = false;
    for arg in args {
        if seen_flag || arg.starts_with('-') {
            seen_flag = true;
            other.push(arg);
        } else {
            packages.push(arg);
        }
    }
    (packages, other)
}

/// Copy the bundled `.vst3` for `package` into the user's VST3 plugin directory,
/// replacing any existing install.
fn install_vst3(target_dir: &Path, package: &str) -> anyhow::Result<()> {
    let dest_dir = user_vst3_dir()?;
    std::fs::create_dir_all(&dest_dir)?;

    let bundle_name = format!("{package}.vst3");
    let src = target_dir.join("bundled").join(&bundle_name);
    let dst = dest_dir.join(&bundle_name);

    if !src.exists() {
        anyhow::bail!(
            "bundle not found at {} — did `cargo xtask bundle` succeed?",
            src.display()
        );
    }

    if dst.exists() {
        std::fs::remove_dir_all(&dst).map_err(|e| {
            anyhow::anyhow!(
                "failed to remove existing install at {}: {e}. Close any DAW that has the plugin loaded.",
                dst.display()
            )
        })?;
    }

    let status = std::process::Command::new("cp")
        .arg("-R")
        .arg(&src)
        .arg(&dst)
        .status()?;
    if !status.success() {
        anyhow::bail!("`cp -R` failed while installing {bundle_name}");
    }

    println!("Installed {bundle_name} to {}", dst.display());
    println!("  Rescan in your DAW (Ableton: Preferences → Plug-Ins → Rescan) to pick up changes.");
    Ok(())
}

#[cfg(target_os = "macos")]
fn user_vst3_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("$HOME is not set"))?;
    Ok(PathBuf::from(home).join("Library/Audio/Plug-Ins/VST3"))
}

#[cfg(target_os = "windows")]
fn user_vst3_dir() -> anyhow::Result<PathBuf> {
    let appdata = std::env::var("COMMONPROGRAMFILES")
        .map_err(|_| anyhow::anyhow!("$COMMONPROGRAMFILES is not set"))?;
    Ok(PathBuf::from(appdata).join("VST3"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn user_vst3_dir() -> anyhow::Result<PathBuf> {
    anyhow::bail!("`install` is only supported on macOS and Windows")
}
