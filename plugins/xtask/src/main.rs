//! Custom xtask entry point.
//!
//! `nih_plug_xtask::main()` picks the leftmost ancestor directory that contains a
//! `Cargo.toml`, which breaks when the plugins workspace is nested inside another
//! workspace (MANIFOLD's root). We set the CWD ourselves and call the library's
//! public `build` / `bundle` functions directly.

use std::path::PathBuf;

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
        .ok_or_else(|| anyhow::anyhow!("missing command (expected 'bundle')"))?;

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
        cmd => anyhow::bail!("unknown command '{cmd}' (expected 'bundle')"),
    }
}

/// Split `bundle` args into package names (bare positional args before any flag)
/// and pass-through cargo args (everything starting with `-`).
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
