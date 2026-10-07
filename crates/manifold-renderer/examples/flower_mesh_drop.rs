//! Standard Box3D convex-part flower drop, saved as a GLB replay.
//! Usage: flower_mesh_drop input.glb output.glb [bpm impact-beat-offset [pieces]]
//! Beat offsets start at zero; `120 4` places impact at two seconds.
//! Original visible geometry/materials are preserved; physics uses fitted convex parts.

use std::{borrow::Cow, error::Error, fs, path::Path};

use bytemuck::Zeroable;
use manifold_foundation::{Beats, Bpm};
use manifold_physics::{BodyConfig, BodyKind, PhysicsWorld, Seconds};
use manifold_renderer::mesh::MeshVertex;
use manifold_renderer::node_graph::physics_mesh::prepare_colliders;
use serde_json::{Value, json};

use manifold_renderer::node_graph::mesh_partition as fracture;

struct Contributor {
    primitive: Value,
    transform: Mat4,
    triangle_start: usize,
    indices: Vec<u32>,
}

#[derive(Default)]
struct Track {
    positions: Vec<[f32; 3]>,
    rotations: Vec<[f32; 4]>,
}

type Mat4 = [[f32; 4]; 4];
const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

const CONTACT_HZ: u32 = 60;

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
    contributors: &mut Vec<Contributor>,
    doc: &Value,
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
            let triangle_start = triangles.len();
            for t in indices.chunks_exact(3) {
                if t.iter()
                    .any(|&i| i as usize >= vertices.len() - base as usize)
                {
                    return Err("Triangle index outside its primitive".into());
                }
                triangles.push([base + t[0], base + t[1], base + t[2]]);
            }
            contributors.push(Contributor {
                primitive: doc["meshes"][mesh.index()]["primitives"][primitive.index()].clone(),
                transform: world,
                triangle_start,
                indices,
            });
        }
    }
    for child in node.children() {
        collect(child, world, bin, vertices, triangles, contributors, doc)?;
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

fn floor_world() -> Result<PhysicsWorld, Box<dyn Error>> {
    let mut world = PhysicsWorld::new([0.0, -9.81, 0.0])?;
    world.add_hull(
        &slab(),
        BodyConfig {
            kind: BodyKind::Fixed,
            ..BodyConfig::default()
        },
    )?;
    Ok(world)
}

/// Reuse every source attribute and material; only triangle index lists change.
/// Surface patches retain scan boundaries. No new caps or thickness are invented.
fn fragment_nodes(
    doc: &mut Value,
    bin: &mut Vec<u8>,
    fragments: &[fracture::Fragment],
    contributors: &[Contributor],
    scale: f32,
    offset: [f32; 3],
) -> Vec<usize> {
    fragments.iter().enumerate().map(|(number, fragment)| {
        let mut children = Vec::new();
        for contributor in contributors {
            let end = contributor.triangle_start + contributor.indices.len() / 3;
            let indices: Vec<u32> = fragment.triangle_ids.iter()
                .filter(|&&i| i >= contributor.triangle_start && i < end)
                .flat_map(|&i| {
                    let start = (i - contributor.triangle_start) * 3;
                    contributor.indices[start..start + 3].iter().copied()
                }).collect();
            if indices.is_empty() { continue; }
            while !bin.len().is_multiple_of(4) { bin.push(0); }
            let byte_offset = bin.len();
            for index in &indices { bin.extend(index.to_le_bytes()); }
            let view = append(doc, "bufferViews", json!({"buffer":0,"byteOffset":byte_offset,"byteLength":indices.len()*4}));
            let accessor = append(doc, "accessors", json!({"bufferView":view,"componentType":5125,"count":indices.len(),"type":"SCALAR"}));
            let mut primitive = contributor.primitive.clone();
            primitive["indices"] = json!(accessor);
            let mesh = append(doc, "meshes", json!({"primitives":[primitive]}));
            let matrix: Vec<f32> = contributor.transform.into_iter().flatten().collect();
            children.push(append(doc, "nodes", json!({"mesh":mesh,"matrix":matrix})));
        }
        let normalized = append(doc, "nodes", json!({"children":children,"translation":offset,"scale":[scale,scale,scale]}));
        append(doc, "nodes", json!({"name":format!("Flower piece {}", number+1),"children":[normalized]}))
    }).collect()
}

fn run(
    input: &Path,
    output: &Path,
    timing: Option<MusicalTiming>,
    pieces: Option<usize>,
) -> Result<(), Box<dyn Error>> {
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
    let mut contributors = Vec::new();
    for root in scene.nodes() {
        collect(
            root,
            IDENTITY,
            &bin,
            &mut vertices,
            &mut triangles,
            &mut contributors,
            &doc,
        )?;
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
    let fragments = pieces
        .map(|count| fracture::partition(&vertices, &triangles, count))
        .transpose()?;
    let hulls_for = |vertices: &[[f32; 3]], triangles: &[[u32; 3]], parts| {
        let mesh: Vec<_> = triangles
            .iter()
            .flat_map(|tri| {
                tri.iter().map(|&index| MeshVertex {
                    position: vertices[index as usize],
                    ..MeshVertex::zeroed()
                })
            })
            .collect();
        prepare_colliders(&mesh, parts)
    };
    let collider = hulls_for(&vertices, &triangles, 32)?;
    let fragment_colliders = fragments
        .as_ref()
        .map(|fragments| {
            fragments
                .iter()
                .map(|f| hulls_for(&f.vertices, &f.triangles, 1))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let mut world = floor_world()?;
    let body = world.add_hulls(&collider.hulls, config)?;
    world.set_hit_events(body, true)?;
    eprintln!(
        "Original scan: {} vertices, {} triangles; 32 standard convex colliders; longest dimension 2.2 m; 1 m drop.",
        vertices.len(),
        triangles.len()
    );
    let contact_hz = CONTACT_HZ;
    let mut times = Vec::new();
    let mut tracks: Vec<Track> = (0..pieces.unwrap_or(1)).map(|_| Track::default()).collect();
    let mut fragment_bodies = Vec::new();
    let mut lowest = f32::INFINITY;
    let mut final_clearance = 0.0_f32;
    let start = std::time::Instant::now();
    const REPLAY_SECONDS: u32 = 6;
    let mut first_hit = None;
    for tick in 0..=REPLAY_SECONDS * contact_hz {
        let mut impact_sample = false;
        if tick > 0 {
            // Refresh collision contacts independently of the 60 Hz replay.
            world.step(Seconds(1.0 / f64::from(contact_hz)), 4)?;
            if first_hit.is_none()
                && let Some(speed) = world.hit_speed(body)?
            {
                first_hit = Some((Seconds(f64::from(tick) / f64::from(contact_hz)), speed));
                impact_sample = true;
                if let Some(fragments) = &fragments {
                    let pose = world.pose(body)?;
                    let angular = world.angular_velocity(body)?;
                    let area: f32 = fragments.iter().map(|fragment| fragment.area).sum();
                    let mut broken_world = floor_world()?;
                    for (fragment, collider) in
                        fragments.iter().zip(fragment_colliders.as_ref().unwrap())
                    {
                        let handle = broken_world.add_hulls(
                            &collider.hulls,
                            BodyConfig {
                                position: pose.position,
                                rotation: pose.rotation,
                                mass: config.mass * fragment.area / area,
                                ..config
                            },
                        )?;
                        // Inherit the rigid body's velocity at this piece's centre
                        // of mass, including angular motion. No explosion kick.
                        broken_world.set_velocity(
                            handle,
                            world.velocity_at_local_point(body, fragment.center)?,
                            angular,
                        )?;
                        fragment_bodies.push(handle);
                    }
                    // The intact body no longer exists in the active simulation.
                    world = broken_world;
                    eprintln!(
                        "Released {} original surface pieces with standard convex colliders at first impact; inherited motion; mutual collisions enabled.",
                        fragments.len()
                    );
                }
            }
        }
        // Keep the exact detected impact pose even when it falls between the
        // usual 60 Hz replay samples. Event timing is bounded by a contact tick.
        if tick % (contact_hz / 60) != 0 && !impact_sample {
            continue;
        }
        final_clearance = 0.0;
        for (index, track) in tracks.iter_mut().enumerate() {
            let pose = world.pose(fragment_bodies.get(index).copied().unwrap_or(body))?;
            if !pose
                .position
                .iter()
                .chain(&pose.rotation)
                .all(|x| x.is_finite())
            {
                return Err("Simulation produced a non-finite pose".into());
            }
            let points = fragments.as_ref().map_or(vertices.as_slice(), |fragments| {
                fragments[index].vertices.as_slice()
            });
            let clearance = points
                .iter()
                .map(|&p| rotate(pose.rotation, p)[1] + pose.position[1])
                .fold(f32::INFINITY, f32::min);
            lowest = lowest.min(clearance);
            final_clearance = final_clearance.max(clearance.abs());
            track.positions.push(pose.position);
            track.rotations.push(pose.rotation);
        }
        times.push([tick as f32 / contact_hz as f32]);
        if pieces.is_some() && tick > 0 && tick % contact_hz == 0 {
            eprintln!(
                "Fragment check: {} simulated s in {:.1} s; sampled minimum y={lowest:.6} m",
                tick / contact_hz,
                start.elapsed().as_secs_f64()
            );
        }
    }
    eprintln!(
        "{} s simulation in {:.2} s: lowest vertex y={lowest:.5} m, final clearance={final_clearance:.5} m, final velocity={:?}",
        REPLAY_SECONDS,
        start.elapsed().as_secs_f64(),
        world.linear_velocity(fragment_bodies.first().copied().unwrap_or(body))?
    );
    if let Some(timing) = timing {
        let (flight, speed) = first_hit.ok_or("No impact event was detected for beat alignment")?;
        let release = timing.release_time(flight)?;
        let original_times = times.clone();
        for track in &mut tracks {
            times.clone_from(&original_times);
            delay_replay(
                &mut times,
                &mut track.positions,
                &mut track.rotations,
                release,
            )?;
        }
        // Private experiment metadata; the importer needs only the standard
        // animation keys. Beat offsets are measured from playback time zero.
        doc["extras"]["flowerDropTiming"] = json!({
            "bpm": timing.bpm.0,
            "impactBeatOffset": timing.impact.0,
            "impactSeconds": timing.impact_time().0,
            "releaseSeconds": release.0,
            "flightSeconds": flight.0,
            "approachSpeed": speed,
            "resolutionSeconds": 1.0 / f64::from(contact_hz),
        });
        eprintln!(
            "{}: release at {:.6} s; first solver hit after {:.6} s ({speed:.3} m/s); impact at beat offset {} / {:.6} s. Event resolution {:.3} ms.",
            timing.bpm,
            release.0,
            flight.0,
            timing.impact.0,
            timing.impact_time().0,
            1000.0 / f64::from(contact_hz)
        );
    }
    let mut replay_roots = if let Some(fragments) = &fragments {
        if fragment_bodies.is_empty() {
            return Err("No impact detected to release the pieces".into());
        }
        doc["extras"]["flowerFracture"] = json!({"pieces":fragments.len(),"sourceTriangles":triangles.len(),"pieceCollisions":true,"method":"original triangle surface patches", "colliders":"standard Box3D convex hulls","addedImpulse":0});
        fragment_nodes(&mut doc, &mut bin, fragments, &contributors, scale, offset)
    } else {
        let normalized = append(
            &mut doc,
            "nodes",
            json!({"name":"Scan scale and origin","children":roots,"translation":offset,"scale":[scale,scale,scale]}),
        );
        vec![append(
            &mut doc,
            "nodes",
            json!({"name":"Box3D exact mesh drop","children":[normalized]}),
        )]
    };
    let time = accessor(&mut doc, &mut bin, &times, "SCALAR");
    let mut samplers = Vec::new();
    let mut channels = Vec::new();
    for (&root, track) in replay_roots.iter().zip(&tracks) {
        doc["nodes"][root]["translation"] = json!(track.positions[0]);
        doc["nodes"][root]["rotation"] = json!(track.rotations[0]);
        let position = accessor(&mut doc, &mut bin, &track.positions, "VEC3");
        let rotation = accessor(&mut doc, &mut bin, &track.rotations, "VEC4");
        for (path, output) in [("translation", position), ("rotation", rotation)] {
            channels.push(json!({"sampler":samplers.len(),"target":{"node":root,"path":path}}));
            samplers.push(json!({"input":time,"output":output,"interpolation":"LINEAR"}));
        }
    }
    let floor_node = floor(&mut doc, &mut bin);
    replay_roots.push(floor_node);
    let scene = append(
        &mut doc,
        "scenes",
        json!({"name":"Flower mesh contact experiment","nodes":replay_roots}),
    );
    doc["scene"] = json!(scene);
    doc["animations"] =
        json!([{"name":"Box3D drop replay","samplers":samplers,"channels":channels}]);
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
    if lowest < -0.001 || (pieces.is_none() && final_clearance.abs() > 0.03) {
        return Err(
            "Floor contact check failed (more than 1 mm penetration, or intact body not on floor). Inspect the saved replay. Fragment pieces may legitimately rest on one another."
                .into(),
        );
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 && args.len() != 4 && args.len() != 5 {
        return Err("Usage: flower_mesh_drop input.glb output.glb [bpm impact-beat-offset [pieces]] (120 4 32 = 32 pieces at 2 seconds)".into());
    }
    let timing = if args.len() >= 4 {
        Some(MusicalTiming::new(
            Bpm(args[2].to_str().ok_or("Invalid BPM text")?.parse()?),
            Beats(args[3].to_str().ok_or("Invalid beat text")?.parse()?),
        )?)
    } else {
        None
    };
    let pieces = if args.len() == 5 {
        let count: usize = args[4].to_str().ok_or("Invalid piece count")?.parse()?;
        if !(2..=50).contains(&count) {
            return Err("This small fracture experiment supports 2–50 pieces".into());
        }
        Some(count)
    } else {
        None
    };
    run(Path::new(&args[0]), Path::new(&args[1]), timing, pieces)
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
