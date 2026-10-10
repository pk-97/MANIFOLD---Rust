mod tests {
use manifold_node_engine::freeze::classify::FusionKind;
    /// Every registered (non-fixture) primitive is either fusable or names
    /// its `BoundaryReason` — the enforcement half of D4/D5
    /// (docs/GRAPH_TOOLING_DESIGN.md). `node.__*` fixtures only register
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
        assert!(atomic_atoms > 0, "no registered atom declares atomic outputs — the sweep is vacuous");
        assert!(
            violations.is_empty(),
            "atoms with atomic outputs must declare fusion_kind: Boundary:\n  {}",
            violations.join("\n  ")
        );
    }

    /// `docs/DEPTH_RELIGHT_DESIGN.md` D6(a): a `precision_critical` input
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

    /// The `RangeContract` declared-excuse pattern (`docs/PARAM_RANGE_CONTRACT_DESIGN.md`
    /// D5), transcribed verbatim from `every_boundary_atom_declares_its_reason`
    /// above: walks every registered primitive's params via
    /// `EffectNode::param_contract`, and asserts the set of
    /// `(type_id, param name, reason)` triples carrying a contract EXACTLY
    /// EQUALS this curated table. P1 ships this table EMPTY — no contract
    /// exists in production yet (D6: remove-by-default, no kernel proof, no
    /// contract) — so this test proves the mechanism, not any real boundary.
    /// `node.__`-prefixed test fixtures are excluded, same as the
    /// boundary-reason walk (their contracts are test scaffolding, not
    /// production facts this ledger tracks).
    #[test]
    fn every_range_contract_names_a_real_boundary() {
        use manifold_node_engine::persistence::PrimitiveRegistry;

        // Curated table: every `(type_id, param_id, reason)` any registered
        // primitive is allowed to declare a `RangeContract` for. Empty in
        // P1; seeded in P2 (PARAM_RANGE_CONTRACT_DESIGN.md section 2/D6) — each
        // entry names its kernel/shader evidence file:line so a contract
        // can't creep back onto a merely-conventional range.
        //
        // node.switch_texture (mux_texture.rs) — hand-`impl EffectNode`,
        // `param_contract` override:
        //   - selector: mux_texture.rs:197-200 `resolve_selector_index`
        //     rounds+clamps to [0, num_inputs); absolute index space is
        //     [0, MAX_INPUTS-1] (mux_texture.rs:45).
        //   - num_inputs: mux_texture.rs:148-149 `rebuild_ports` clamps
        //     n to [1, MAX_INPUTS] before slicing the static
        //     IN_PORT_NAMES table.
        // node.multi_blend (multi_blend.rs) — hand-`impl EffectNode`,
        // `param_contract` override:
        //   - num_inputs: multi_blend.rs:191 `reconfigure` clamps to
        //     [2, MAX_INPUTS] before `rebuild_ports` slices IN_PORT_NAMES.
        // node.connect_nearest (array_connect_nearest.rs) — `primitive!`
        // macro `param_contracts:` field:
        //   - max_edges: array_connect_nearest.rs `array_output_capacity`
        //     returns `Some(max_edges)` verbatim as the allocated `edges`
        //     array capacity — sizes a real allocation.
        //
        // section 2 VERIFY reads performed, all REJECTED (evidence in the P2
        // session report, not repeated here — no contract added):
        //   - connect_nearest.max_distance: only ever squared into a
        //     comparison threshold, no division, no degenerate collapse
        //     at 0 — stays a display hint.
        //   - render.window (node.draw_lines, render_lines.rs): consumed
        //     only via `window_edges = (segments*window).ceil().max(1)` —
        //     already div-by-zero-proof independent of `window`'s value.
        //   - content_window.width (node.edge_stretch, uv_strip_clamp_body.wgsl):
        //     `clamp(uv, lo, hi)` is well-defined even at width=0 (lo==hi
        //     collapses to a single valid coordinate, not undefined math).
        //   - split.amount (node.rgb_split, chromatic_displace_body.wgsl):
        //     the sampler clamps at the texture edge, not at a fixed
        //     ±32 — the actual dead-input point depends on velocity
        //     magnitude and canvas dims, not a fixed physical bound.
        const CURATED: &[(&str, &str, manifold_core::effects::RangeReason)] = &[
            (
                "node.switch_texture",
                "selector",
                manifold_core::effects::RangeReason::Index,
            ),
            (
                "node.switch_texture",
                "num_inputs",
                manifold_core::effects::RangeReason::Count,
            ),
            (
                "node.multi_blend",
                "num_inputs",
                manifold_core::effects::RangeReason::Count,
            ),
            (
                "node.connect_nearest",
                "max_edges",
                manifold_core::effects::RangeReason::Count,
            ),
        ];

        let registry = PrimitiveRegistry::with_builtin();
        let mut found: Vec<(String, String, manifold_core::effects::RangeReason)> = Vec::new();

        for type_id in registry.known_type_ids() {
            if type_id.starts_with("node.__") {
                continue;
            }
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("registry missing {type_id}"));
            for param in node.parameters() {
                if let Some(contract) = node.param_contract(&param.name) {
                    found.push((type_id.to_string(), param.name.to_string(), contract.reason));
                }
            }
        }

        let mut violations: Vec<String> = Vec::new();
        for (type_id, param_id, reason) in &found {
            if !CURATED
                .iter()
                .any(|(t, p, r)| t == type_id && p == param_id && r == reason)
            {
                violations.push(format!(
                    "{type_id}.{param_id}: declares RangeContract (reason {reason:?}) but is \
                     not in the curated RANGE_CONTRACT table — add it deliberately with the \
                     kernel/shader evidence, or remove the contract",
                ));
            }
        }
        for (type_id, param_id, reason) in CURATED {
            if !found
                .iter()
                .any(|(t, p, r)| t == *type_id && p == *param_id && r == reason)
            {
                violations.push(format!(
                    "{type_id}.{param_id}: listed in the curated RANGE_CONTRACT table \
                     (reason {reason:?}) but the registered primitive declares no such \
                     contract — either it was removed (drop the table entry) or the \
                     declaration was lost",
                ));
            }
        }

        assert!(
            violations.is_empty(),
            "range_contract declaration violations:\n  {}",
            violations.join("\n  "),
        );
    }

    /// Per-input read-semantics: dither tags BOTH its inputs `CoincidentTexel`
    /// (exact-texel, no sampler), while a plain color atom leaves `INPUT_ACCESS`
    /// empty (every input defaults to `Coincident`).
    #[test]
    fn input_access_tags_dither_texel_and_defaults_color_coincident() {
        use manifold_node_engine::freeze::classify::InputAccess;
        use manifold_node_engine::persistence::PrimitiveRegistry;
        let registry = PrimitiveRegistry::with_builtin();

        let dither = registry.construct("node.dither").expect("registry missing node.dither");
        assert_eq!(
            dither.input_access(),
            &[InputAccess::CoincidentTexel, InputAccess::CoincidentTexel],
            "dither's in + pattern are both exact-texel"
        );

        let gain = registry.construct("node.exposure").expect("registry missing node.exposure");
        assert!(
            gain.input_access().is_empty(),
            "a color atom leaves INPUT_ACCESS empty (= all Coincident by default)"
        );
        assert_eq!(InputAccess::default(), InputAccess::Coincident);
    }

    /// BUG-z3l6 fail-closed source scan: any fusable primitive whose `run()` reads
    /// `ctx.time` must declare either `derived_uniforms` (frame-derived uniforms)
    /// or `frame_time_inputs` (ports whose unwired fallback is the frame clock).
    /// Without the declaration, the fused kernel silently bakes the param default
    /// and the effect freezes in performance mode.
    #[test]
    fn every_fusable_time_reading_atom_declares_frame_time_or_derived() {
        use manifold_node_engine::persistence::PrimitiveRegistry;
        use std::fs::{read_dir, read_to_string};

        let registry = PrimitiveRegistry::with_builtin();
        let mut violations: Vec<String> = Vec::new();
        manifold_nodes::testkit::source_roots::verify_wgsl_roots().expect("WGSL crate inventory");
        let roots = manifold_nodes::testkit::source_roots::primitive_source_roots().expect("primitive source roots");
        for entry in roots.iter().flat_map(|dir| read_dir(dir).expect("read primitives dir")) {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|s| s.to_str()) != Some("rs") {
                continue;
            }
            let source = read_to_string(&path).expect("read source");
            let Some(type_id) = extract_primitive_type_id(&source) else {
                continue;
            };
            if type_id.starts_with("node.__") {
                continue;
            }
            let Some(run_start) = source.find("fn run(&mut self") else {
                continue;
            };
            let run_end = source[run_start..]
                .find("#[cfg(test)]")
                .map(|i| run_start + i)
                .unwrap_or(source.len());
            let run_body = &source[run_start..run_end];
            if !run_body.contains("ctx.time") {
                continue;
            }
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("registry missing {type_id}"));
            if !node.fusion_kind().is_fusable() {
                continue;
            }
            if node.derived_uniforms().is_empty() && node.frame_time_inputs().is_empty() {
                violations.push(format!(
                    "{type_id} ({}) reads ctx.time in run(), is fusable, \
                     but declares neither derived_uniforms nor frame_time_inputs",
                    path.display()
                ));
            }
        }

        assert!(
            violations.is_empty(),
            "frame-time fusion contract violations:\n  {}",
            violations.join("\n  ")
        );
    }

    fn extract_primitive_type_id(source: &str) -> Option<&str> {
        let idx = source.find("type_id:")? + "type_id:".len();
        let rest = source[idx..].trim_start();
        if !rest.starts_with('"') {
            return None;
        }
        let rest = &rest[1..];
        let end = rest.find('"')?;
        Some(&rest[..end])
    }
}
