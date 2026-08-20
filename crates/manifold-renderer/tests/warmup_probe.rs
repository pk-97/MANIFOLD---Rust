use std::path::PathBuf;

#[test]
fn warmup_probe_rt_fixture() {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.push("tests/fixtures/rt/RtEmissiveStrength.manifold");
    println!("loading {}", path.display());
    let project = manifold_io::loader::load_project(&path).expect("load fixture");
    for (i, layer) in project.timeline.layers.iter().enumerate() {
        println!(
            "layer {}: name={} type={:?} gen_type={}",
            i,
            layer.name,
            layer.layer_type,
            layer.generator_type().as_str()
        );
    }
}
