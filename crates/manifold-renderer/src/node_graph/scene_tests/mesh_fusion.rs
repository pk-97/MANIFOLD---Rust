use crate::node_graph::freeze::install::*;
use crate::node_graph::mesh_change::*;

    /// P2 (BUG-e3p6.4, design §3.3) — the fused node's mesh-rule sidecar
    /// composed from member declarations, proven at the composition seam for
    /// the Surface Waves core: a gathered-external `normal_wave_mesh` feeding
    /// a coincident `morph_mesh`. This shape cannot reach the composition
    /// through `fuse_canonical_def_masked` today — morph's `weights_len`
    /// derived uniform has no registered recompute, so the fail-closed gate
    /// keeps the region unfused — so the seam is exercised directly.
    /// Composition semantics under test: an external leaf renames to
    /// `src_<slot>`; an internal dependency recurses into the producing
    /// member's declaration; `Written` dominates its aspect; the
    /// sorted/deduped leaf list collapses both mesh wires onto the single
    /// vertices external.
    #[test]
    fn mesh_change_compose_region_rules_wave_morph() {
        use crate::node_graph::freeze::classify::InputAccess;
        use crate::node_graph::freeze::region::{
            ExternalRef, Region, RegionInput, RegionMember,
        };
        use crate::node_graph::mesh_change::{MeshAspect, MeshDependency};
        use crate::node_graph::primitives::{MorphMesh, NormalWaveMesh};
        use std::borrow::Cow;

        // Buffer region (space None): scalar ports are not region ports, so
        // wave threads only its gathered mesh `in`; morph threads `in`
        // (external), `b` (wave's register), `weights` (external).
        let region = Region {
            members: vec![
                RegionMember {
                    doc_id: 1,
                    inputs: vec![RegionInput::External(0)],
                    input_access: vec![InputAccess::BufferGather],
                    quantize_f16: false,
                },
                RegionMember {
                    doc_id: 2,
                    inputs: vec![
                        RegionInput::External(0),
                        RegionInput::Member(1),
                        RegionInput::External(1),
                    ],
                    input_access: vec![InputAccess::Coincident; 3],
                    quantize_f16: false,
                },
            ],
            externals: vec![
                ExternalRef { from_node: 0, from_port: "vertices".to_string() },
                ExternalRef { from_node: 0, from_port: "weights".to_string() },
            ],
            outputs: vec![(2, "out".to_string())],
            space: None,
            sampled_externals: vec![],
            virtual_chains: vec![],
            output_capacity: None,
        };
        let keepalive: Vec<Box<dyn crate::node_graph::effect_node::EffectNode>> = vec![
            Box::new(NormalWaveMesh::new()),
            Box::new(MorphMesh::new()),
        ];
        let all_members: Vec<&RegionMember> = region.members.iter().collect();
        let rules = compose_region_mesh_rules(&region, &all_members, &keepalive, None);
        assert_eq!(rules.len(), 1, "exactly the region's mesh output earns a rule");
        let rule = &rules[0];
        assert_eq!(rule.output, "dst", "single-output region emits dst, got {:?}", rule);
        // morph declares topology = Dependencies([in.Topology, b.Topology]).
        // `in` is external slot 0 → src_0.Topology. `b` is wave's register;
        // wave declares Dependencies([in.Topology]) and wave's `in` is the
        // SAME external slot 0 → src_0.Topology again. Sort + dedup → one leaf.
        assert_eq!(
            rule.topology,
            PreparedMeshRevisionRule::Dependencies(vec![MeshDependency {
                input: Cow::Owned("src_0".to_string()),
                aspect: MeshAspect::Topology,
            }]),
            "composed topology must rename + recurse + dedup to the single vertices external"
        );
        assert!(
            matches!(rule.positions, PreparedMeshRevisionRule::Written),
            "morph positions are Written and Written dominates, got {:?}",
            rule.positions
        );
    }
