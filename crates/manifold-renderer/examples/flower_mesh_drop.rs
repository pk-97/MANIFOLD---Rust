//! One exact-mesh Box3D drop, saved as a GLB replay for MANIFOLD's existing importer.
//! Usage: flower_mesh_drop input.glb output.glb [bpm impact-beat-offset]
//! Beat offsets start at zero; `120 4` places impact at two seconds.
//! Original geometry/materials are preserved. No scan collider proxies or mesh reduction.

use std::{borrow::Cow, error::Error, fs, path::Path};

use manifold_foundation::{Beats, Bpm};
use manifold_physics::{BodyConfig, BodyKind, PhysicsWorld, Seconds};
use serde_json::{Value, json};

type Mat4 = [[f32; 4]; 4];
const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

const CONTACT_HZ: u32 = 240;

#[derive(Clone, Copy)]
struct MusicalTiming {
    bpm: Bpm,
    impact: Beats,
}

impl MusicalTiming {
    fn new(bpm: Bpm, impact: Beats) -> Result<Self, Box<dyn Error>> {
        if !bpm.is_valid() || !impact.is_finite() || impact < Beats::ZERO {
            return Err("Use 20–300 BPM and a finite, nonnegative impact beat offset".into());
        }
        Ok(Self { bpm, impact })
    }

    fn impact_time(self) -> Seconds {
        Seconds(self.impact.0 * 60.0 / f64::from(self.bpm.0))
    }

    fn release_time(self, flight: Seconds) -> Result<Seconds, Box<dyn Error>> {
        let release = self.impact_time() - flight;
        if !release.is_finite() || release < Seconds::ZERO {
            return Err(
                "Impact beat is too early for this drop; choose a later beat offset".into(),
            );
        }
        Ok(release)
    }
}

fn delay_replay(
    times: &mut Vec<[f32; 1]>,
    positions: &mut Vec<[f32; 3]>,
    rotations: &mut Vec<[f32; 4]>,
    release: Seconds,
) -> Result<(), Box<dyn Error>> {
    for time in times.iter_mut() {
        time[0] = (f64::from(time[0]) + release.0) as f32;
    }
    if times.iter().any(|t| !t[0].is_finite())
        || times.windows(2).any(|pair| pair[0][0] >= pair[1][0])
    {
        return Err("Beat offset is too large to preserve glTF animation time precision".into());
    }
    if release > Seconds::ZERO {
        // Hold the original release pose until its scheduled time. All later
        // keyframe intervals stay unchanged: no speed or gravity adjustment.
        times.insert(0, [0.0]);
        positions.insert(0, positions[0]);
        rotations.insert(0, rotations[0]);
    }
    Ok(())
}

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

fn run(input: &Path, output: &Path, timing: Option<MusicalTiming>) -> Result<(), Box<dyn Error>> {
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
    world.set_hit_events(body, true)?;
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
    const REPLAY_SECONDS: u32 = 6;
    let mut first_hit = None;
    for tick in 0..=REPLAY_SECONDS * CONTACT_HZ {
        let mut impact_sample = false;
        if tick > 0 {
            // Mesh CCD is unsupported. Refresh contacts at 240 Hz so this
            // one-metre drop advances less than the speculative contact margin.
            world.step(Seconds(1.0 / f64::from(CONTACT_HZ)), 4)?;
            if first_hit.is_none()
                && let Some(speed) = world.hit_speed(body)?
            {
                first_hit = Some((Seconds(f64::from(tick) / f64::from(CONTACT_HZ)), speed));
                impact_sample = true;
            }
        }
        // Keep the exact detected impact pose even when it falls between the
        // usual 60 Hz replay samples. Event timing is bounded by a 240 Hz tick.
        if tick % 4 != 0 && !impact_sample {
            continue;
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
        times.push([tick as f32 / CONTACT_HZ as f32]);
        positions.push(pose.position);
        rotations.push(pose.rotation);
    }
    eprintln!(
        "{} s simulation in {:.2} s: lowest vertex y={lowest:.5} m, final clearance={final_clearance:.5} m, final velocity={:?}",
        REPLAY_SECONDS,
        start.elapsed().as_secs_f64(),
        world.linear_velocity(body)?
    );
    if let Some(timing) = timing {
        let (flight, speed) = first_hit.ok_or("No impact event was detected for beat alignment")?;
        let release = timing.release_time(flight)?;
        delay_replay(&mut times, &mut positions, &mut rotations, release)?;
        // Private experiment metadata; the importer needs only the standard
        // animation keys. Beat offsets are measured from playback time zero.
        doc["extras"]["flowerDropTiming"] = json!({
            "bpm": timing.bpm.0,
            "impactBeatOffset": timing.impact.0,
            "impactSeconds": timing.impact_time().0,
            "releaseSeconds": release.0,
            "flightSeconds": flight.0,
            "approachSpeed": speed,
            "resolutionSeconds": 1.0 / f64::from(CONTACT_HZ),
        });
        eprintln!(
            "{}: release at {:.6} s; first solver hit after {:.6} s ({speed:.3} m/s); impact at beat offset {} / {:.6} s. Event resolution {:.3} ms.",
            timing.bpm,
            release.0,
            flight.0,
            timing.impact.0,
            timing.impact_time().0,
            1000.0 / f64::from(CONTACT_HZ)
        );
    }
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
    if args.len() != 2 && args.len() != 4 {
        return Err("Usage: flower_mesh_drop input.glb output.glb [bpm impact-beat-offset] (120 4 = impact at 2 seconds)".into());
    }
    let timing = if args.len() == 4 {
        Some(MusicalTiming::new(
            Bpm(args[2].to_str().ok_or("Invalid BPM text")?.parse()?),
            Beats(args[3].to_str().ok_or("Invalid beat text")?.parse()?),
        )?)
    } else {
        None
    };
    run(Path::new(&args[0]), Path::new(&args[1]), timing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn musical_timing_preserves_flight_and_holds_until_release() {
        for bpm in [90.0, 120.0, 173.0] {
            let timing = MusicalTiming::new(Bpm(bpm), Beats(4.0)).unwrap();
            let flight = Seconds(107.0 / 240.0);
            let release = timing.release_time(flight).unwrap();
            let mut times = vec![[0.0], [flight.as_f32()], [6.0]];
            let mut positions = vec![[1.0; 3], [2.0; 3], [3.0; 3]];
            let mut rotations = vec![[0.0, 0.0, 0.0, 1.0]; 3];
            delay_replay(&mut times, &mut positions, &mut rotations, release).unwrap();
            assert_eq!(positions[0], positions[1]);
            assert_eq!(times[0], [0.0]);
            assert!((f64::from(times[2][0]) - timing.impact_time().0).abs() < 1e-6);
            assert!((f64::from(times[2][0] - times[1][0]) - flight.0).abs() < 1e-6);
            assert_eq!(positions[2], [2.0; 3]);
        }
    }

    #[test]
    fn musical_timing_rejects_impossible_or_unrepresentable_schedules() {
        assert!(MusicalTiming::new(Bpm(0.0), Beats(4.0)).is_err());
        assert!(MusicalTiming::new(Bpm(120.0), Beats(f64::NAN)).is_err());
        let early = MusicalTiming::new(Bpm(120.0), Beats(0.5)).unwrap();
        assert!(early.release_time(Seconds(0.45)).is_err());
        assert!(
            delay_replay(
                &mut vec![[0.0], [1.0 / 240.0]],
                &mut vec![[0.0; 3]; 2],
                &mut vec![[0.0, 0.0, 0.0, 1.0]; 2],
                Seconds(1e10),
            )
            .is_err()
        );
    }
}
