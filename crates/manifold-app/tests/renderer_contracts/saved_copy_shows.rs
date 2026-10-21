//! Peter's two saved shows with a copy group play bit-identically after the
//! load migration: every pose at tick 120 hashes to the digest main produced
//! from the same files through the same loader, the graph the app plays (the
//! layer's own, else the catalog's) and the saved card values. Drivers and
//! scene modifiers do not run here; they act on the same bodies either side,
//! so the migration's job ends at handing the world the same scene.
//!
//! Mirage Physics carries its own graph. box3DTesting tracked the stock
//! Physics Boxes, which now reads its Spacing and rotation cards differently,
//! so the load gives the layer the converted copy the file saved.

use std::path::{Path, PathBuf};

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_core::effects::{ParamConvert, PresetInstance};
use manifold_core::project::{EmbeddedPreset, Project};
use manifold_core::preset_def::PresetKind;
use manifold_core::Seconds;
use manifold_node_engine::scene::transform::Transform;
use manifold_water_rigid::physics::{CopySpawn, DEFAULT_DENSITY, RigidBody, RigidSceneInputs, RigidSimulation};

const WORLD: &str = "node.physics_world";
const BODY_SLOTS: usize = 64;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/copy_groups").join(name)
}

/// What the app's loader hook does: the file's own presets go into the
/// catalog before every instance's card is rebuilt against it.
fn install(presets: &[EmbeddedPreset]) {
    let (mut effect, mut generator, mut modifier) = (Vec::new(), Vec::new(), Vec::new());
    for preset in presets {
        let Some(id) = preset.id() else { continue };
        let entry = (id.as_str().to_string(), serde_json::to_string(&preset.def).unwrap(), preset.origin);
        match preset.kind {
            PresetKind::Effect => effect.push(entry),
            PresetKind::Generator => generator.push(entry),
            PresetKind::SceneModifier => modifier.push(entry),
        }
    }
    manifold_node_engine::load::preset_loader::set_project_presets(effect, generator, modifier);
}

struct Show {
    project: Project,
}

impl Show {
    fn load(name: &str) -> Self {
        let project = manifold_io::loader::load_project_with(&fixture(name), install).expect("fixture loads");
        Self { project }
    }

    /// The Physics Boxes instance and the graph it plays.
    fn physics(&self) -> (&PresetInstance, EffectGraphDef) {
        for layer in &self.project.timeline.layers {
            let Some(instance) = layer.gen_params() else { continue };
            let def = match layer.generator_graph() {
                Some(def) => def.clone(),
                // What the app plays: the catalog, where stock outranks the
                // file's own snapshot.
                None => (*manifold_nodes::bundled_presets::bundled_preset_def(instance.effect_type()).expect("a catalog preset")).clone(),
            };
            if def.nodes.iter().any(|n| n.type_id == WORLD) {
                return (instance, def);
            }
        }
        panic!("no Physics World in the show");
    }
}

struct Values<'a> {
    def: &'a EffectGraphDef,
    instance: &'a PresetInstance,
}

impl Values<'_> {
    fn node(&self, id: u32) -> &EffectGraphNode {
        self.def.nodes.iter().find(|n| n.id == id).expect("node")
    }

    /// The value a node param plays at: a bound card's saved value through
    /// its conversion, else the stored node value.
    fn get(&self, id: u32, param: &str) -> Option<f32> {
        let node = self.node(id);
        let metadata = self.def.preset_metadata.as_ref();
        let bindings = metadata.into_iter().flat_map(|m| m.bindings.iter()).chain(self.instance.user_added_bindings());
        for binding in bindings {
            let BindingTarget::Node { node_id, param: target } = &binding.target else { continue };
            if *node_id != node.node_id || target != param {
                continue;
            }
            assert!(binding.scale == 1.0 && binding.offset == 0.0, "{} remaps", binding.id);
            let card = match self.instance.params.get(&binding.id) {
                Some(_) => self.instance.get_base_param(&binding.id),
                None => binding.default_value,
            };
            return Some(match binding.convert {
                ParamConvert::Float => card,
                ParamConvert::IntRound | ParamConvert::EnumRound | ParamConvert::Trigger => card.round(),
                ParamConvert::BoolThreshold => f32::from(u8::from(card >= 0.5)),
            });
        }
        match node.params.get(param)? {
            SerializedParamValue::Float { value } => Some(*value),
            SerializedParamValue::Int { value } => Some(*value as f32),
            SerializedParamValue::Enum { value } => Some(*value as f32),
            SerializedParamValue::Bool { value } => Some(f32::from(u8::from(*value))),
            other => panic!("{param}: {other:?}"),
        }
    }

    fn must(&self, id: u32, param: &str) -> f32 {
        self.get(id, param).unwrap_or_else(|| panic!("node {id} has no {param}"))
    }

    /// A world input: a wired Value node's value, else the world's own.
    fn world_input(&self, world: u32, port: &str, default: f32) -> f32 {
        match self.def.wires.iter().find(|w| w.to_node == world && w.to_port == port) {
            Some(wire) => {
                assert_eq!(self.node(wire.from_node).type_id, "node.value", "{port} wired from a non-value node");
                self.must(wire.from_node, "value")
            }
            None => self.get(world, port).unwrap_or(default),
        }
    }

    fn body(&self, id: u32) -> RigidBody {
        let transform = self
            .def
            .wires
            .iter()
            .find(|w| w.to_node == id && w.to_port == "transform")
            .expect("transform wired")
            .from_node;
        assert_eq!(self.node(transform).type_id, "node.transform_3d");
        assert!(
            !self.def.wires.iter().any(|w| w.to_node == transform || (w.to_node == id && w.to_port != "transform")),
            "the harness reads stored values only",
        );
        let axes = |names: [&str; 3], default: f32| names.map(|name| self.get(transform, name).unwrap_or(default));
        RigidBody {
            transform: Transform {
                pos: axes(["pos_x", "pos_y", "pos_z"], 0.0),
                rot_euler: axes(["rot_x", "rot_y", "rot_z"], 0.0),
                scale: axes(["scale_x", "scale_y", "scale_z"], 1.0),
                billboard: self.get(transform, "billboard").unwrap_or(0.0) >= 0.5,
            },
            enabled: self.get(id, "enabled").unwrap_or(1.0) >= 0.5,
            shape: self.get(id, "shape").unwrap_or(1.0) as u32,
            kind: self.get(id, "motion").unwrap_or(1.0) as u32,
            density: self.get(id, "density").unwrap_or(DEFAULT_DENSITY),
            friction: self.get(id, "friction").unwrap_or(0.5),
            bounce: self.get(id, "bounce").unwrap_or(0.15),
            spawn: self.spawn(id),
            ..RigidBody::default()
        }
    }

    fn spawn(&self, id: u32) -> CopySpawn {
        let defaults = CopySpawn::default();
        CopySpawn {
            count: self.get(id, "copy_count").unwrap_or(defaults.count),
            spacing: self.get(id, "copy_size_spacing").unwrap_or(defaults.spacing),
            columns: self.get(id, "copy_columns").unwrap_or(defaults.columns),
            layout: self.get(id, "copy_layout").unwrap_or(defaults.layout),
            turn: ["copy_turn_x", "copy_turn_y", "copy_turn_z"].map(|name| self.get(id, name).unwrap_or(0.0)),
        }
    }
}

fn digest(poses: impl Iterator<Item = Transform>) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for pose in poses {
        for value in pose.pos.into_iter().chain(pose.rot_euler).chain(pose.scale) {
            for byte in value.to_bits().to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    hash
}

struct Played {
    digest: u64,
    made: usize,
    not_made: usize,
}

fn play(name: &str) -> Played {
    let show = Show::load(name);
    let (instance, def) = show.physics();
    let values = Values { def: &def, instance };
    let world = def.nodes.iter().find(|n| n.type_id == WORLD).unwrap().id;
    let mut scene = RigidSceneInputs {
        bodies: vec![None; BODY_SLOTS],
        gravity: [
            values.world_input(world, "gravity_x", 0.0),
            values.world_input(world, "gravity_y", -9.81),
            values.world_input(world, "gravity_z", 0.0),
        ],
        ..RigidSceneInputs::default()
    };
    let mut group = None;
    for wire in def.wires.iter().filter(|w| w.to_node == world) {
        if let Some(slot) = wire.to_port.strip_prefix("body_") {
            scene.bodies[slot.parse::<usize>().unwrap()] = Some(values.body(wire.from_node));
        } else if let Some(slot) = wire.to_port.strip_prefix("copies_") {
            assert_eq!(slot, "0", "one copy group");
            group = Some(values.body(wire.from_node));
        }
    }
    let group = group.expect("a copy group");
    println!("{name}: {:?} at {:?} gravity {:?}", group.spawn, group.transform, scene.gravity);
    scene.groups = vec![Some(group)];
    let speed = values.world_input(world, "speed", 1.0);
    let reset = values.world_input(world, "reset", 0.0);
    let mut simulation = RigidSimulation::default();
    for frame in 0..=120 {
        simulation.advance_scene(&scene, Seconds(f64::from(frame) / 60.0), speed, reset).unwrap();
    }
    let digest = digest(simulation.poses.iter().chain(simulation.group_poses(0)).copied());
    let played = Played { digest, made: simulation.active_copies(0), not_made: simulation.copies_not_made() };
    println!("{name}: digest {:#018x}, {} made, {} not made", played.digest, played.made, played.not_made);
    played
}

#[test]
fn mirage_physics_plays_as_it_did_on_main() {
    let played = play("Mirage Physics.manifold");
    assert_eq!(played.made, 1_000);
    assert_eq!(played.digest, 0x8a47_86af_8ca5_dc25, "Mirage Physics left main's trajectory");
}

#[test]
fn box3d_testing_fills_the_copy_pool_and_plays_as_it_did_on_main() {
    let played = play("box3DTesting.manifold");
    assert_eq!((played.made, played.not_made), (4_000, 0));
    assert_eq!(played.digest, 0x0d0b_9bc5_3009_5639, "box3DTesting left main's trajectory");
}
