use std::env;
use std::path::PathBuf;

#[path = "../../scripts/native_source_identity.rs"]
mod native_source_identity;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let native_dir = manifest_dir.join("native");
    let engine_dir = native_dir.join("flip_engine");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("out dir"));
    native_source_identity::emit_source_identity(
        &manifest_dir,
        &["native", "src"],
        "MANIFOLD_FLUIDS_SOURCE_IDENTITY",
    )
    .expect("compute manifold-fluids source identity");

    let generated_version = out_dir.join("versionutils.cpp");
    let version_template = std::fs::read_to_string(engine_dir.join("versionutils.cpp.in"))
        .expect("read pinned FLIP version template");
    let version = version_template
        .replace("@FFENGINE_VERSION_MAJOR@", "1")
        .replace("@FFENGINE_VERSION_MINOR@", "8")
        .replace("@FFENGINE_VERSION_REVISION@", "8")
        .replace(
            "@FFENGINE_VERSION_LABEL@",
            "1.8.8 GitHub Release 2026-07-14",
        )
        .replace("@FFENGINE_VERSION_SUPPORT_LICENSE_TYPE@", "GitHub")
        .replace("@FFENGINE_VERSION_SUPPORT_LICENSE_ID@", "MANIFOLD");
    std::fs::write(&generated_version, version).expect("write generated FLIP version source");

    println!("cargo:rerun-if-changed={}", engine_dir.display());
    println!(
        "cargo:rerun-if-changed={}",
        native_dir
            .join("coupling_viscosity_operator_probe.cpp")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir
            .join("coupling_viscosity_operator_probe.h")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("coupling_viscosity_probe.cpp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("coupling_viscosity_probe.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("coupling_boundary_probe.cpp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("coupling_boundary_probe.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("coupling_operator_probe.cpp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("coupling_operator_probe.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("coupling_probe.cpp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("coupling_probe.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("bridge.cpp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("bridge.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("LICENSE_MIT.md").display()
    );

    let sources = [
        "aabb.cpp",
        "collision.cpp",
        "diffuseparticlesimulation.cpp",
        "fluidmaterialgrid.cpp",
        "fluidsimulation.cpp",
        "forcefield.cpp",
        "forcefieldcurve.cpp",
        "forcefieldgrid.cpp",
        "forcefieldpoint.cpp",
        "forcefieldsurface.cpp",
        "forcefieldutils.cpp",
        "forcefieldvolume.cpp",
        "gridindexkeymap.cpp",
        "gridindexvector.cpp",
        "gridutils.cpp",
        "influencegrid.cpp",
        "interpolation.cpp",
        "levelsetsolver.cpp",
        "levelsetutils.cpp",
        "logfile.cpp",
        "macvelocityfield.cpp",
        "meshfluidsource.cpp",
        "meshlevelset.cpp",
        "meshobject.cpp",
        "meshutils.cpp",
        "noisegenerationutils.cpp",
        "particlelevelset.cpp",
        "particlemaskgrid.cpp",
        "particlemesher.cpp",
        "particlesheeter.cpp",
        "particlesystem.cpp",
        "polygonizer3d.cpp",
        "rigidboundaryvelocity.cpp",
        "rigidfluidcoupling.cpp",
        "pressuresolver.cpp",
        "scalarfield.cpp",
        "spatialpointgrid.cpp",
        "surfaceframe.cpp",
        "stopwatch.cpp",
        "threadutils.cpp",
        "trianglemesh.cpp",
        "turbulencefield.cpp",
        "velocityadvector.cpp",
        "viscositysolver.cpp",
        "vmath.cpp",
    ];

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .opt_level(3)
        .warnings(false)
        .define("WITH_MIXBOX", "0")
        .include(&engine_dir)
        .include(&native_dir)
        .file(native_dir.join("bridge.cpp"))
        .file(native_dir.join("coupling_probe.cpp"))
        .file(native_dir.join("coupling_operator_probe.cpp"))
        .file(native_dir.join("coupling_boundary_probe.cpp"))
        .file(native_dir.join("coupling_viscosity_probe.cpp"))
        .file(native_dir.join("coupling_viscosity_operator_probe.cpp"))
        .file(engine_dir.join("mixbox/mixbox_stub.cpp"))
        .file(generated_version);
    if env::var_os("CARGO_FEATURE_FACE_ORACLE").is_some() {
        build.define("MANIFOLD_FACE_ORACLE", "1");
    }
    if env::var_os("CARGO_FEATURE_WHITEWATER_ORACLE").is_some() {
        build.define("MANIFOLD_WHITEWATER_ORACLE", "1");
    }
    for source in sources {
        build.file(engine_dir.join(source));
    }
    build.compile("manifold_flip_fluids");
}
