//! Renderer-owned catalog contracts for freeze installation.

use std::sync::Arc;
use ahash::AHashSet;
use manifold_core::{NodeId, PresetTypeId};
use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_node_engine::freeze::install::*;
use manifold_node_engine::freeze::install::{def_content_key, effect_def_content_key, resolve_node_id};
use manifold_node_engine::param_binding::ParamTarget;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::persistence::PrimitiveRegistry;

fn registry() -> PrimitiveRegistry {
    PrimitiveRegistry::with_builtin()
}

#[cfg(test)]
mod tests {
    use crate::freeze::install::*;
    #[test]
    fn content_keyed_cache_separates_edited_from_canonical_and_negative_caches() {
        // An edited shape (different topology) must get its own fused entry by its
        // own content key and never clobber the canonical one; a non-fusable def
        // must cache `None` rather than recompile each call. Uses ColorGrade,
        // whose canonical shape fuses.
        let base = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(&PresetTypeId::new("ColorGrade"))
            .expect("ColorGrade canonical view");

        // Canonical content key fuses and is stable across calls (cache hit).
        let canon_a = fused_view_for(&base.canonical_def, base);
        let canon_b = fused_view_for(&base.canonical_def, base);
        assert!(canon_a.is_some(), "canonical ColorGrade must fuse");
        assert!(
            Arc::ptr_eq(canon_a.as_ref().unwrap(), canon_b.as_ref().unwrap()),
            "same def content must return the same cached Arc view",
        );

        // Mutating the def's content (duplicate a node → a structurally distinct
        // def) must route to a *different* cache entry, proving keying is by
        // content, not type id. We don't assert it fuses (the malformed dup may
        // strand → None); we assert the canonical entry is untouched afterward.
        let mut edited = (*base.canonical_def).clone();
        edited.nodes.push(edited.nodes[0].clone());
        assert_ne!(
            def_content_key(&base.canonical_def),
            def_content_key(&edited),
            "a structural edit must change the content key",
        );
        let _ = fused_view_for(&edited, base);
        let canon_c = fused_view_for(&base.canonical_def, base);
        assert!(
            Arc::ptr_eq(canon_a.as_ref().unwrap(), canon_c.as_ref().unwrap()),
            "an edited def's entry must not clobber the canonical entry",
        );
    }

    /// The fused view must carry the full binding-retarget map so the chain
    /// builder can repoint a per-instance USER binding (which lives off the def,
    /// on `PresetInstance.user_param_bindings`, and so is invisible to the
    /// content-keyed fuse) onto the fused node — exactly as the static card
    /// bindings are. Without this the map was discarded after retargeting the
    /// statics, and a user-exposed slider went inert the moment the effect
    /// re-fused on editor close (the effect/generator divergence: generators
    /// keep bindings in the def, so they retargeted; effects didn't).
    #[test]
    fn fused_view_carries_retarget_map_for_user_bindings() {
        let base = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(&PresetTypeId::new("ColorGrade"))
            .expect("ColorGrade canonical view");
        // Plain JSON-loaded view: no fusion, so nothing to retarget.
        assert!(
            base.fused_retarget.is_empty(),
            "unfused view must carry an empty retarget map",
        );

        let fused = fused_view_for(&base.canonical_def, base).expect("ColorGrade fuses");
        // Same routing the standalone `fuse_canonical_def` retarget asserts —
        // proving the map survived onto the cached view rather than being
        // dropped after the static-binding rewrite.
        assert_eq!(
            fused
                .fused_retarget
                .get(&("gain".to_string(), "gain".to_string()))
                .map(|(id, f)| (id.as_str(), f.as_str())),
            Some(("fused_region_0", "n0_gain")),
            "an inner (node_id, param) the fuse collapsed must resolve to its \
             fused uniform field so a user binding can be repointed onto it",
        );
        // The map is total over fused-away inner params (every param of every
        // collapsed node), so a user binding can never strand under fusion.
        assert_eq!(fused.fused_retarget.len(), 14, "all 7 ColorGrade atoms' params");
    }

    /// P5/D4: a Vec3 param (`node.brightness`'s `weights`) and a Vec4 param
    /// (`node.channel_mixer`'s `row0..row3`) both actually SEED correct
    /// per-component values into the fused node's `params` map — not just
    /// "the codegen text compiles" (the GPU parity tests already prove that
    /// downstream), but that install-time seeding (`effective_param_vec3`/
    /// `effective_param_vec4`) reconstructs the right `n{i}_<name>_x/_y/_z
    /// [_w]` fields from the atom's declared default, matching the exact
    /// component order `codegen.rs`'s struct/arg emission uses. Also reruns
    /// the `seeded_fields_match_wgsl_compute_params` drift guard on a region
    /// containing a non-scalar param, which the ColorGrade-only original
    /// never covered.
    #[test]
    fn vec3_and_vec4_params_seed_correct_component_values() {
        use manifold_node_engine::exec::effect_node::EffectNode;
        use manifold_node_engine::primitives::wgsl_compute::WgslCompute;
        let json = r#"{
            "version": 1, "name": "vec-params", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 2, "typeId": "node.brightness", "nodeId": "bright" },
                { "id": 3, "typeId": "node.channel_mixer", "nodeId": "mixer" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "source" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "source" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let fused = fuse_canonical_def(&def, &registry())
            .expect("contrast+brightness+channel_mixer all fuse (P5 lifts Vec3/Vec4)");
        let node = fused
            .def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.wgsl_compute")
            .expect("one fused region");

        // node.brightness is region index 1 (contrast=0, bright=1, mixer=2) —
        // the Vec3 `weights` default is BT.709 luma [0.2126, 0.7152, 0.0722].
        let get = |field: &str| match node.params.get(field) {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("expected a seeded Float at `{field}`, got {other:?}"),
        };
        assert_eq!(get("n1_weights_x"), 0.2126);
        assert_eq!(get("n1_weights_y"), 0.7152);
        assert_eq!(get("n1_weights_z"), 0.0722);

        // node.channel_mixer is region index 2 — row0's Vec4 default is the
        // identity matrix's first row [1.0, 0.0, 0.0, 0.0].
        assert_eq!(get("n2_row0_x"), 1.0);
        assert_eq!(get("n2_row0_y"), 0.0);
        assert_eq!(get("n2_row0_z"), 0.0);
        assert_eq!(get("n2_row0_w"), 0.0);
        // row1's default is [0.0, 1.0, 0.0, 0.0].
        assert_eq!(get("n2_row1_x"), 0.0);
        assert_eq!(get("n2_row1_y"), 1.0);

        // Drift guard (same pattern as `seeded_fields_match_wgsl_compute_
        // params`, on a region a Vec3/Vec4 param actually reaches): every
        // seeded field name must be a real reparsed WgslCompute param.
        let mut wc = WgslCompute::new();
        wc.set_wgsl_source(node.wgsl_source.as_deref().unwrap());
        let param_names: AHashSet<&str> =
            wc.parameters().iter().map(|p| p.name.as_ref()).collect();
        for field in node.params.keys() {
            assert!(
                param_names.contains(field.as_str()),
                "seeded field `{field}` is not a derived WgslCompute param — codegen drift"
            );
        }
    }

    /// The cached fused view retargets every outer-card binding onto its region's
    /// fused node, preserving the card surface: 9 bindings, all pointing at the
    /// fused node, at the matching `n{i}_{param}` field.
    #[test]
    fn fused_view_retargets_every_binding() {
        let view = fused_view_by_id(&PresetTypeId::new("ColorGrade"))
            .expect("ColorGrade has a fused view");
        assert_eq!(view.bindings.len(), 9, "all outer-card sliders survive");
        for b in &view.bindings {
            match &b.target {
                ParamTarget::Node { node_id, param } => {
                    assert_eq!(node_id.as_str(), "fused_region_0");
                    assert!(param.starts_with('n'), "retargeted to a fused field");
                }
                other => panic!("binding {:?} not retargeted to a node: {other:?}", b.id),
            }
        }
        // Spot-check two specific routings end-to-end through the cache.
        let field_for = |id: &str| {
            view.bindings
                .iter()
                .find(|b| AsRef::<str>::as_ref(&b.id) == id)
                .and_then(|b| match &b.target {
                    ParamTarget::Node { param, .. } => Some(param.clone()),
                    _ => None,
                })
        };
        assert_eq!(field_for("amount").as_deref(), Some("n5_amount"));
        assert_eq!(field_for("gain").as_deref(), Some("n0_gain"));
        assert_eq!(field_for("tint_focus").as_deref(), Some("n4_focus"));
    }

    /// A generator's `preset_metadata` binding is retargeted onto the fused node.
    /// checkerboard (Source) → gain → invert fuse into one region; the binding that
    /// drove `gain.gain` is repointed at the fused node's `n1_gain` field (gain is
    /// member 1), so the generator's modulation surface keeps driving the kernel.
    #[test]
    fn generator_binding_def_retargets_onto_fused() {
        use manifold_node_engine::persistence::EffectGraphDefExt;
        use manifold_core::effect_graph_def::BindingTarget;
        let json = r#"{
            "version": 1, "name": "FuseGen",
            "presetMetadata": {
                "id": "FuseGen", "displayName": "Fuse Gen", "category": "Diagnostic",
                "oscPrefix": "fuse_gen",
                "params": [{ "id": "g", "name": "Gain", "min": 0.0, "max": 4.0, "defaultValue": 2.0 }],
                "bindings": [{ "id": "g", "label": "Gain", "defaultValue": 2.0,
                    "target": { "kind": "node", "nodeId": "gain", "param": "gain" } }]
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "nodeId": "gen_in" },
                { "id": 1, "typeId": "node.checkerboard", "nodeId": "checker" },
                { "id": 2, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 4, "typeId": "node.absolute_value", "nodeId": "atomless" },
                { "id": 5, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" },
                { "fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let view = fused_generator_view_for(&def).expect("the generator fuses");
        let cached = fused_generator_view_for(&def).expect("cached view");
        assert!(Arc::ptr_eq(&view, &cached));
        let fused = view.def.clone();
        let (target, field) = view.retarget.get(&("gain".into(), "gain".into()))
            .expect("value edit route survives caching");
        let fused_id = NodeId::new("fused_region_0");
        assert_eq!(view.node_retarget.get(&NodeId::new("gain")), Some(&fused_id));
        assert_eq!(view.node_retarget.get(&NodeId::new("atomless")), Some(&fused_id));
        assert_eq!(view.node_retarget, cached.node_retarget, "cache retains member attribution");
        assert!(view.node_retarget.values().all(|id| {
            view.def.nodes.iter().any(|node| resolve_node_id(node) == *id)
        }));
        let mut graph = (*fused).clone().into_graph(&registry(), &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).unwrap();
        let runtime_id = graph.instance_by_node_id(target).unwrap();
        graph.set_param(runtime_id, field, ParamValue::Float(0.375)).unwrap();
        assert_eq!(graph.get_node(runtime_id).unwrap().params.get(field.as_str()), Some(&ParamValue::Float(0.375)));
        let meta = fused.preset_metadata.as_ref().expect("metadata preserved");
        assert_eq!(meta.bindings.len(), 1);
        match &meta.bindings[0].target {
            BindingTarget::Node { node_id, param } => {
                assert_eq!(node_id.as_str(), "fused_region_0", "binding re-anchored to the fused node");
                assert_eq!(param, "n1_gain", "gain is member 1, so its field is n1_gain");
            }
            other => panic!("binding not retargeted to a node: {other:?}"),
        }
    }

    /// The enum mirror of [`generator_binding_def_retargets_onto_fused`] +
    /// [`fused_view_retargets_every_binding`]: a binding with an `EnumRound`
    /// convert onto a member's Enum param (mix.mode — the FluidSim3D
    /// `container` shape) retargets onto the fused uniform field with its
    /// convert rewritten to `IntRound`, and the fused def passes the loader's
    /// convert check (`BindingConvertTypeMismatch` is exactly what stranded
    /// this before — the fused field introspects as Int, which rejects an
    /// Enum-producing convert). The atom fuses instead of being classify
    /// gated (59b3cf25 removed).
    #[test]
    fn enum_converted_binding_retargets_with_int_round_and_loads() {
        use manifold_core::effect_graph_def::BindingTarget;
        let json = r#"{
            "version": 1, "name": "EnumFuseGen",
            "presetMetadata": {
                "id": "EnumFuseGen", "displayName": "Enum Fuse Gen", "category": "Diagnostic",
                "oscPrefix": "enum_fuse_gen",
                "params": [{ "id": "m", "name": "Mode", "min": 0.0, "max": 4.0, "defaultValue": 3.0, "wholeNumbers": true }],
                "bindings": [{ "id": "m", "label": "Mode", "defaultValue": 3.0,
                    "target": { "kind": "node", "nodeId": "mix", "param": "mode" },
                    "convert": { "type": "EnumRound" } }]
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "nodeId": "gen_in" },
                { "id": 1, "typeId": "node.checkerboard", "nodeId": "checker" },
                { "id": 2, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 3, "typeId": "node.mix", "nodeId": "mix" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "a" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "b" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let reg = registry();
        let fused_view = fuse_generator_view(&def, &reg)
            .expect("an enum-binding-targeted member must fuse, not classify gate");
        let fused = &fused_view.def;
        let meta = fused.preset_metadata.as_ref().expect("metadata preserved");
        assert_eq!(meta.bindings.len(), 1);
        match &meta.bindings[0].target {
            BindingTarget::Node { node_id, param } => {
                assert_eq!(node_id.as_str(), "fused_region_0");
                assert_eq!(param, "n2_mode", "mix is member 2, so its field is n2_mode");
            }
            other => panic!("binding not retargeted to a node: {other:?}"),
        }
        assert_eq!(
            meta.bindings[0].convert,
            manifold_core::effects::ParamConvert::IntRound,
            "EnumRound must rewrite to IntRound on retarget — the fused u32 \
             field consumes Float and casts at the uniform-write boundary"
        );
        // The fused def must clear the loader's binding-convert validation —
        // the check that originally rejected EnumRound against the fused Int
        // field. A load error here means the rewrite regressed.
        use manifold_node_engine::persistence::EffectGraphDefExt;
        (**fused)
            .clone()
            .into_graph(&reg, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default())
            .expect("fused def with retargeted enum binding must load");
    }

    /// BUG-j8gy machinery: under `cfg(test)` the chain-build lookup compiles
    /// inline (deterministic — no worker), so a fusable def comes back `Ready`
    /// and lands in the content cache, exactly the pre-async behavior every
    /// chain-build test relies on.
    #[test]
    fn fused_effect_view_for_compiles_inline_in_tests() {
        let base = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(&PresetTypeId::new("ColorGrade"))
            .expect("ColorGrade canonical view");
        match fused_effect_view_for(&base.canonical_def, base) {
            FusedEffectLookup::Ready(view) => {
                assert!(
                    !view.fused_retarget.is_empty(),
                    "ColorGrade fuses — Ready must carry the fused view"
                );
            }
            FusedEffectLookup::Pending => panic!("test builds never report Pending"),
            FusedEffectLookup::Refused => panic!("ColorGrade has a fusable region"),
        }
        // Second lookup is a cache hit on the same content key.
        assert!(matches!(
            fused_effect_view_for(&base.canonical_def, base),
            FusedEffectLookup::Ready(_)
        ));
    }

    #[test]
    fn effect_key_ignores_binding_metadata() {
        // Two effect defs differing only in binding label/default_value/scale/offset
        // must produce the same effect content key AND byte-identical fused WGSL.
        // These fields never reach the generated shader — codegen only reads binding
        // targets via `param_is_binding_target` (region.rs:2178).
        let base = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(&PresetTypeId::new("ColorGrade"))
            .expect("ColorGrade canonical view");

        // Clone the base def and modify a binding's cosmetic fields
        let mut def_with_metadata = (*base.canonical_def).clone();
        if let Some(meta) = def_with_metadata.preset_metadata.as_mut()
            && let Some(binding) = meta.bindings.first_mut() {
                binding.label = "DIFFERENT_LABEL".to_string();
                binding.default_value = 999.0;
                binding.scale = 2.0;
                binding.offset = 1.0;
            }

        // Effect content key must be identical (metadata doesn't affect codegen)
        assert_eq!(
            effect_def_content_key(&base.canonical_def),
            effect_def_content_key(&def_with_metadata),
            "binding metadata (label/default_value/scale/offset) must not affect effect content key"
        );

        // Generator key MUST still distinguish these (generator runtime reads bindings
        // from the cached def, so normalization would lose metadata)
        assert_ne!(
            def_content_key(&base.canonical_def),
            def_content_key(&def_with_metadata),
            "generator key must still distinguish binding metadata differences"
        );
    }

    #[test]
    fn effect_key_distinguishes_binding_target_changes() {
        // Two effect defs differing in a binding's target must produce different
        // effect keys — retargeting changes which params the codegen treats as
        // binding-exposed, producing different WGSL.
        let base = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(&PresetTypeId::new("ColorGrade"))
            .expect("ColorGrade canonical view");

        // Clone and modify a binding's target
        let mut def_with_different_target = (*base.canonical_def).clone();
        if let Some(meta) = def_with_different_target.preset_metadata.as_mut()
            && let Some(binding) = meta.bindings.first_mut() {
                // Change target to point to a different param — this MUST change the key
                if let manifold_core::effect_graph_def::BindingTarget::Node { param, .. } = &mut binding.target {
                    *param = "different_param".to_string();
                }
            }

        assert_ne!(
            effect_def_content_key(&base.canonical_def),
            effect_def_content_key(&def_with_different_target),
            "binding target changes must affect effect content key"
        );
    }

    #[test]
    fn generator_key_still_distinguishes_scale_offset() {
        // Generator key must distinguish scale/offset changes because the generator
        // runtime reads these from the cached fused def (registry.rs:253-268).
        let base = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(&PresetTypeId::new("Plasma"))
            .expect("Plasma canonical view");

        let mut def_with_scale = (*base.canonical_def).clone();
        if let Some(meta) = def_with_scale.preset_metadata.as_mut()
            && let Some(binding) = meta.bindings.first_mut() {
                binding.scale = 2.0;
            }

        assert_ne!(
            def_content_key(&base.canonical_def),
            def_content_key(&def_with_scale),
            "generator key must still distinguish scale changes"
        );
    }

    #[test]
    fn effect_binding_metadata_produces_byte_identical_wgsl() {
        // Two effect defs differing only in binding metadata must produce
        // byte-identical fused WGSL. This proves that key equality isn't
        // circular — the actual codegen output is the same, not just the hash.
        let base = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(&PresetTypeId::new("ColorGrade"))
            .expect("ColorGrade canonical view");

        // Clone and modify binding metadata
        let mut def_with_metadata = (*base.canonical_def).clone();
        if let Some(meta) = def_with_metadata.preset_metadata.as_mut()
            && let Some(binding) = meta.bindings.first_mut() {
                binding.label = "DIFFERENT_LABEL".to_string();
                binding.default_value = 999.0;
                binding.scale = 2.0;
                binding.offset = 1.0;
            }

        // Fuse both defs and extract WGSL
        let registry = PrimitiveRegistry::with_builtin();
        let fused_a = fuse_canonical_def(&base.canonical_def, &registry)
            .expect("canonical ColorGrade must fuse");
        let fused_b = fuse_canonical_def(&def_with_metadata, &registry)
            .expect("metadata-modified ColorGrade must fuse");

        // Extract WGSL from fused nodes
        let wgsl_a: String = fused_a.def.nodes.iter()
            .filter(|n| n.type_id == "node.wgsl_compute")
            .filter_map(|n| n.wgsl_source.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        let wgsl_b: String = fused_b.def.nodes.iter()
            .filter(|n| n.type_id == "node.wgsl_compute")
            .filter_map(|n| n.wgsl_source.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            wgsl_a, wgsl_b,
            "binding metadata changes must produce byte-identical fused WGSL"
        );
    }
}
