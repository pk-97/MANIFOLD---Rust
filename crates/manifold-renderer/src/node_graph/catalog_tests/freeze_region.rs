use crate::node_graph::freeze::region::*;

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphWire};

use crate::node_graph::PrimitiveRegistry;
use crate::node_graph::ports::PortType;

fn registry() -> PrimitiveRegistry { PrimitiveRegistry::with_builtin() }


    fn colorgrade_def() -> EffectGraphDef {
        let json = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/effect-presets/ColorGrade.json"
        ))
        .expect("read ColorGrade.json");
        serde_json::from_str(&json).expect("parse ColorGrade.json")
    }

    fn strange_attractor_def() -> EffectGraphDef {
        let json = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/generator-presets/StrangeAttractor.json"
        ))
        .expect("read StrangeAttractor.json");
        serde_json::from_str(&json).expect("parse StrangeAttractor.json")
    }

    /// BUG-007: `cycle_contains_array` must construct nodes CONFIGURED. A
    /// full-kernel `node.wgsl_compute` particle node (StrangeAttractor's sim
    /// stage) declares its `var<storage, read_write> array<Particle>` output only
    /// after its `wgsl_source` is parsed. A bare construct sees the default kernel
    /// with no Array output, so the particle stage is invisible to the SCC scan and
    /// a texture atom on the same feedback loop wrongly passes cut rule 12 and
    /// fuses tier-A f16 in-loop (where the bit-exact induction fails across a
    /// scatter — FluidSim divergence class).
    #[test]
    fn cycle_through_configured_particle_wgsl_compute_is_particle_loop() {
        let reg = registry();
        let def =
            manifold_core::flatten::flatten_groups(&strange_attractor_def()).expect("flatten");
        let sim = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.wgsl_compute")
            .expect("StrangeAttractor ships a full-kernel particle wgsl_compute node")
            .clone();

        // Root-cause pin: the Array output exists only on the CONFIGURED construct.
        let bare = reg.construct(&sim.type_id).expect("construct node.wgsl_compute");
        assert!(
            !bare.outputs().iter().any(|o| matches!(o.ty, PortType::Array(_))),
            "bare construct sees the default kernel — no Array output (the blind spot)",
        );
        let configured = configured_construct(&reg, &sim).expect("configured construct");
        assert!(
            configured
                .outputs()
                .iter()
                .any(|o| matches!(o.ty, PortType::Array(_))),
            "configured construct reports the particle Array output",
        );

        // Minimal feedback cycle: texture atom (id 101) ↔ particle node (id 100).
        // Only wires + node type_ids matter to `cycle_contains_array`.
        let mut sim = sim;
        sim.id = 100;
        let mut tex = sim.clone();
        tex.id = 101;
        tex.type_id = "node.channel_mixer".to_string();
        tex.wgsl_source = None;
        let def2 = EffectGraphDef {
            version: def.version,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: def.scene_modifiers.clone(),
            nodes: vec![tex, sim],
            wires: vec![
                EffectGraphWire {
                    from_node: 101,
                    from_port: "out".into(),
                    to_node: 100,
                    to_port: "in".into(),
                },
                EffectGraphWire {
                    from_node: 100,
                    from_port: "out".into(),
                    to_node: 101,
                    to_port: "in".into(),
                },
            ],
        };
        assert!(
            cycle_contains_array(101, &def2, &reg),
            "a loop through a configured particle wgsl_compute node is a particle \
             loop — cut rule 12 must fire (BUG-007)",
        );
    }

    /// The whole ColorGrade card is one region: all 7 atoms, one external (the
    /// source, read once even though both gain and mix.a read it), output = the
    /// clamp that feeds final_output. This is the existing whole-card case now
    /// expressed as the general partition's single-component result.
    #[test]
    fn colorgrade_is_one_region() {
        let regions = partition_regions(&colorgrade_def(), &registry());
        assert_eq!(regions.len(), 1, "ColorGrade is a single region");
        let r = &regions[0];
        assert_eq!(r.members.len(), 7, "all 7 color atoms");
        assert_eq!(r.externals.len(), 1, "source read once (gain + mix.a share it)");
        // The clamp (the atom feeding final_output) is the region output.
        let out_node = colorgrade_def()
            .nodes
            .iter()
            .find(|n| n.type_id == "node.clamp")
            .map(|n| n.id)
            .unwrap();
        assert_eq!(r.outputs, vec![(out_node, "out".to_string())], "clamp is the region output");
        // mix reads the external fork AND colorize's register: an External + a
        // Member input, proving the fork resolves.
        let mix_id = colorgrade_def()
            .nodes
            .iter()
            .find(|n| n.type_id == "node.mix")
            .map(|n| n.id)
            .unwrap();
        let mix = r.members.iter().find(|m| m.doc_id == mix_id).unwrap();
        assert!(
            mix.inputs.iter().any(|i| matches!(i, RegionInput::External(0)))
                && mix.inputs.iter().any(|i| matches!(i, RegionInput::Member(_))),
            "mix threads the source fork (External) + colorize (Member)"
        );
    }

    /// P3 wave 2 (2026-07-14): `node.shininess`/`node.rim_light`/
    /// `node.matcap_two_tone` (OilyFluid), `node.brightness` (MetallicGlass)
    /// and `node.channel_mixer` (StarField) all converted onto the freeze
    /// codegen path that wave — `fusion_kind() == Pointwise` with a real
    /// `wgsl_body` — but every one of them carries a Color/Vec3/Vec4 param.
    /// Before P5 `classify_node`'s
    /// scalar-only cut rule rejected all five, so none was ever a region
    /// member. P5 lifts Vec3/Vec4/Color (three/four namespaced uniform
    /// fields, same mechanism as the standalone codegen's "P3 wave 2"
    /// reassembly) — this test now pins the OPPOSITE, equally real finding:
    /// every one of these five atoms, in the three bundled presets that ship
    /// them, now DOES join a region. `graph_tool fusion` confirms the same
    /// per-node verdict interactively.
    #[test]
    fn wave2_color_param_atoms_now_fuse_in_shipped_presets() {
        let registry = registry();
        let cases: &[(&str, &[&str])] = &[
            ("OilyFluid", &["node.shininess", "node.rim_light", "node.matcap_two_tone"],
            ),
            ("MetallicGlass", &["node.brightness"]),
            ("StarField", &["node.channel_mixer"]),
        ];
        for (preset_name, type_ids) in cases {
            let type_id = manifold_core::PresetTypeId::new(preset_name);
            let json = crate::node_graph::bundled_presets::bundled_preset_json(&type_id)
                .unwrap_or_else(|| panic!("{preset_name}: no bundled JSON"));
            let def: EffectGraphDef = serde_json::from_str(&json).expect("preset parses");
            let def = manifold_core::flatten::flatten_groups(&def).expect("flattens");

            let fused_doc_ids: std::collections::HashSet<u32> = partition_regions(&def, &registry)
                .iter()
                .flat_map(|r| r.members.iter().map(|m| m.doc_id))
                .collect();

            for &type_id_str in *type_ids {
                let hits: Vec<u32> = def
                    .nodes
                    .iter()
                    .filter(|n| n.type_id == type_id_str)
                    .map(|n| n.id)
                    .collect();
                assert!(!hits.is_empty(), "{preset_name}: expected {type_id_str} to appear");
                for doc_id in hits {
                    assert!(
                        fused_doc_ids.contains(&doc_id),
                        "{preset_name}: {type_id_str} (doc_id={doc_id}) should now join a region \
                         — P5 lifted its Color/Vec3/Vec4 param"
                    );
                }
            }
        }
    }
