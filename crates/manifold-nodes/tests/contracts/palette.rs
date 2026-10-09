mod tests {
use manifold_node_engine::palette::*;
use manifold_core::PresetTypeId;

    #[test]
    fn hidden_whitewater_atoms_still_register() {
        let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
        let atoms = palette_atoms();
        for type_id in [
            "node.surface_crossings",
            "node.nearest_crossing",
            "node.crossing_distance",
            "node.liquid_cells",
            "node.lattice_curvature",
            "node.extend_lattice",
            "node.turbulence_field",
            "node.whitewater_influence",
            "node.dust_potential",
            "node.jitter_particles",
            "node.sample_faces_at_particles",
            "node.whitewater_emitter_velocity",
            "node.energy_potential",
            "node.wavecrest_potential",
            "node.inside_turbulence_potential",
            "node.turbulence_emission_count",
            "node.spawn_whitewater",
            "node.whitewater_type",
            "node.advect_whitewater",
            "node.retype_whitewater",
            "node.age_whitewater",
            "node.preserve_foam",
            "node.keep_whitewater",
            "node.upwind_distance",
            "node.emission_count",
        ] {
            assert!(registry.contains(type_id), "{type_id} must remain registered");
            assert!(
                !atoms.iter().any(|atom| atom.type_id == type_id),
                "{type_id} must stay hidden from the palette",
            );
        }
    }

    #[test]
    fn palette_atoms_are_unique_and_grouped() {
        let atoms = palette_atoms();
        assert!(!atoms.is_empty());

        // Category groups appear in `PaletteCategory::ORDER`; entries
        // within each group are alphabetical by label.
        let mut last_cat_idx: Option<usize> = None;
        let mut last_label_in_cat: Option<&str> = None;
        for atom in &atoms {
            let cat_idx = PaletteCategory::ORDER
                .iter()
                .position(|&c| c == atom.category)
                .expect("category in ORDER");
            match last_cat_idx {
                None => {}
                Some(prev_idx) if prev_idx == cat_idx => {
                    let prev_label = last_label_in_cat.expect("had prior label in same cat");
                    assert!(
                        prev_label <= atom.label.as_str(),
                        "{:?} > {:?} within {:?}",
                        prev_label,
                        atom.label,
                        atom.category,
                    );
                }
                Some(prev_idx) => {
                    assert!(
                        prev_idx < cat_idx,
                        "categories must appear in ORDER, got {:?} after {:?}",
                        atom.category,
                        atoms[0].category,
                    );
                }
            }
            last_cat_idx = Some(cat_idx);
            last_label_in_cat = Some(atom.label.as_str());
        }

        let ids: std::collections::HashSet<_> = atoms.iter().map(|a| &a.type_id).collect();
        assert_eq!(ids.len(), atoms.len(), "duplicate type ids in palette");

        // The first driver slice ships Value + LFO + Math.
        let drivers: Vec<_> = atoms
            .iter()
            .filter(|a| a.category == PaletteCategory::Driver)
            .map(|a| a.label.as_str())
            .collect();
        // Sanity-check Driver section keeps growing as the catalog
        // gains scalar sources/operators. New entries should land in
        // alphabetical order; this assertion enumerates what's shipped
        // today so unintended drops show up.
        assert_eq!(
            drivers,
            &[
                "Atmosphere",
                "Beat Gate",
                "Beat Ramp",
                "Camera Switch",
                "Canvas Area Scale",
                "Clip Trigger Cycle",
                "Color Sample",
                "Compose Vec3",
                "Compressor Envelope",
                "Connect Nearest",
                "Cycle Table Row",
                "Envelope Beats",
                "Envelope Decay",
                "Envelope Follower (A/R)",
                "Filter Detections",
                "Free Camera",
                "Frequency Ratio",
                "Inject Burst",
                "LFO",
                "Light",
                "Look-At Camera",
                "Loop Camera",
                "Luminance",
                "Math",
                "One Euro Filter",
                "Orbit Camera",
                "Peak",
                "Render Mode",
                "Sample & Hold",
                "Scale + Offset (value)",
                "Scene Object",
                "Smoothing",
                "Sum Into Bins",
                "Texture Size",
                "Track Persist",
                "Track Regions",
                // `node.transform_3d` (P1, SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md):
                // this literal enumeration wasn't updated when the atom
                // landed — a pre-existing gap from that phase, not this one.
                "Transform 3D",
                "Transform Components",
                "Trigger Ease To",
                "Trigger Gate",
                "Value",
                // Sorts last: ASCII byte-order puts a lowercase leading
                // letter after every uppercase-leading label above.
                "glTF Animation Source",
                "glTF Morph Weights",
                "glTF Skeleton Pose",
            ],
        );
    }

    #[test]
    fn catalog_default_for_mirror_has_required_handles() {
        let def = catalog_graph_def_for(&PresetTypeId::MIRROR).expect("Mirror has catalog default");
        let handles: std::collections::HashSet<_> = def
            .nodes
            .iter()
            .filter_map(|n| n.handle.as_deref())
            .collect();
        assert!(handles.contains("source"));
        assert!(handles.contains("uv_transform"));
        assert!(handles.contains("mix"));
        assert!(handles.contains("final_output"));
    }
}
