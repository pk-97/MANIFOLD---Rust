//! The freeze compiler's marker ABI — single-sourced (FUSION_SOTA_DESIGN D1).
//!
//! Codegen (`freeze::codegen`, `freeze::install`) and the runtime host
//! primitive (`primitives::wgsl_compute`) communicate through WGSL **comment
//! markers** — a stringly-typed contract with the wire format as the
//! cross-session pipeline-cache key (the WGSL text itself). Before this
//! module, each end hand-formatted/hand-matched the marker strings
//! independently (`format!`/`push_str` on the emit side, `strip_prefix` on
//! the parse side) — two texts of the same grammar that could drift. This
//! module makes `Marker::emit`/`Marker::parse` the ONLY implementations of
//! that grammar; every producer and consumer goes through it.
//!
//! Precedent: [`crate::node_graph::freeze::classify::fusion_kind_str`] — one
//! rendering shared by `catalog_gen` and `graph_tool` for the same reason
//! (two ends of one string contract must never disagree).
//!
//! Full inventory + semantics: `docs/FREEZE_COMPILER_MAP.md` section 5. Two markers
//! here (`Pure`, `Fusion`) are hand-authored only — no codegen emit site
//! exists for them (BlackHole's bake, user `@fusion:` fragments) — but they
//! still round-trip through this grammar so a review can diff one place.
//! `ResetGated` has a documented meaning but, like `Pure`/`Fusion`, is
//! written by hand today; nothing in `freeze::codegen`/`freeze::install`
//! emits it yet (seed-pattern kernels are hand-authored).
//!
//! Out of scope: `@channel_skip`, `@in:`, `@param:` — these are a different,
//! unrelated micro-grammar (Channels-struct field skipping; user fragment
//! port/param declarations), not part of the marker ABI table in
//! `FREEZE_COMPILER_MAP.md` section 5, and not touched by this module.

use crate::node_graph::freeze::classify::CapacityExpr;

/// One marker on the freeze compiler's WGSL comment-based wire.
///
/// `emit` and `parse` are inverses for every variant (`marker_roundtrip_every_variant`
/// below). `parse` accepts either a full source line (containing `//`) or a bare
/// comment body — it looks for the first `//` itself, matching the historical
/// `split_line_comment` convention every call site used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Marker {
    /// `// @fused_output` — own line, precedes a `var<storage, read_write>`
    /// array global that is a FRESH output-only buffer (fused buffer codegen).
    FusedOutput,
    /// `// @dispatch_count_param: <field>` — the named uniform field carries
    /// the kernel's live element count (fused buffer codegen, in-place loops).
    DispatchCountParam { field: String },
    /// `// @sampler_address_mode: <mode>` — `mode` is `"repeat"` or `"mirror"`
    /// (fused texture/buffer codegen's gather sampler). `"clamp"` emits no
    /// marker at all (byte-identical default), so this variant is never
    /// constructed for the clamp case.
    SamplerAddressMode { mode: String },
    /// `// @reset_gated` — own line. Hand-authored on seed-pattern kernels;
    /// the node exposes a synthetic optional `reset_trigger` input.
    ResetGated,
    /// `// @pure` — own line. Hand-authored assertion (BlackHole bake) that a
    /// kernel's output depends only on params + wired inputs.
    Pure,
    /// `// @fusion: <kind>` — `kind` is `"pointwise"` or `"source"`.
    /// Hand-authored on a user `node.wgsl_compute` fragment (`fn body(...)`).
    Fusion { kind: String },
    /// `// @camera_external: <name>` — fused texture/buffer codegen, one per
    /// distinct wired `Camera` external the region routes (`name` is
    /// `camera_ext_N`).
    CameraExternal { name: String },
    /// `// @derived_uniform_member: <first_field> words=<n> <type_id> [<camera_port>] [<member_port>=<fused_port> …]`
    /// — fused texture/buffer codegen, one per region member with non-empty
    /// `derived_uniforms()`. Trailing `<member_port>=<fused_port>` pairs map
    /// the array input ports the member's recompute consults
    /// (`derived_uniform_registry::array_ports`) to the names they carry
    /// inside the fused kernel: `src_<e>` for a wired external, or the
    /// sentinel `count` for a region-internal register (its length IS the
    /// kernel's element count). Absent ports (unwired optionals) map to
    /// nothing — the recompute's lookup degrades to 0, matching `run()`.
    /// Only ports the recompute declares are emitted, so members without
    /// array-derived uniforms keep the pre-extension marker text
    /// byte-identical.
    DerivedUniformMember {
        first_field: String,
        words: u32,
        type_id: String,
        camera_port: Option<String>,
        array_ports: Vec<(String, String)>,
    },
    /// `// @fused_output_capacity: <expr>` — fused buffer codegen (BUG-orm4),
    /// own line right after the fresh `// @fused_output dst` binding, only
    /// when a region member declared `FusedOutputCapacity::MultipleOf` (the
    /// region's count is widened past the min-over-inputs default).
    /// `<expr>` is the composed capacity expression over the array input
    /// slots in the `min(…)`/`mul(<u32>,…)`/`s<slot>` grammar
    /// (`CapacityExpr::to_marker_payload`). `node.wgsl_compute` evaluates it
    /// over the wired input capacities to size `dst` exactly to the kernel's
    /// dispatch count. Identity regions emit no marker (byte-identical WGSL;
    /// the min-over-inputs default is then exactly right).
    FusedOutputCapacity { expr: CapacityExpr },
    /// `// @input_access: <port> <token>` — fused texture codegen (install),
    /// one per `src_<e>` texture input, recording how the region's members
    /// actually read it (`coincident` / `coincident_texel` / `gather` /
    /// `gather_texel`). Without it `node.wgsl_compute` reports the default
    /// (filtering) access and the executor's mixed-consumer rule wrongly
    /// vetoes fp32 promotion of a shared upstream intermediate (P7/D8: the
    /// relight height field read fp16 by the fused kernel while GTAO asked
    /// for fp32).
    InputAccess { port: String, token: String },
    /// `// @precision_critical: <port>` — fused texture codegen (install), one
    /// per `src_<e>` consumed by a member port the atom declares
    /// `precision_critical` (D6(a)). Lets the fused kernel keep requesting the
    /// fp32 upstream intermediate its members would have requested unfused.
    PrecisionCritical { port: String },
}

impl Marker {
    /// Render this marker's comment text — no trailing newline, no `//`
    /// leading whitespace beyond the literal `// ` prefix. Callers append
    /// `\n` (own-line markers) or interpolate trailing after code on the
    /// same line (`@sampler_address_mode`), matching each call site's
    /// pre-existing placement so emission stays byte-identical.
    pub fn emit(&self) -> String {
        match self {
            Marker::FusedOutput => "// @fused_output".to_string(),
            Marker::DispatchCountParam { field } => {
                format!("// @dispatch_count_param: {field}")
            }
            Marker::SamplerAddressMode { mode } => {
                format!("// @sampler_address_mode: {mode}")
            }
            Marker::ResetGated => "// @reset_gated".to_string(),
            Marker::Pure => "// @pure".to_string(),
            Marker::Fusion { kind } => format!("// @fusion: {kind}"),
            Marker::CameraExternal { name } => format!("// @camera_external: {name}"),
            Marker::DerivedUniformMember { first_field, words, type_id, camera_port, array_ports } => {
                let mut s = match camera_port {
                    Some(cp) => {
                        format!("// @derived_uniform_member: {first_field} words={words} {type_id} {cp}")
                    }
                    None => {
                        format!("// @derived_uniform_member: {first_field} words={words} {type_id}")
                    }
                };
                for (member, fused) in array_ports {
                    s.push_str(&format!(" {member}={fused}"));
                }
                s
            }
            Marker::InputAccess { port, token } => format!("// @input_access: {port} {token}"),
            Marker::PrecisionCritical { port } => format!("// @precision_critical: {port}"),
            Marker::FusedOutputCapacity { expr } => {
                format!("// @fused_output_capacity: {}", expr.to_marker_payload())
            }
        }
    }

    /// Parse one marker off a source line. `line` may be a whole source line
    /// (only the text after the first `//` is considered — matching the
    /// pre-existing `split_line_comment` convention) or a bare comment body.
    /// Returns `None` when the line carries no recognized marker, or a
    /// recognized prefix with a malformed/empty payload (e.g.
    /// `@dispatch_count_param:` with no field name, `@fusion:` with an unknown kind).
    /// Callers scan `source.lines()` (after `strip_block_comments`) and call
    /// this once per line.
    pub fn parse(line: &str) -> Option<Marker> {
        let comment = match line.find("//") {
            Some(idx) => &line[idx + 2..],
            None => line,
        };
        let c = comment.trim();

        if c == "@fused_output" {
            return Some(Marker::FusedOutput);
        }
        if c == "@reset_gated" {
            return Some(Marker::ResetGated);
        }
        if c == "@pure" {
            return Some(Marker::Pure);
        }
        if let Some(rest) = c.strip_prefix("@dispatch_count_param:") {
            let name = rest.trim();
            return (!name.is_empty())
                .then(|| Marker::DispatchCountParam { field: name.to_string() });
        }
        if let Some(rest) = c.strip_prefix("@sampler_address_mode:") {
            let mode = rest.trim();
            return (!mode.is_empty())
                .then(|| Marker::SamplerAddressMode { mode: mode.to_string() });
        }
        if let Some(rest) = c.strip_prefix("@fusion:") {
            let kind = rest.trim();
            return match kind {
                "pointwise" | "source" => Some(Marker::Fusion { kind: kind.to_string() }),
                _ => None,
            };
        }
        if let Some(rest) = c.strip_prefix("@camera_external:") {
            let name = rest.trim();
            return (!name.is_empty()).then(|| Marker::CameraExternal { name: name.to_string() });
        }
        if let Some(rest) = c.strip_prefix("@input_access:") {
            let mut parts = rest.split_whitespace();
            let port = parts.next()?;
            let token = parts.next()?;
            return matches!(token, "coincident" | "coincident_texel" | "gather" | "gather_texel")
                .then(|| Marker::InputAccess { port: port.to_string(), token: token.to_string() });
        }
        if let Some(rest) = c.strip_prefix("@precision_critical:") {
            let name = rest.trim();
            return (!name.is_empty())
                .then(|| Marker::PrecisionCritical { port: name.to_string() });
        }
        if let Some(rest) = c.strip_prefix("@fused_output_capacity:") {
            let expr = CapacityExpr::parse_marker_payload(rest)?;
            return Some(Marker::FusedOutputCapacity { expr });
        }
        if let Some(rest) = c.strip_prefix("@derived_uniform_member:") {
            let mut parts = rest.split_whitespace();
            let first_field = parts.next()?;
            let words_tok = parts.next()?;
            let words_str = words_tok.strip_prefix("words=")?;
            let words: u32 = words_str.parse().ok()?;
            let type_id = parts.next()?;
            // Everything after the type_id: at most one BARE token (the
            // camera port, pre-extension grammar) plus any number of
            // `<member_port>=<fused_port>` pairs. A token containing '='
            // is a pair; a bare token is the camera port.
            let mut camera_port: Option<String> = None;
            let mut array_ports: Vec<(String, String)> = Vec::new();
            for tok in parts {
                if let Some((member, fused)) = tok.split_once('=') {
                    if member.is_empty() || fused.is_empty() {
                        return None;
                    }
                    array_ports.push((member.to_string(), fused.to_string()));
                } else {
                    camera_port = Some(tok.to_string());
                }
            }
            return Some(Marker::DerivedUniformMember {
                first_field: first_field.to_string(),
                words,
                type_id: type_id.to_string(),
                camera_port,
                array_ports,
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant round-trips: `parse(&m.emit()) == Some(m)`. One
    /// representative instance per variant (invariant table, FUSION_SOTA_DESIGN section 3).
    #[test]
    fn marker_roundtrip_every_variant() {
        let variants = vec![
            Marker::FusedOutput,
            Marker::DispatchCountParam { field: "n0_active_count".to_string() },
            Marker::FusedOutputCapacity {
                expr: CapacityExpr::Min(vec![
                    CapacityExpr::Mul(2, Box::new(CapacityExpr::Slot(0))),
                    CapacityExpr::Slot(1),
                ]),
            },
            Marker::SamplerAddressMode { mode: "repeat".to_string() },
            Marker::SamplerAddressMode { mode: "mirror".to_string() },
            Marker::ResetGated,
            Marker::Pure,
            Marker::Fusion { kind: "pointwise".to_string() },
            Marker::Fusion { kind: "source".to_string() },
            Marker::CameraExternal { name: "camera_ext_0".to_string() },
            Marker::DerivedUniformMember {
                first_field: "n0_dt_scaled".to_string(),
                words: 1,
                type_id: "euler_step_particles".to_string(),
                camera_port: None,
                array_ports: Vec::new(),
            },
            Marker::DerivedUniformMember {
                first_field: "n0_cam_fwd_x".to_string(),
                words: 3,
                type_id: "flatten_to_camera_plane".to_string(),
                camera_port: Some("camera_ext_0".to_string()),
                array_ports: Vec::new(),
            },
            Marker::DerivedUniformMember {
                first_field: "n1_weights_len".to_string(),
                words: 1,
                type_id: "node.morph_mesh".to_string(),
                camera_port: None,
                array_ports: vec![
                    ("weights".to_string(), "count".to_string()),
                    ("bias".to_string(), "src_2".to_string()),
                ],
            },
            Marker::DerivedUniformMember {
                first_field: "n0_cam_fwd_x".to_string(),
                words: 3,
                type_id: "flatten_to_camera_plane".to_string(),
                camera_port: Some("camera_ext_0".to_string()),
                array_ports: vec![("points".to_string(), "src_0".to_string())],
            },
        ];
        for m in variants {
            let emitted = m.emit();
            assert_eq!(
                Marker::parse(&emitted),
                Some(m.clone()),
                "round-trip failed for {m:?} (emitted {emitted:?})"
            );
        }
    }

    /// A trailing-comment placement (e.g. `@sampler_address_mode` on the same
    /// line as the `var samp: sampler;` declaration) parses identically to an
    /// own-line marker — `parse` finds the first `//` itself.
    #[test]
    fn parse_accepts_trailing_comment_placement() {
        let line = "@group(0) @binding(3) var samp: sampler; // @sampler_address_mode: repeat";
        assert_eq!(
            Marker::parse(line),
            Some(Marker::SamplerAddressMode { mode: "repeat".to_string() })
        );
    }

    #[test]
    fn parse_rejects_empty_payload() {
        assert_eq!(Marker::parse("// @dispatch_count_param:"), None);
        assert_eq!(Marker::parse("// @camera_external:"), None);
    }

    #[test]
    fn parse_rejects_unknown_fusion_kind() {
        assert_eq!(Marker::parse("// @fusion: unknown"), None);
    }

    #[test]
    fn parse_rejects_non_marker_lines() {
        assert_eq!(Marker::parse("fn body(c: vec4<f32>) -> vec4<f32> {"), None);
        assert_eq!(Marker::parse("// just a comment"), None);
    }

    #[test]
    fn parse_derived_uniform_member_without_camera_port() {
        assert_eq!(
            Marker::parse("// @derived_uniform_member: n0_dt_scaled words=1 euler_step_particles"),
            Some(Marker::DerivedUniformMember {
                first_field: "n0_dt_scaled".to_string(),
                words: 1,
                type_id: "euler_step_particles".to_string(),
                camera_port: None,
                array_ports: Vec::new(),
            })
        );
    }

    #[test]
    fn parse_derived_uniform_member_with_array_ports() {
        // Member→fused-port pairs round-trip, alone and after a camera port;
        // a malformed pair (empty side) is rejected.
        assert_eq!(
            Marker::parse(
                "// @derived_uniform_member: n1_weights_len words=1 node.morph_mesh weights=count"
            ),
            Some(Marker::DerivedUniformMember {
                first_field: "n1_weights_len".to_string(),
                words: 1,
                type_id: "node.morph_mesh".to_string(),
                camera_port: None,
                array_ports: vec![("weights".to_string(), "count".to_string())],
            })
        );
        assert_eq!(
            Marker::parse(
                "// @derived_uniform_member: n0_cam_fwd_x words=3 flatten_to_camera_plane camera_ext_0 points=src_0"
            ),
            Some(Marker::DerivedUniformMember {
                first_field: "n0_cam_fwd_x".to_string(),
                words: 3,
                type_id: "flatten_to_camera_plane".to_string(),
                camera_port: Some("camera_ext_0".to_string()),
                array_ports: vec![("points".to_string(), "src_0".to_string())],
            })
        );
        assert_eq!(
            Marker::parse("// @derived_uniform_member: n1_weights_len words=1 node.morph_mesh =src_0"),
            None
        );
    }

    #[test]
    fn parse_derived_uniform_member_with_camera_port() {
        assert_eq!(
            Marker::parse(
                "// @derived_uniform_member: n0_cam_fwd_x words=3 flatten_to_camera_plane camera_ext_0"
            ),
            Some(Marker::DerivedUniformMember {
                first_field: "n0_cam_fwd_x".to_string(),
                words: 3,
                type_id: "flatten_to_camera_plane".to_string(),
                camera_port: Some("camera_ext_0".to_string()),
                array_ports: Vec::new(),
            })
        );
    }

    /// Recursively collect every `.rs` file under `dir` (std::fs only — no
    /// `walkdir`, no shelling out to `rg`/`find`; same convention as
    /// `manifold-core/tests/docs_index_sync.rs`).
    fn rust_files_under(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                rust_files_under(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// Invariant (FUSION_SOTA_DESIGN D1 / section 3): every marker byte on the wire is
    /// produced/consumed by THIS module. A Rust string literal starting `"// @`
    /// anywhere else in `manifold-renderer/src` is a hand-formatted/hand-matched
    /// marker that has drifted out of the single-sourced grammar — the exact
    /// failure mode D1 closes. Doc-comment prose referencing a marker in
    /// backticks (e.g. `` `// @reset_gated` ``) does NOT match this pattern (no
    /// leading `"`), so this only catches real string-literal duplication.
    #[test]
    fn marker_literals_live_in_one_module() {
        // CARGO_MANIFEST_DIR = <repo>/crates/manifold-renderer
        let src_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_files_under(&src_dir, &mut files);

        let mut violations = Vec::new();
        for path in &files {
            // Only this file (`node_graph/freeze/markers.rs`) may contain the
            // literal wire-format prefix.
            if path.to_string_lossy().replace('\\', "/").ends_with("node_graph/freeze/markers.rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else { continue };
            for (i, line) in text.lines().enumerate() {
                if line.contains("\"// @") {
                    violations.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "marker string literals found outside freeze/markers.rs — route through \
             Marker::emit()/Marker::parse() instead:\n{}",
            violations.join("\n")
        );
    }

    /// Deterministic dump of every fused `node.wgsl_compute` kernel's WGSL text,
    /// across every bundled effect + generator preset — sorted so re-runs are
    /// byte-stable. This is the P1 hard gate's raw material: the marker refactor
    /// (D1) must change zero emitted bytes, and the WGSL text is the
    /// cross-session pipeline-cache key, so "zero bytes changed" is checked at
    /// the text level, not "compiles" or "renders the same".
    fn capture_all_fused_wgsl() -> String {
        use crate::node_graph::PrimitiveRegistry;
        use crate::node_graph::freeze::install::{fuse_canonical_def, fuse_generator_view};
        use manifold_core::effect_graph_def::EffectGraphDef;
        use manifold_core::preset_def::PresetKind;

        let registry = PrimitiveRegistry::with_builtin();
        let mut out = String::new();

        let mut effect_ids: Vec<_> =
            crate::node_graph::bundled_presets::bundled_preset_type_ids(PresetKind::Effect)
                .collect();
        effect_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for type_id in effect_ids {
            let Some(view) = crate::node_graph::loaded_preset_view_by_id(&type_id) else {
                continue;
            };
            let Some(fused) = fuse_canonical_def(&view.canonical_def, &registry) else {
                continue;
            };
            let mut nodes: Vec<_> =
                fused.def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").collect();
            nodes.sort_by_key(|n| n.id);
            for node in nodes {
                if let Some(wgsl) = &node.wgsl_source {
                    out.push_str(&format!("=== effect:{} node:{} ===\n", type_id.as_str(), node.id));
                    out.push_str(wgsl);
                    out.push('\n');
                }
            }
        }

        let mut gen_ids: Vec<_> =
            crate::node_graph::bundled_presets::bundled_preset_type_ids(PresetKind::Generator)
                .collect();
        gen_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for type_id in gen_ids {
            let Some(json) = crate::node_graph::bundled_presets::bundled_preset_json(&type_id)
            else {
                continue;
            };
            let Ok(def) = serde_json::from_str::<EffectGraphDef>(&json) else { continue };
            let Some(fused_view) = fuse_generator_view(&def, &registry) else { continue };
            let mut nodes: Vec<_> =
                fused_view.def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").collect();
            nodes.sort_by_key(|n| n.id);
            for node in nodes {
                if let Some(wgsl) = &node.wgsl_source {
                    out.push_str(&format!(
                        "=== generator:{} node:{} ===\n",
                        type_id.as_str(),
                        node.id
                    ));
                    out.push_str(wgsl);
                    out.push('\n');
                }
            }
        }
        out
    }

    fn golden_path() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fused_wgsl_snapshot.txt")
    }

    /// P1 hard gate (FUSION_SOTA_DESIGN D1): the marker refactor must emit
    /// byte-identical WGSL for every bundled preset. The golden fixture was
    /// captured from origin/main HEAD (6888ea28, pre-refactor codegen) by
    /// temporarily stashing only `freeze/codegen.rs` + `freeze/install.rs` (the
    /// emit sites), running this test with `UPDATE_FUSION_GOLDEN=1`, then
    /// restoring the refactor and re-running normally. Regenerate the fixture
    /// (`UPDATE_FUSION_GOLDEN=1 cargo test …`) only for an INTENTIONAL codegen
    /// change — never to make this phase's refactor pass.
    #[test]
    fn fused_wgsl_snapshot_unchanged() {
        let actual = capture_all_fused_wgsl();
        let path = golden_path();
        if std::env::var("UPDATE_FUSION_GOLDEN").is_ok() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &actual).unwrap();
            return;
        }
        let golden = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "missing golden fixture at {path:?} — run with UPDATE_FUSION_GOLDEN=1 to create it"
            )
        });
        assert_eq!(
            actual, golden,
            "fused WGSL text changed for at least one bundled preset — the marker \
             refactor (D1) must emit byte-identical output (the WGSL text is the \
             pipeline-cache key). If this change is intentional (a different phase's \
             codegen work), regenerate with UPDATE_FUSION_GOLDEN=1."
        );
    }
}
