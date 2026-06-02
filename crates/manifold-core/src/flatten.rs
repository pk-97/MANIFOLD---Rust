//! Group flattening — the `EffectGraphDef -> EffectGraphDef` preprocessing pass
//! that expands embedded node groups into a flat document the runtime already
//! knows how to load.
//!
//! See `docs/NODE_GROUPS_DESIGN.md`. The short version: a **group** is a node
//! ([`EffectGraphNode::group`] is `Some`) wrapping a sub-graph plus a declared
//! interface. [`flatten_groups`] replaces every group node with its inlined
//! body — inner handles prefixed with the group instance's handle
//! (`soft_focus/blur`), the body's `system.group_input` / `system.group_output`
//! boundary nodes folded away, and the wires that crossed the boundary
//! re-anchored to the concrete inner nodes. The result contains no group nodes
//! and is structurally identical to a hand-wired flat document, so nothing
//! downstream (the loader, executor, performance surface) ever knows groups
//! existed.
//!
//! This is pure data manipulation: no GPU, no renderer types, no registry. The
//! authoritative *type*-check on the rewired wires happens later, when the flat
//! document goes through the renderer's `instantiate_def`; the flattener only
//! does structural rewriting.

use std::collections::{BTreeMap, BTreeSet};

use crate::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID,
    GroupDef, SerializedParamValue,
};

/// Delimiter between a group instance's handle and an inner node's handle in a
/// flattened document (`soft_focus/blur`). Reserved: a user handle may not
/// contain it. Confirmed clear of every shipping preset (see the design doc).
const HANDLE_DELIM: char = '/';

/// Defensive cap on group-nesting depth. Embedded-by-value groups form a finite
/// tree and cannot recurse infinitely, so this only fires on pathological input
/// (or, once reference-groups land, a true ref cycle).
const MAX_DEPTH: usize = 64;

/// Which end of a wire an error refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireSide {
    From,
    To,
}

/// Everything that can go wrong while flattening. Structured (not stringly) so
/// tests can assert the exact failure and the caller can render a precise
/// message.
#[derive(Debug, Clone, PartialEq)]
pub enum FlattenError {
    /// A wire references a group port that the group's interface doesn't declare.
    UnknownGroupPort {
        group_handle: String,
        port: String,
        side: WireSide,
    },
    /// A `system.group_output` port has zero or more-than-one inner producer.
    AmbiguousGroupOutput {
        group_handle: String,
        port: String,
        producers: usize,
    },
    /// Two interface ports or two interface params share a name.
    DuplicateInterfaceName {
        group_handle: String,
        name: String,
    },
    /// A group instance's `params` key isn't declared in `interface.params`.
    UnknownGroupParam {
        group_handle: String,
        param: String,
    },
    /// An `interface.params` entry points at an inner `(handle, param)` that
    /// doesn't exist among the group's direct children.
    GroupParamTargetMissing {
        group_handle: String,
        target_handle: String,
        target_param: String,
    },
    /// A user handle contains the reserved [`HANDLE_DELIM`] character.
    ReservedHandleChar { handle: String },
    /// A group node carries no `handle`; a group instance must be named, since
    /// its handle is the namespace root for every inner node.
    MissingGroupHandle { node_id: u32 },
    /// A wire uses a boundary node the wrong way round (into a `group_input`,
    /// or out of a `group_output`).
    MalformedBoundaryWire { node_id: u32, side: WireSide },
    /// A wire connects a group's input directly to its output. Legal in
    /// principle but unsupported in v1 — insert an explicit pass-through node.
    PassthroughNotSupported {
        group_handle: String,
        input_port: String,
        output_port: String,
    },
    /// Nesting exceeded [`MAX_DEPTH`] (pathological input or a reference cycle).
    GroupCycle { depth: usize },
}

impl std::fmt::Display for FlattenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlattenError::UnknownGroupPort {
                group_handle,
                port,
                side,
            } => write!(
                f,
                "group '{group_handle}': wire {side:?} references undeclared port '{port}'"
            ),
            FlattenError::AmbiguousGroupOutput {
                group_handle,
                port,
                producers,
            } => write!(
                f,
                "group '{group_handle}': output '{port}' has {producers} inner producers (need exactly 1)"
            ),
            FlattenError::DuplicateInterfaceName { group_handle, name } => {
                write!(f, "group '{group_handle}': duplicate interface name '{name}'")
            }
            FlattenError::UnknownGroupParam {
                group_handle,
                param,
            } => write!(
                f,
                "group '{group_handle}': override targets undeclared param '{param}'"
            ),
            FlattenError::GroupParamTargetMissing {
                group_handle,
                target_handle,
                target_param,
            } => write!(
                f,
                "group '{group_handle}': param routes to missing inner target '{target_handle}.{target_param}'"
            ),
            FlattenError::ReservedHandleChar { handle } => {
                write!(f, "handle '{handle}' contains the reserved '/' character")
            }
            FlattenError::MissingGroupHandle { node_id } => write!(
                f,
                "group node {node_id} has no handle (a group instance must be named)"
            ),
            FlattenError::MalformedBoundaryWire { node_id, side } => {
                write!(f, "node {node_id}: wire {side:?} misuses a group boundary node")
            }
            FlattenError::PassthroughNotSupported {
                group_handle,
                input_port,
                output_port,
            } => write!(
                f,
                "group '{group_handle}': direct input '{input_port}' -> output '{output_port}' \
                 passthrough is unsupported; insert an explicit pass-through node"
            ),
            FlattenError::GroupCycle { depth } => write!(
                f,
                "group nesting exceeded depth {depth} (reference cycle or pathological nesting)"
            ),
        }
    }
}

impl std::error::Error for FlattenError {}

/// Expand every group in `def` into a flat document. Groupless documents are
/// returned clone-equal (ids preserved); documents containing groups are
/// renumbered with fresh, unique node ids.
pub fn flatten_groups(def: &EffectGraphDef) -> Result<EffectGraphDef, FlattenError> {
    // Fast path: nothing to do. Preserves the document byte-for-byte (ids and
    // all), so every existing flat preset is provably untouched.
    if !def.nodes.iter().any(|n| n.group.is_some()) {
        return Ok(def.clone());
    }

    let mut alloc = IdAlloc { next: 0 };
    // The top level has no group_input / group_output of its own, so the
    // returned boundary maps are empty; we keep only nodes + wires.
    let frag = flatten_fragment(&def.nodes, &def.wires, "", "<root>", &mut alloc, 0)?;

    Ok(EffectGraphDef {
        version: def.version,
        name: def.name.clone(),
        description: def.description.clone(),
        preset_metadata: def.preset_metadata.clone(),
        nodes: frag.nodes,
        wires: frag.wires,
    })
}

struct IdAlloc {
    next: u32,
}

impl IdAlloc {
    fn alloc(&mut self) -> u32 {
        let id = self.next;
        self.next += 1;
        id
    }
}

/// A fully-flattened graph fragment. For a *group body* the boundary maps carry
/// how its `group_input` / `group_output` ports connect to concrete inner
/// nodes; for the top level they're empty.
struct FlatFragment {
    nodes: Vec<EffectGraphNode>,
    wires: Vec<EffectGraphWire>,
    /// `group_input` port name -> the concrete inner endpoints it feeds.
    input_consumers: BTreeMap<String, Vec<(u32, String)>>,
    /// `group_output` port name -> the single concrete inner endpoint feeding it.
    output_producer: BTreeMap<String, (u32, String)>,
}

/// How a node id in the current fragment resolves once flattened.
enum Resolved {
    /// An ordinary node, now living at this fresh flat id.
    Plain(u32),
    /// This fragment's own `system.group_input` boundary.
    FragmentInput,
    /// This fragment's own `system.group_output` boundary.
    FragmentOutput,
    /// An expanded sub-group, with its boundary routing and declared ports.
    Group {
        handle: String,
        input_consumers: BTreeMap<String, Vec<(u32, String)>>,
        output_producer: BTreeMap<String, (u32, String)>,
        valid_inputs: BTreeSet<String>,
        valid_outputs: BTreeSet<String>,
    },
}

/// The producer side of a wire, after resolution.
enum Producer {
    Concrete(u32, String),
    FromFragmentInput(String),
}

/// The consumer side of a wire, after resolution.
enum Sink {
    Concrete(Vec<(u32, String)>),
    ToFragmentOutput(String),
}

fn flatten_fragment(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    prefix: &str,
    scope_label: &str,
    alloc: &mut IdAlloc,
    depth: usize,
) -> Result<FlatFragment, FlattenError> {
    if depth > MAX_DEPTH {
        return Err(FlattenError::GroupCycle { depth });
    }

    let mut out_nodes: Vec<EffectGraphNode> = Vec::new();
    let mut out_wires: Vec<EffectGraphWire> = Vec::new();
    let mut input_consumers: BTreeMap<String, Vec<(u32, String)>> = BTreeMap::new();
    let mut output_producer: BTreeMap<String, (u32, String)> = BTreeMap::new();
    let mut resolved: BTreeMap<u32, Resolved> = BTreeMap::new();

    // ── Node pass: classify each node, expand sub-groups inline ──
    for node in nodes {
        if node.type_id == GROUP_INPUT_TYPE_ID {
            resolved.insert(node.id, Resolved::FragmentInput);
            continue;
        }
        if node.type_id == GROUP_OUTPUT_TYPE_ID {
            resolved.insert(node.id, Resolved::FragmentOutput);
            continue;
        }

        if let Some(group) = node.group.as_deref() {
            let handle = node
                .handle
                .as_deref()
                .ok_or(FlattenError::MissingGroupHandle { node_id: node.id })?;
            reject_reserved(handle)?;
            check_unique_interface(group, handle)?;

            let child_prefix = format!("{prefix}{handle}{HANDLE_DELIM}");
            let mut body =
                flatten_fragment(&group.nodes, &group.wires, &child_prefix, handle, alloc, depth + 1)?;

            apply_param_overrides(node, group, &child_prefix, &mut body.nodes)?;

            let valid_inputs = group.interface.inputs.iter().map(|p| p.name.clone()).collect();
            let valid_outputs = group.interface.outputs.iter().map(|p| p.name.clone()).collect();

            out_nodes.append(&mut body.nodes);
            out_wires.append(&mut body.wires);

            resolved.insert(
                node.id,
                Resolved::Group {
                    handle: handle.to_string(),
                    input_consumers: body.input_consumers,
                    output_producer: body.output_producer,
                    valid_inputs,
                    valid_outputs,
                },
            );
            continue;
        }

        // Ordinary node: fresh id, prefixed handle, copied through.
        if let Some(h) = node.handle.as_deref() {
            reject_reserved(h)?;
        }
        let new_id = alloc.alloc();
        let mut clone = node.clone();
        clone.id = new_id;
        clone.handle = node.handle.as_deref().map(|h| format!("{prefix}{h}"));
        clone.group = None;
        out_nodes.push(clone);
        resolved.insert(node.id, Resolved::Plain(new_id));
    }

    // ── Wire pass: resolve endpoints, fold boundaries, emit concrete wires ──
    for w in wires {
        let producer = resolve_from(w, &resolved, scope_label)?;
        let sink = resolve_to(w, &resolved, scope_label)?;

        match (producer, sink) {
            // Wire feeding from this fragment's input -> record as inner
            // consumers of that input port (folded, not emitted).
            (Producer::FromFragmentInput(port), Sink::Concrete(consumers)) => {
                input_consumers.entry(port).or_default().extend(consumers);
            }
            // Producer -> this fragment's output -> record the producer (folded).
            (Producer::Concrete(pid, pport), Sink::ToFragmentOutput(port)) => {
                if output_producer.contains_key(&port) {
                    return Err(FlattenError::AmbiguousGroupOutput {
                        group_handle: scope_label.to_string(),
                        port,
                        producers: 2,
                    });
                }
                output_producer.insert(port, (pid, pport));
            }
            // Direct input->output bypass: unsupported in v1.
            (Producer::FromFragmentInput(input_port), Sink::ToFragmentOutput(output_port)) => {
                return Err(FlattenError::PassthroughNotSupported {
                    group_handle: scope_label.to_string(),
                    input_port,
                    output_port,
                });
            }
            // Ordinary internal wire (possibly fanned out across consumers).
            (Producer::Concrete(pid, pport), Sink::Concrete(consumers)) => {
                for (cid, cport) in consumers {
                    out_wires.push(EffectGraphWire {
                        from_node: pid,
                        from_port: pport.clone(),
                        to_node: cid,
                        to_port: cport,
                    });
                }
            }
        }
    }

    Ok(FlatFragment {
        nodes: out_nodes,
        wires: out_wires,
        input_consumers,
        output_producer,
    })
}

fn resolve_from(
    w: &EffectGraphWire,
    resolved: &BTreeMap<u32, Resolved>,
    scope_label: &str,
) -> Result<Producer, FlattenError> {
    match resolved.get(&w.from_node) {
        Some(Resolved::Plain(id)) => Ok(Producer::Concrete(*id, w.from_port.clone())),
        Some(Resolved::FragmentInput) => Ok(Producer::FromFragmentInput(w.from_port.clone())),
        Some(Resolved::FragmentOutput) => Err(FlattenError::MalformedBoundaryWire {
            node_id: w.from_node,
            side: WireSide::From,
        }),
        Some(Resolved::Group {
            handle,
            output_producer,
            valid_outputs,
            ..
        }) => {
            if !valid_outputs.contains(&w.from_port) {
                return Err(FlattenError::UnknownGroupPort {
                    group_handle: handle.clone(),
                    port: w.from_port.clone(),
                    side: WireSide::From,
                });
            }
            match output_producer.get(&w.from_port) {
                Some((id, port)) => Ok(Producer::Concrete(*id, port.clone())),
                None => Err(FlattenError::AmbiguousGroupOutput {
                    group_handle: handle.clone(),
                    port: w.from_port.clone(),
                    producers: 0,
                }),
            }
        }
        None => Err(FlattenError::UnknownGroupPort {
            group_handle: scope_label.to_string(),
            port: w.from_port.clone(),
            side: WireSide::From,
        }),
    }
}

fn resolve_to(
    w: &EffectGraphWire,
    resolved: &BTreeMap<u32, Resolved>,
    scope_label: &str,
) -> Result<Sink, FlattenError> {
    match resolved.get(&w.to_node) {
        Some(Resolved::Plain(id)) => Ok(Sink::Concrete(vec![(*id, w.to_port.clone())])),
        Some(Resolved::FragmentOutput) => Ok(Sink::ToFragmentOutput(w.to_port.clone())),
        Some(Resolved::FragmentInput) => Err(FlattenError::MalformedBoundaryWire {
            node_id: w.to_node,
            side: WireSide::To,
        }),
        Some(Resolved::Group {
            handle,
            input_consumers,
            valid_inputs,
            ..
        }) => {
            if !valid_inputs.contains(&w.to_port) {
                return Err(FlattenError::UnknownGroupPort {
                    group_handle: handle.clone(),
                    port: w.to_port.clone(),
                    side: WireSide::To,
                });
            }
            // Declared-but-unused input -> empty list -> outer wire is dropped.
            Ok(Sink::Concrete(
                input_consumers.get(&w.to_port).cloned().unwrap_or_default(),
            ))
        }
        None => Err(FlattenError::UnknownGroupPort {
            group_handle: scope_label.to_string(),
            port: w.to_port.clone(),
            side: WireSide::To,
        }),
    }
}

/// Apply a group instance's param overrides (and unoverridden interface
/// defaults) onto the inner nodes the interface routes them to.
fn apply_param_overrides(
    node: &EffectGraphNode,
    group: &GroupDef,
    child_prefix: &str,
    body_nodes: &mut [EffectGraphNode],
) -> Result<(), FlattenError> {
    let group_handle = node.handle.as_deref().unwrap_or_default();

    // Every override key must name a declared interface param.
    for key in node.params.keys() {
        if !group.interface.params.iter().any(|p| &p.name == key) {
            return Err(FlattenError::UnknownGroupParam {
                group_handle: group_handle.to_string(),
                param: key.clone(),
            });
        }
    }

    for p in &group.interface.params {
        let value = node
            .params
            .get(&p.name)
            .cloned()
            .or_else(|| p.default.clone());
        let Some(value) = value else { continue };

        let target_handle = format!("{child_prefix}{}", p.target_handle);
        let set = set_param_on(body_nodes, &target_handle, &p.target_param, value);
        if !set {
            return Err(FlattenError::GroupParamTargetMissing {
                group_handle: group_handle.to_string(),
                target_handle: p.target_handle.clone(),
                target_param: p.target_param.clone(),
            });
        }
    }
    Ok(())
}

fn set_param_on(
    nodes: &mut [EffectGraphNode],
    handle: &str,
    param: &str,
    value: SerializedParamValue,
) -> bool {
    for n in nodes.iter_mut() {
        if n.handle.as_deref() == Some(handle) {
            n.params.insert(param.to_string(), value);
            return true;
        }
    }
    false
}

fn reject_reserved(handle: &str) -> Result<(), FlattenError> {
    if handle.contains(HANDLE_DELIM) {
        Err(FlattenError::ReservedHandleChar {
            handle: handle.to_string(),
        })
    } else {
        Ok(())
    }
}

fn check_unique_interface(group: &GroupDef, group_handle: &str) -> Result<(), FlattenError> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for name in group.interface.inputs.iter().map(|p| p.name.as_str()) {
        if !seen.insert(name) {
            return Err(FlattenError::DuplicateInterfaceName {
                group_handle: group_handle.to_string(),
                name: name.to_string(),
            });
        }
    }
    seen.clear();
    for name in group.interface.outputs.iter().map(|p| p.name.as_str()) {
        if !seen.insert(name) {
            return Err(FlattenError::DuplicateInterfaceName {
                group_handle: group_handle.to_string(),
                name: name.to_string(),
            });
        }
    }
    seen.clear();
    for name in group.interface.params.iter().map(|p| p.name.as_str()) {
        if !seen.insert(name) {
            return Err(FlattenError::DuplicateInterfaceName {
                group_handle: group_handle.to_string(),
                name: name.to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_graph_def::{
        EFFECT_GRAPH_VERSION, GROUP_TYPE_ID, GroupInterface, GroupParamDef, InterfacePortDef,
    };

    // ── builders to keep the graph fixtures readable ──

    fn node(id: u32, type_id: &str, handle: Option<&str>) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: crate::NodeId::default(),
            type_id: type_id.to_string(),
            handle: handle.map(|h| h.to_string()),
            params: BTreeMap::new(),
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }

    fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire {
            from_node,
            from_port: from_port.to_string(),
            to_node,
            to_port: to_port.to_string(),
        }
    }

    fn port(name: &str) -> InterfacePortDef {
        InterfacePortDef {
            name: name.to_string(),
            port_type: "Texture2D".to_string(),
        }
    }

    fn def(nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>) -> EffectGraphDef {
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes,
            wires,
        }
    }

    fn find<'a>(d: &'a EffectGraphDef, handle: &str) -> &'a EffectGraphNode {
        d.nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some(handle))
            .unwrap_or_else(|| panic!("no node with handle {handle}; have {:?}", handles(d)))
    }

    fn handles(d: &EffectGraphDef) -> Vec<Option<String>> {
        d.nodes.iter().map(|n| n.handle.clone()).collect()
    }

    /// Resolve a flattened wire to `(from_handle.from_port -> to_handle.to_port)`
    /// so assertions read in handle-space, independent of renumbering.
    fn wires_by_handle(d: &EffectGraphDef) -> Vec<(String, String, String, String)> {
        let id_to_handle: BTreeMap<u32, String> = d
            .nodes
            .iter()
            .map(|n| (n.id, n.handle.clone().unwrap_or_else(|| format!("#{}", n.id))))
            .collect();
        let mut out: Vec<_> = d
            .wires
            .iter()
            .map(|w| {
                (
                    id_to_handle[&w.from_node].clone(),
                    w.from_port.clone(),
                    id_to_handle[&w.to_node].clone(),
                    w.to_port.clone(),
                )
            })
            .collect();
        out.sort();
        out
    }

    /// A `soft_focus` group: GroupInput.src fans to blur.src and mix.a;
    /// blur.out -> mix.b; mix.out -> GroupOutput.out; `amount` -> mix.t.
    fn soft_focus_group(id: u32, handle: &str) -> EffectGraphNode {
        let mut g = node(id, GROUP_TYPE_ID, Some(handle));
        g.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: vec![port("src")],
                outputs: vec![port("out")],
                params: vec![GroupParamDef {
                    name: "amount".to_string(),
                    target_handle: "mix".to_string(),
                    target_param: "t".to_string(),
                    default: Some(SerializedParamValue::Float { value: 0.5 }),
                }],
            },
            nodes: vec![
                node(0, GROUP_INPUT_TYPE_ID, None),
                node(1, "node.blur", Some("blur")),
                node(2, "node.mix", Some("mix")),
                node(3, GROUP_OUTPUT_TYPE_ID, None),
            ],
            wires: vec![
                wire(0, "src", 1, "src"),
                wire(0, "src", 2, "a"),
                wire(1, "out", 2, "b"),
                wire(2, "out", 3, "out"),
            ],
        }));
        g
    }

    #[test]
    fn flattens_single_group_to_concrete_nodes() {
        // source -> group.src ; group.out -> final
        let d = def(
            vec![
                node(0, "system.source", Some("source")),
                soft_focus_group(1, "soft_focus"),
                node(2, "system.final_output", Some("final")),
            ],
            vec![wire(0, "out", 1, "src"), wire(1, "out", 2, "in")],
        );
        let flat = flatten_groups(&d).unwrap();

        // No group / boundary nodes survive.
        assert!(flat.nodes.iter().all(|n| n.group.is_none()));
        assert!(flat.nodes.iter().all(|n| n.type_id != GROUP_INPUT_TYPE_ID
            && n.type_id != GROUP_OUTPUT_TYPE_ID
            && n.type_id != GROUP_TYPE_ID));
        // Inner handles are prefixed.
        find(&flat, "soft_focus/blur");
        find(&flat, "soft_focus/mix");
        // Wiring matches the hand-flat equivalent.
        assert_eq!(
            wires_by_handle(&flat),
            vec![
                ("soft_focus/blur".into(), "out".into(), "soft_focus/mix".into(), "b".into()),
                ("soft_focus/mix".into(), "out".into(), "final".into(), "in".into()),
                ("source".into(), "out".into(), "soft_focus/blur".into(), "src".into()),
                ("source".into(), "out".into(), "soft_focus/mix".into(), "a".into()),
            ]
        );
    }

    #[test]
    fn fans_out_group_input_to_all_inner_consumers() {
        let d = def(
            vec![node(0, "system.source", Some("source")), soft_focus_group(1, "g")],
            vec![wire(0, "out", 1, "src")],
        );
        let flat = flatten_groups(&d).unwrap();
        // One external input wire became two (blur.src and mix.a).
        let from_source: Vec<_> = wires_by_handle(&flat)
            .into_iter()
            .filter(|(f, _, _, _)| f == "source")
            .collect();
        assert_eq!(from_source.len(), 2);
    }

    #[test]
    fn routes_group_param_override_to_inner_target() {
        let mut g = soft_focus_group(1, "g");
        g.params
            .insert("amount".to_string(), SerializedParamValue::Float { value: 0.9 });
        let d = def(vec![g], vec![]);
        let flat = flatten_groups(&d).unwrap();
        let mix = find(&flat, "g/mix");
        assert_eq!(
            mix.params.get("t"),
            Some(&SerializedParamValue::Float { value: 0.9 })
        );
    }

    #[test]
    fn unoverridden_param_falls_back_to_interface_default() {
        let d = def(vec![soft_focus_group(1, "g")], vec![]);
        let flat = flatten_groups(&d).unwrap();
        let mix = find(&flat, "g/mix");
        assert_eq!(
            mix.params.get("t"),
            Some(&SerializedParamValue::Float { value: 0.5 })
        );
    }

    #[test]
    fn two_instances_of_same_group_get_distinct_prefixes() {
        let d = def(
            vec![soft_focus_group(1, "a"), soft_focus_group(2, "b")],
            vec![],
        );
        let flat = flatten_groups(&d).unwrap();
        find(&flat, "a/blur");
        find(&flat, "b/blur");
        find(&flat, "a/mix");
        find(&flat, "b/mix");
        // All ids unique.
        let ids: BTreeSet<u32> = flat.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids.len(), flat.nodes.len());
    }

    #[test]
    fn nested_groups_flatten_recursively() {
        // outer group contains an inner soft_focus group plus passthrough wiring.
        let inner = soft_focus_group(1, "inner");
        let mut outer = node(5, GROUP_TYPE_ID, Some("outer"));
        outer.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: vec![port("src")],
                outputs: vec![port("out")],
                params: vec![],
            },
            nodes: vec![node(0, GROUP_INPUT_TYPE_ID, None), inner, node(2, GROUP_OUTPUT_TYPE_ID, None)],
            wires: vec![wire(0, "src", 1, "src"), wire(1, "out", 2, "out")],
        }));
        let d = def(
            vec![node(0, "system.source", Some("source")), outer, node(9, "system.final_output", Some("final"))],
            vec![wire(0, "out", 5, "src"), wire(5, "out", 9, "in")],
        );
        let flat = flatten_groups(&d).unwrap();
        // Doubly-prefixed inner handles.
        find(&flat, "outer/inner/blur");
        find(&flat, "outer/inner/mix");
        // End-to-end connectivity survived two levels of folding.
        let wh = wires_by_handle(&flat);
        assert!(wh.contains(&(
            "source".into(),
            "out".into(),
            "outer/inner/blur".into(),
            "src".into()
        )));
        assert!(wh.contains(&(
            "outer/inner/mix".into(),
            "out".into(),
            "final".into(),
            "in".into()
        )));
    }

    #[test]
    fn groupless_def_is_returned_unchanged() {
        let d = def(
            vec![
                node(0, "system.source", Some("source")),
                node(1, "node.blur", Some("blur")),
                node(2, "system.final_output", Some("final")),
            ],
            vec![wire(0, "out", 1, "src"), wire(1, "out", 2, "in")],
        );
        let flat = flatten_groups(&d).unwrap();
        assert_eq!(flat, d);
    }

    // ── error cases ──

    #[test]
    fn unknown_group_port_errors() {
        let d = def(
            vec![node(0, "system.source", Some("source")), soft_focus_group(1, "g")],
            vec![wire(0, "out", 1, "nope")],
        );
        assert!(matches!(
            flatten_groups(&d),
            Err(FlattenError::UnknownGroupPort { side: WireSide::To, .. })
        ));
    }

    #[test]
    fn ambiguous_group_output_errors() {
        let mut g = soft_focus_group(1, "g");
        // Wire a second producer (blur.out) into GroupOutput.out as well.
        if let Some(body) = g.group.as_deref_mut() {
            body.wires.push(wire(1, "out", 3, "out"));
        }
        let d = def(vec![g], vec![]);
        assert!(matches!(
            flatten_groups(&d),
            Err(FlattenError::AmbiguousGroupOutput { producers: 2, .. })
        ));
    }

    #[test]
    fn duplicate_interface_name_errors() {
        let mut g = soft_focus_group(1, "g");
        if let Some(body) = g.group.as_deref_mut() {
            body.interface.inputs.push(port("src"));
        }
        let d = def(vec![g], vec![]);
        assert!(matches!(
            flatten_groups(&d),
            Err(FlattenError::DuplicateInterfaceName { .. })
        ));
    }

    #[test]
    fn unknown_group_param_errors() {
        let mut g = soft_focus_group(1, "g");
        g.params
            .insert("bogus".to_string(), SerializedParamValue::Float { value: 1.0 });
        let d = def(vec![g], vec![]);
        assert!(matches!(
            flatten_groups(&d),
            Err(FlattenError::UnknownGroupParam { .. })
        ));
    }

    #[test]
    fn reserved_handle_char_errors() {
        let g = soft_focus_group(1, "bad/name");
        let d = def(vec![g], vec![]);
        assert!(matches!(
            flatten_groups(&d),
            Err(FlattenError::ReservedHandleChar { .. })
        ));
    }

    #[test]
    fn missing_group_handle_errors() {
        let mut g = soft_focus_group(1, "g");
        g.handle = None;
        let d = def(vec![g], vec![]);
        assert!(matches!(
            flatten_groups(&d),
            Err(FlattenError::MissingGroupHandle { .. })
        ));
    }

    #[test]
    fn passthrough_not_supported_errors() {
        let mut g = soft_focus_group(1, "g");
        if let Some(body) = g.group.as_deref_mut() {
            // GroupInput.src -> GroupOutput.out directly.
            body.wires.push(wire(0, "src", 3, "out"));
        }
        let d = def(vec![g], vec![]);
        assert!(matches!(
            flatten_groups(&d),
            Err(FlattenError::PassthroughNotSupported { .. })
        ));
    }
}
