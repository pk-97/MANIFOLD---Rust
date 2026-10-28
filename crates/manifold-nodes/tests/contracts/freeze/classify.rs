mod tests {
    use manifold_node_engine::freeze::classify::FusionKind;
    /// Every registered (non-fixture) primitive is either fusable or names
    /// its `BoundaryReason` — the enforcement half of the boundary-reason rule.
    /// `node.__*` fixtures only register
    /// under `cfg(test)` and are excluded the same way
    /// `catalog_gen::is_test_fixture` and
    /// `primitives::mod::every_conventional_array_port_declares_a_channels_signature`
    /// already carve them out. Every primitive must satisfy
    /// `is_fusable() XOR boundary_reason().is_some()` — there is no
    /// undeclared middle.
    #[test]
    fn every_boundary_atom_declares_its_reason() {
        use manifold_node_engine::freeze::classify::{BoundaryReason, CONVERSION_DEBT_LEDGER};
        use manifold_node_engine::persistence::PrimitiveRegistry;

        let registry = PrimitiveRegistry::with_builtin();
        let mut violations: Vec<String> = Vec::new();
        let mut conversion_debt_holders: Vec<&str> = Vec::new();

        for type_id in registry.known_type_ids() {
            if type_id.starts_with("node.__") {
                continue;
            }
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("registry missing {type_id}"));

            let fusable = node.fusion_kind().is_fusable();
            let reason = node.boundary_reason();

            if reason == Some(BoundaryReason::ConversionDebt) {
                conversion_debt_holders.push(type_id);
            }

            if fusable == reason.is_some() {
                violations.push(format!(
                    "{type_id}: fusable={fusable}, boundary_reason={reason:?} — \
                     every primitive must be fusable XOR declare a BoundaryReason \
                     (fusable atoms must NOT also declare a reason; Boundary atoms \
                     MUST declare exactly one)",
                ));
            }
        }

        for &ledger_id in CONVERSION_DEBT_LEDGER {
            if !conversion_debt_holders.contains(&ledger_id) {
                violations.push(format!(
                    "{ledger_id}: listed in CONVERSION_DEBT_LEDGER but the registered \
                     primitive no longer declares BoundaryReason::ConversionDebt — either \
                     it was converted (remove it from the ledger) or the declaration was lost",
                ));
            }
        }
        for &holder in &conversion_debt_holders {
            if !CONVERSION_DEBT_LEDGER.contains(&holder) {
                violations.push(format!(
                    "{holder}: declares BoundaryReason::ConversionDebt but is not in \
                     CONVERSION_DEBT_LEDGER — add it deliberately or use a different reason",
                ));
            }
        }

        assert!(
            violations.is_empty(),
            "boundary_reason declaration violations:\n  {}",
            violations.join("\n  "),
        );
    }

    /// Every atom with an atomic output — sole output (scatter) or a side
    /// output next to a coincident one — declares `FusionKind::Boundary`. The
    /// accumulator only holds its value once the whole dispatch has run, so
    /// it is always a region cut; a fusable declaration would be a promise
    /// `classify_buffer_node` silently breaks (docs/FREEZE_COMPILER_MAP.md
    /// section 4, "The cut rules").
    #[test]
    fn atomic_output_atoms_are_boundaries() {
        use manifold_node_engine::persistence::PrimitiveRegistry;

        let registry = PrimitiveRegistry::with_builtin();
        let mut atomic_atoms = 0usize;
        let mut violations: Vec<String> = Vec::new();
        for type_id in registry.known_type_ids() {
            if type_id.starts_with("node.__") {
                continue;
            }
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("registry missing {type_id}"));
            if node.atomic_outputs().is_empty() {
                continue;
            }
            atomic_atoms += 1;
            if node.fusion_kind() != FusionKind::Boundary {
                violations.push(format!(
                    "{type_id}: atomic outputs {:?} but fusion_kind {:?}",
                    node.atomic_outputs(),
                    node.fusion_kind()
                ));
            }
        }
        assert!(
            atomic_atoms > 0,
            "no registered atom declares atomic outputs — the sweep is vacuous"
        );
        assert!(
            violations.is_empty(),
            "atoms with atomic outputs must declare fusion_kind: Boundary:\n  {}",
            violations.join("\n  ")
        );
    }

    /// A `precision_critical` input
    /// declares that its producer benefits from an `Rgba32Float` intermediate
    /// — but that promotion is only safe if THIS atom itself reads the input
    /// via an exact `textureLoad` (`InputAccess::is_texel_exact`), never a
    /// filtering sampler. Marking a `Coincident`/`Gather` input critical
    /// would be self-defeating: the format-selection seam would hand this
    /// very atom a non-filterable texture its own `textureSampleLevel` read
    /// can't correctly serve on Apple GPUs. Walks every registered primitive
    /// and asserts every name in `precision_critical_inputs()` both (a)
    /// resolves to a real texture input and (b) is texel-exact.
    #[test]
    fn precision_critical_inputs_are_texel_exact() {
        use manifold_node_engine::freeze::classify::input_access_of;
        use manifold_node_engine::persistence::PrimitiveRegistry;

        let registry = PrimitiveRegistry::with_builtin();
        let mut violations: Vec<String> = Vec::new();

        for type_id in registry.known_type_ids() {
            if type_id.starts_with("node.__") {
                continue;
            }
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("registry missing {type_id}"));

            for &name in node.precision_critical_inputs() {
                match input_access_of(node.as_ref(), name) {
                    None => violations.push(format!(
                        "{type_id}: precision_critical names \"{name}\", which is not a \
                         declared texture input on this node",
                    )),
                    Some(access) if !access.is_texel_exact() => violations.push(format!(
                        "{type_id}.{name}: precision_critical requires a texel-exact \
                         InputAccess (CoincidentTexel/GatherTexel); this input is \
                         {access:?} (a filtering sampler read) — Rgba32Float is \
                         non-filterable on Apple GPUs, so this atom's own read would break",
                    )),
                    Some(_) => {}
                }
            }
        }

        assert!(
            violations.is_empty(),
            "precision_critical / InputAccess mismatches:\n  {}",
            violations.join("\n  "),
        );
    }
}
