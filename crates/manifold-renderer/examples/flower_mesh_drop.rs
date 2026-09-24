//! One exact-mesh Box3D drop, saved as a GLB replay for MANIFOLD's existing importer.
//! Usage: cargo run -p manifold-renderer --example flower_mesh_drop -- input.glb output.glb
//! Original geometry/materials are preserved. No scan collider proxies or mesh reduction.

use std::{borrow::Cow, error::Error, fs, path::Path};

use manifold_physics::{BodyConfig, BodyKind, PhysicsWorld, Seconds};
use serde_json::{Value, json};

type Mat4 = [[f32; 4]; 4];
const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn multiply(a: Mat4, b: Mat4) -> Mat4 {
    std::array::from_fn(|c| std::array::from_fn(|r| (0..4).map(|k| a[k][r] * b[c][k]).sum()))
}

fn point(m: Mat4, p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[3][r] + (0..3).map(|c| m[c][r] * p[c]).sum::<f32>())
}

fn rotate(q: [f32; 4], p: [f32; 3]) -> [f32; 3] {
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let v = [q[0], q[1], q[2]];
    let t = cross(v, p).map(|x| 2.0 * x);
    let u = cross(v, t);
    std::array::from_fn(|i| p[i] + q[3] * t[i] + u[i])
}

fn collect(
    node: gltf::Node<'_>,
    parent: Mat4,
    bin: &[u8],
    vertices: &mut Vec<[f32; 3]>,
    triangles: &mut Vec<[u32; 3]>,
) -> Result<(), Box<dyn Error>> {
    let world = multiply(parent, node.transform().matrix());
    if node.skin().is_some() {
        return Err("This experiment requires a rigid, unskinned scan".into());
    }
    if let Some(mesh) = node.mesh() {
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles
                || primitive.morph_targets().len() != 0
            {
                return Err(
                    "This experiment requires triangle geometry without morph targets".into(),
                );
            }
            let reader = primitive.reader(|buffer| (buffer.index() == 0).then_some(bin));
            let base = u32::try_from(vertices.len())?;
            let positions = reader.read_positions().ok_or("Missing POSITION data")?;
            vertices.extend(positions.map(|p| point(world, p)));
            let indices: Vec<u32> = match reader.read_indices() {
                Some(indices) => indices.into_u32().collect(),
                None => (0..u32::try_from(vertices.len())? - base).collect(),
            };
            if !indices.len().is_multiple_of(3) {
                return Err("Incomplete triangle".into());
            }
            for t in indices.chunks_exact(3) {
                if t.iter()
                    .any(|&i| i as usize >= vertices.len() - base as usize)
                {
                    return Err("Triangle index outside its primitive".into());
                }
                triangles.push([base + t[0], base + t[1], base + t[2]]);
            }
        }
    }
    for child in node.children() {
        collect(child, world, bin, vertices, triangles)?;
    }
    Ok(())
}

fn append(doc: &mut Value, field: &str, value: Value) -> usize {
    if doc[field].is_null() {
        doc[field] = json!([]);
    }
    let array = doc[field].as_array_mut().expect("glTF array");
    let index = array.len();
    array.push(value);
    index
}

fn accessor<const N: usize>(
    doc: &mut Value,
    bin: &mut Vec<u8>,
    data: &[[f32; N]],
    kind: &str,
) -> usize {
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let offset = bin.len();
    for row in data {
        for value in row {
            bin.extend(value.to_le_bytes());
        }
    }
    let view = append(
        doc,
        "bufferViews",
        json!({"buffer":0,"byteOffset":offset,"byteLength":bin.len()-offset}),
    );
    let min: Vec<f32> = (0..N)
        .map(|i| data.iter().map(|p| p[i]).fold(f32::INFINITY, f32::min))
        .collect();
    let max: Vec<f32> = (0..N)
        .map(|i| data.iter().map(|p| p[i]).fold(f32::NEG_INFINITY, f32::max))
        .collect();
    append(
        doc,
        "accessors",
        json!({"bufferView":view,"componentType":5126,"count":data.len(),"type":kind,"min":min,"max":max}),
    )
}

fn floor(doc: &mut Value, bin: &mut Vec<u8>) -> usize {
    // Exactly the same slab used by the physics world: top y=0, bottom y=-0.2.
    let corners = slab();
    let faces = [
        ([0, 2, 3, 1], [0.0, 0.0, -1.0]),
        ([4, 5, 7, 6], [0.0, 0.0, 1.0]),
        ([0, 4, 6, 2], [-1.0, 0.0, 0.0]),
        ([1, 3, 7, 5], [1.0, 0.0, 0.0]),
        ([0, 1, 5, 4], [0.0, -1.0, 0.0]),
        ([2, 6, 7, 3], [0.0, 1.0, 0.0]),
    ];
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    for (face, normal) in faces {
        for index in [0, 1, 2, 0, 2, 3] {
            positions.push(corners[face[index]]);
            normals.push(normal);
        }
    }
    let pos = accessor(doc, bin, &positions, "VEC3");
    let normal = accessor(doc, bin, &normals, "VEC3");
    let material = append(
        doc,
        "materials",
        json!({"name":"Contact floor","pbrMetallicRoughness":{"baseColorFactor":[0.18,0.2,0.23,1.0],"metallicFactor":0,"roughnessFactor":0.8}}),
    );
    let mesh = append(
        doc,
        "meshes",
        json!({"primitives":[{"attributes":{"POSITION":pos,"NORMAL":normal},"material":material}]}),
    );
    append(doc, "nodes", json!({"name":"Contact floor","mesh":mesh}))
}

fn slab() -> [[f32; 3]; 8] {
    std::array::from_fn(|i| {
        [
            if i & 1 == 0 { -10.0 } else { 10.0 },
            if i & 2 == 0 { -0.2 } else { 0.0 },
            if i & 4 == 0 { -10.0 } else { 10.0 },
        ]
    })
}

fn run(input: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    if input == output || (output.exists() && fs::canonicalize(input)? == fs::canonicalize(output)?)
    {
        return Err("Choose a separate output file; the source scan must remain intact".into());
    }
    let bytes = fs::read(input)?;
    let glb = gltf::binary::Glb::from_slice(&bytes)?;
    let mut doc: Value = serde_json::from_slice(&glb.json)?;
    let mut bin = glb
        .bin
        .ok_or("Input must be a self-contained GLB")?
        .into_owned();
    let parsed = gltf::Gltf::from_slice(&bytes)?;
    if parsed.buffers().len() != 1
        || doc["buffers"][0].get("uri").is_some()
        || parsed
            .images()
            .any(|image| matches!(image.source(), gltf::image::Source::Uri { .. }))
    {
        return Err("Input must contain its geometry and textures in its GLB buffer".into());
    }
    if parsed.animations().len() != 0 {
        return Err("Use an unanimated scan for this experiment".into());
    }
    let scene = parsed
        .default_scene()
        .ok_or("Input needs a default scene")?;
    let roots: Vec<usize> = scene.nodes().map(|n| n.index()).collect();
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    for root in scene.nodes() {
        collect(root, IDENTITY, &bin, &mut vertices, &mut triangles)?;
    }
    if vertices.is_empty() {
        return Err("Scan contains no vertices".into());
    }
    let min: [f32; 3] =
        std::array::from_fn(|i| vertices.iter().map(|p| p[i]).fold(f32::INFINITY, f32::min));
    let max: [f32; 3] = std::array::from_fn(|i| {
        vertices
            .iter()
            .map(|p| p[i])
            .fold(f32::NEG_INFINITY, f32::max)
    });
    let span = (0..3).map(|i| max[i] - min[i]).fold(0.0, f32::max);
    if !span.is_finite() || span <= 0.0 {
        return Err("Invalid scan bounds".into());
    }
    let scale = 2.2 / span;
    let offset: [f32; 3] = std::array::from_fn(|i| -0.5 * (min[i] + max[i]) * scale);
    for p in &mut vertices {
        for i in 0..3 {
            p[i] = p[i] * scale + offset[i];
        }
    }
    let angle = 20_f32.to_radians() * 0.5;
    let rotation = [0.0, 0.0, angle.sin(), angle.cos()];
    let bottom = vertices
        .iter()
        .map(|&p| rotate(rotation, p)[1])
        .fold(f32::INFINITY, f32::min);
    let config = BodyConfig {
        position: [0.0, 1.0 - bottom, 0.0],
        rotation,
        ..BodyConfig::default()
    };
    let mut world = PhysicsWorld::new([0.0, -9.81, 0.0])?;
    world.add_hull(
        &slab(),
        BodyConfig {
            kind: BodyKind::Fixed,
            ..BodyConfig::default()
        },
    )?;
    let body = world.add_triangle_mesh(&vertices, &triangles, config)?;
    eprintln!(
        "Exact scan collider: {} vertices, {} triangles; longest dimension 2.2 m; 1 m drop. Uniform surface mass, no collider approximation.",
        vertices.len(),
        triangles.len()
    );
    let mut times = Vec::new();
    let mut positions = Vec::new();
    let mut rotations = Vec::new();
    let mut lowest = f32::INFINITY;
    let mut final_clearance = 0.0;
    let start = std::time::Instant::now();
    const REPLAY_FRAMES: u32 = 360;
    for frame in 0..=REPLAY_FRAMES {
        if frame > 0 {
            // Mesh CCD is unsupported. Refresh contacts at 240 Hz so this
            // one-metre drop advances less than the speculative contact margin.
            for _ in 0..4 {
                world.step(Seconds(1.0 / 240.0), 4)?;
            }
        }
        let pose = world.pose(body)?;
        if !pose
            .position
            .iter()
            .chain(&pose.rotation)
            .all(|x| x.is_finite())
        {
            return Err("Simulation produced a non-finite pose".into());
        }
        final_clearance = vertices
            .iter()
            .map(|&p| rotate(pose.rotation, p)[1] + pose.position[1])
            .fold(f32::INFINITY, f32::min);
        lowest = lowest.min(final_clearance);
        times.push([frame as f32 / 60.0]);
        positions.push(pose.position);
        rotations.push(pose.rotation);
    }
    eprintln!(
        "{} s simulation in {:.2} s: lowest vertex y={lowest:.5} m, final clearance={final_clearance:.5} m, final velocity={:?}",
        REPLAY_FRAMES / 60,
        start.elapsed().as_secs_f64(),
        world.linear_velocity(body)?
    );
    let normalized = append(
        &mut doc,
        "nodes",
        json!({"name":"Scan scale and origin","children":roots,"translation":offset,"scale":[scale,scale,scale]}),
    );
    let root = append(
        &mut doc,
        "nodes",
        json!({"name":"Box3D exact mesh drop","children":[normalized],"translation":positions[0],"rotation":rotations[0]}),
    );
    let floor_node = floor(&mut doc, &mut bin);
    let scene = append(
        &mut doc,
        "scenes",
        json!({"name":"Flower mesh contact experiment","nodes":[root,floor_node]}),
    );
    doc["scene"] = json!(scene);
    let time = accessor(&mut doc, &mut bin, &times, "SCALAR");
    let position = accessor(&mut doc, &mut bin, &positions, "VEC3");
    let rotation = accessor(&mut doc, &mut bin, &rotations, "VEC4");
    doc["animations"] = json!([{"name":"Box3D drop replay","samplers":[
        {"input":time,"output":position,"interpolation":"LINEAR"},
        {"input":time,"output":rotation,"interpolation":"LINEAR"}],"channels":[
        {"sampler":0,"target":{"node":root,"path":"translation"}},
        {"sampler":1,"target":{"node":root,"path":"rotation"}}]}]);
    doc["buffers"][0]["byteLength"] = json!(bin.len());
    let json = serde_json::to_vec(&doc)?;
    let result = gltf::binary::Glb {
        header: gltf::binary::Header {
            magic: *b"glTF",
            version: 2,
            length: 0,
        },
        json: Cow::Owned(json),
        bin: Some(Cow::Owned(bin)),
    };
    let output_bytes = result.to_vec()?;
    gltf::Gltf::from_slice(&output_bytes)?;
    fs::write(output, output_bytes)?;
    eprintln!(
        "Replay: {} — import this GLB into MANIFOLD to inspect contact.",
        output.display()
    );
    if lowest < -0.03 || final_clearance.abs() > 0.03 {
        return Err(
            "Floor-height check failed (>3 cm below/above y=0); this includes leaving the finite platform. Inspect the saved replay before diagnosing penetration."
                .into(),
        );
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("Usage: flower_mesh_drop input.glb output.glb".into());
    }
    run(Path::new(&args[0]), Path::new(&args[1]))
}
