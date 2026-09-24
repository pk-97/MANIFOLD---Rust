use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let native_dir = manifest_dir.join("native/box3d");
    let include_dir = native_dir.join("include");
    let source_dir = native_dir.join("src");
    // Include private headers when deliberately updating the pinned native tree.
    println!("cargo:rerun-if-changed={}", native_dir.display());

    let sources = [
        "aabb.c",
        "arena_allocator.c",
        "bitset.c",
        "block_allocator.c",
        "body.c",
        "broad_phase.c",
        "capsule.c",
        "compound.c",
        "constraint_graph.c",
        "contact.c",
        "contact_solver.c",
        "convex_manifold.c",
        "core.c",
        "distance.c",
        "distance_joint.c",
        "dynamic_tree.c",
        "height_field.c",
        "hull.c",
        "id_pool.c",
        "island.c",
        "joint.c",
        "manifold.c",
        "math_functions.c",
        "mesh.c",
        "mesh_contact.c",
        "motor_joint.c",
        "mover.c",
        "parallel_for.c",
        "parallel_joint.c",
        "physics_world.c",
        "prismatic_joint.c",
        "recording.c",
        "recording_replay.c",
        "revolute_joint.c",
        "scheduler.c",
        "sensor.c",
        "shape.c",
        "simd.c",
        "solver.c",
        "solver_set.c",
        "sphere.c",
        "spherical_joint.c",
        "table.c",
        "timer.c",
        "triangle_manifold.c",
        "types.c",
        "weld_joint.c",
        "wheel_joint.c",
        "world_snapshot.c",
    ];

    let mut build = cc::Build::new();
    build.include(&include_dir).include(&source_dir).std("c17");
    for source in sources {
        build.file(source_dir.join(source));
        println!(
            "cargo:rerun-if-changed={}",
            source_dir.join(source).display()
        );
    }
    build.file(native_dir.join("bridge.c"));
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("bridge.c").display()
    );
    for header in [
        "base.h",
        "box3d.h",
        "collision.h",
        "config.h",
        "constants.h",
        "id.h",
        "math_functions.h",
        "types.h",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            include_dir.join("box3d").join(header).display()
        );
    }
    build.compile("manifold_box3d");
}
