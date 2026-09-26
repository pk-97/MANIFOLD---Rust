    use super::*;
    use crate::node_graph::ports::ArrayType;
    use crate::node_graph::transform::Transform;

    #[test]
    fn current_frame_rt_update_precedes_flags_depth_and_trace() {
        let source = include_str!("../render_scene.rs");
        let evaluate = source.split_once("    fn evaluate<'ctx, 'gpu>").unwrap().1;
        let update = evaluate.find("self.rt_accel_maintenance(").unwrap();
        let success_reset = evaluate[update..]
            .find("pre.reset_decision |= changed;")
            .map(|offset| offset + update)
            .unwrap();
        let rejection_return = evaluate[update..]
            .find("FrameRenderStatus::Failed(failure)")
            .and_then(|offset| evaluate[update + offset..].find("return;").map(|end| update + offset + end))
            .unwrap();

        for marker in [
            "self.author_rt_flags(",
            "self.ensure_gpu_resources(",
            "self.raster_shadow_prepasses(",
            "self.opaque_depth_snapshot_pass(",
            "self.rt_trace_accumulate(",
        ] {
            let consumer = evaluate.find(marker).unwrap();
            assert!(consumer > success_reset, "consumer precedes successful RT update: {marker}");
            assert!(consumer > rejection_return, "consumer follows an RT rejection: {marker}");
        }
    }

    #[test]
    fn current_frame_path_has_no_settle_or_latched_accel_policy() {
        let source = include_str!("../render_scene.rs");
        let evaluate = source.split_once("    fn evaluate<'ctx, 'gpu>").unwrap().1;
        for obsolete in [
            "rt_deferred_build_decision",
            "rt_trace_gate",
            "rt_refit_eligible",
            "reject_topology",
            "rt_accel_built",
            "rt_topology_rejected",
            "content_settle",
            "settled",
        ] {
            assert!(!evaluate.contains(obsolete), "obsolete RT policy remains in evaluate: {obsolete}");
        }
        assert_eq!(evaluate.matches("self.rt_accel_maintenance(").count(), 1);
        assert_eq!(evaluate.matches("pre.reset_decision |= changed;").count(), 1);
    }

    /// VOLUMETRIC_LIGHT_DESIGN.md V1: the CPU half of "off = zero cost".
    /// `shaft_intensity == 0` (unwired default) must gate `wants_shafts`
    /// false, and a fresh `RenderScene` must never have called any
    /// `ensure_` function for the shaft-inscatter slot — it stays `None`.
    /// P1 has no march kernel yet, so nothing CAN call an `ensure_` fn; this
    /// test pins that the gate and the untouched slot both exist and agree.
    #[test]
    fn wants_shafts_gate() {
        assert!(
            !wants_shafts(&Atmosphere::default()),
            "shaft_intensity 0 (default) must not want shafts"
        );
        let hot = Atmosphere { shaft_intensity: 1.0, ..Atmosphere::default() };
        assert!(wants_shafts(&hot), "shaft_intensity > 0 must want shafts");
        let still_off = Atmosphere { shaft_intensity: 0.0, ..Atmosphere::default() };
        assert!(!wants_shafts(&still_off));

        let scene = RenderScene::new();
        assert!(
            scene.shaft_inscatter.is_none(),
            "off -> no ensure_ call -> the shaft slot stays None"
        );
    }

    /// RAYTRACING_DESIGN.md section 16 TL5: the designated-sun slot pick —
    /// first Sun-mode caster under the RT caster cap, None otherwise.
    /// Tested against the production fn (never a duplicated copy — a copy
    /// drifts silently).
    fn svt_test_light(mode: crate::node_graph::light::LightMode) -> crate::node_graph::light::Light {
        crate::node_graph::light::Light {
            mode,
            falloff: crate::node_graph::light::LightFalloff::Legacy,
            pos: [0.0, 0.0, 0.0],
            aim: [0.0, 0.0, 1.0],
            dir: [0.0, 0.0, 1.0],
            color: [1.0, 1.0, 1.0, 1.0],
            range: 30.0,
            inner_cone_angle: 0.0,
            outer_cone_angle: 0.0,
            cast_shadows: true,
            shadow_softness: crate::node_graph::light::ShadowSoftness::Soft,
            shadow_bias: 0.005,
            shadow_resolution: 1024,
        }
    }

    #[test]
    fn rt_svt_slot_sun_first_is_zero() {
        use crate::node_graph::light::LightMode;
        let casters = [svt_test_light(LightMode::Sun), svt_test_light(LightMode::Point)];
        assert_eq!(rt_svt_slot(&casters), Some(0));
    }

    #[test]
    fn rt_svt_slot_point_only_is_none() {
        use crate::node_graph::light::LightMode;
        let casters = [svt_test_light(LightMode::Point), svt_test_light(LightMode::Point)];
        assert_eq!(rt_svt_slot(&casters), None);
    }

    #[test]
    fn rt_svt_slot_sun_past_cap_is_none() {
        use crate::node_graph::light::LightMode;
        let mut casters: Vec<_> = (0..manifold_gpu::raytrace::MAX_RT_CASTERS)
            .map(|_| svt_test_light(LightMode::Point))
            .collect();
        casters.push(svt_test_light(LightMode::Sun));
        assert_eq!(rt_svt_slot(&casters), None);
    }

    /// RAYTRACING_DESIGN.md RT-D3: `mat4_inverse` feeds the RT shadow-ray
    /// pass's world-position reconstruction — proven, not eyeballed
    /// (CLAUDE.md oracle discipline: "computable question -> write the
    /// three-line script"). Two checks against a REAL camera's
    /// `view_proj` (not an arbitrary matrix): (1) `inv * view_proj ==
    /// identity` to tight tolerance; (2) round-tripping a known world
    /// point through `view_proj` -> NDC -> `mat4_inverse` -> back to
    /// world recovers the original point — the exact operation the RT
    /// kernel performs per-pixel.
    #[test]
    #[allow(clippy::needless_range_loop)] // matrix row/col indices, clearer explicit than enumerate()
    fn mat4_inverse_recovers_identity_for_a_real_camera() {
        let cam = Camera {
            pos: [1.5, 2.0, -3.0],
            ..Camera::default_perspective()
        };
        let vp = cam.view_proj(16.0 / 9.0);
        let inv = mat4_inverse(vp).expect("a real camera's view_proj must be invertible");

        // (1) inv * vp == identity (column-major mat4 multiply) — also the
        // ground-truth check on `mat4_mul` itself (identity is an oracle
        // independent of the helper, not a mirror of it).
        let product = mat4_mul(inv, vp);
        for c in 0..4 {
            for r in 0..4 {
                let expected = if c == r { 1.0 } else { 0.0 };
                assert!(
                    (product[c][r] - expected).abs() < 1e-4,
                    "inv*vp[{c}][{r}] = {}, expected {expected}",
                    product[c][r]
                );
            }
        }

        // (2) world -> clip -> NDC -> (via inv) -> world round-trip, the
        // RT kernel's exact `world_pos_from_depth` operation.
        let world = [0.4, -0.6, 1.2, 1.0f32];
        let mut clip = [0f32; 4];
        for r in 0..4 {
            let mut sum = 0.0;
            for c in 0..4 {
                sum += vp[c][r] * world[c];
            }
            clip[r] = sum;
        }
        let ndc = [clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3], 1.0];
        let mut back = [0f32; 4];
        for r in 0..4 {
            let mut sum = 0.0;
            for c in 0..4 {
                sum += inv[c][r] * ndc[c];
            }
            back[r] = sum;
        }
        for i in 0..3 {
            assert!(
                (back[i] / back[3] - world[i]).abs() < 1e-3,
                "round-tripped world[{i}] = {}, expected {}",
                back[i] / back[3],
                world[i]
            );
        }
    }

    /// RT-T2-C: `mat4_mul(a, b)` must apply `b` FIRST — the operand order
    /// `prev_model * inverse(model)` depends on (a swapped order is still
    /// a valid matrix, so only a non-commuting fixture catches it). Two
    /// distinct translations don't commute with a scale between them, so
    /// scale-then-translate vs translate-then-scale disagree measurably.
    #[test]
    fn mat4_mul_applies_the_right_operand_first() {
        // Column-major: `m[col][row]`, translation lives in column 3.
        let translate = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [1.0, 2.0, 3.0, 1.0f32],
        ];
        let scale = [
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 2.0, 0.0, 0.0],
            [0.0, 0.0, 2.0, 0.0],
            [0.0, 0.0, 0.0, 1.0f32],
        ];
        let p = [1.0, 1.0, 1.0, 1.0f32];
        let apply = |m: [[f32; 4]; 4], v: [f32; 4]| {
            let mut out = [0.0f32; 3];
            for (row, slot) in out.iter_mut().enumerate() {
                *slot = m[0][row] * v[0] + m[1][row] * v[1] + m[2][row] * v[2] + m[3][row] * v[3];
            }
            out
        };

        // scale * translate: translate first, then scale => 2*(1+t)
        assert_eq!(apply(mat4_mul(scale, translate), p), [4.0, 6.0, 8.0]);
        // translate * scale: scale first, then translate => 2*1 + t
        assert_eq!(apply(mat4_mul(translate, scale), p), [3.0, 4.0, 5.0]);
    }

    /// IMPORT_FIDELITY_DESIGN.md D2/F-P1 negative gate: the old flat lod-0
    /// envmap sample + `ibl_strength = 1.0 - roughness*0.7` heuristic is
    /// gone, not paralleled — split-sum (prefiltered chain × BRDF LUT +
    /// cosine irradiance) is the only IBL path left in `fs_pbr`. Plain
    /// source-text check (no GPU needed) rather than an `rg` shell-out, so
    /// it runs in the default nextest sweep.
    #[test]
    fn ibl_strength_heuristic_is_deleted() {
        let src = include_str!("../shaders/render_scene.wgsl");
        assert!(
            !src.contains("ibl_strength"),
            "ibl_strength heuristic must be fully deleted, not left dead/commented"
        );
        assert!(
            src.contains("prefiltered_specular") && src.contains("irradiance_map") && src.contains("brdf_lut"),
            "fs_pbr must consume the split-sum IBL bindings"
        );
    }

    /// BUG-wfxe: the scene kernel must naga-parse — a WGSL syntax break
    /// here is otherwise only caught by GPU pipeline creation. Also pins
    /// the tangent seam: the Vertex struct carries it, the vertex shader
    //  exports it, and no stray 48-byte assumption survives in the source.
    #[test]
    fn render_scene_wgsl_parses_and_carries_tangent() {
        let src = include_str!("../shaders/render_scene.wgsl");
        let module = naga::front::wgsl::parse_str(src).expect("render_scene.wgsl must naga-parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = validator.validate(&module) {
            panic!("render_scene.wgsl validation failed:\n{}", e.emit_to_string(src));
        }
        assert!(
            src.contains("tangent: vec4<f32>") && src.contains("fn tbn_for"),
            "the authored-tangent path (BUG-wfxe) must be present in the kernel"
        );
    }

    /// IMPORT_FIDELITY_DESIGN.md D3/F-P2 negative gate: `texture_flags2`
    /// bits must be read ONLY inside their dedicated resolve functions
    /// (`resolve_mr`/`resolve_occlusion`/`resolve_emissive`, plus
    /// GLTF_MATERIAL_EXTENSIONS_DESIGN.md E4's `resolve_iridescence` —
    /// `texture_flags2.w` is iridescence_map's presence flag, same reuse
    /// doctrine as the render_scene.rs E3/E4/E5 comment), never ad-hoc
    /// from a fragment entry point directly — the shape D3 explicitly
    /// requires (a dedicated resolve function per map, not a shared
    /// channel-select branch scattered across call sites). Plain
    /// source-text scan (no GPU needed), scoped to the actual field-access
    /// syntax `u.texture_flags2` so prose mentions of the name in comments
    /// don't count as "reads".
    #[test]
    fn texture_flags2_is_read_only_inside_its_dedicated_resolve_functions() {
        let src = include_str!("../shaders/render_scene.wgsl");

        fn function_body<'a>(src: &'a str, sig: &str) -> &'a str {
            let start = src.find(sig).unwrap_or_else(|| panic!("missing `{sig}`"));
            let body_start = src[start..].find('{').expect("fn body") + start;
            let mut depth = 0i32;
            let mut end = body_start;
            for (i, c) in src[body_start..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = body_start + i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            &src[start..end]
        }

        let access = "u.texture_flags2";
        let total = src.matches(access).count();
        assert!(total > 0, "expected at least one texture_flags2 read (sanity check on the scan itself)");

        let inside: usize = [
            "fn resolve_mr(",
            "fn resolve_occlusion(",
            "fn resolve_emissive(",
            "fn resolve_iridescence(",
        ]
            .iter()
            .map(|sig| function_body(src, sig).matches(access).count())
            .sum();

        assert_eq!(
            total, inside,
            "u.texture_flags2 must be read ONLY inside resolve_mr/resolve_occlusion/resolve_emissive/resolve_iridescence \
             (IMPORT_FIDELITY_DESIGN.md D3/F-P2 negative gate) — found a read elsewhere"
        );
    }

    /// VOLUMETRIC_LIGHT_DESIGN.md P3 deliverable 4: `shaft_quality`'s
    /// 0/1/2 (Low/Med/High) enum must decode to D2's committed 16/24/32
    /// step counts, and the march's uniform build (`evaluate`, ~line 1837)
    /// feeds `shaft_step_count(atmosphere.shaft_quality)` straight into
    /// `misc.x` — this pins the CPU-side half of "quality actually drives
    /// step count" (already wired since P2; P3 confirms it, per the phase
    /// brief's "confirm P2 actually reads shaft_quality" instruction).
    #[test]
    fn shaft_step_count_matches_design() {
        assert_eq!(shaft_step_count(0), 16, "Low");
        assert_eq!(shaft_step_count(1), 24, "Med (default)");
        assert_eq!(shaft_step_count(2), 32, "High");
        // Any value past 2 clamps to High (the enum can only ever carry
        // 0..=2 in practice; this just keeps the decode total).
        assert_eq!(shaft_step_count(3), 32);
    }

    /// "Hard" must mean an exactly-zero cone, or the RT sun shadow stays a
    /// stochastic estimate the user asked it not to be — `cone_sample`
    /// short-circuits only at `<= 0.0`, and the mask has no temporal
    /// history to average the jitter away. This mapping used to be one
    /// hard-coded 0.02 for every sun, which is what made alpha-masked
    /// petals hatch and flicker with softness set to Hard.
    #[test]
    fn sun_cone_half_angle_honours_the_lights_softness() {
        use crate::node_graph::light::ShadowSoftness;
        assert_eq!(sun_cone_half_angle(ShadowSoftness::Hard), 0.0);
        assert_eq!(
            sun_cone_half_angle(ShadowSoftness::Soft),
            SUN_CONE_SOFT_RADIANS
        );
        assert_eq!(
            sun_cone_half_angle(ShadowSoftness::VerySoft),
            SUN_CONE_VERY_SOFT_RADIANS
        );
        // Contact hardening is what an RT cone does for free, so Contact
        // rides the Soft cone and ignores the raster blocker-search size.
        assert_eq!(
            sun_cone_half_angle(ShadowSoftness::Contact { light_size: 4.0 }),
            SUN_CONE_SOFT_RADIANS
        );
        const { assert!(SUN_CONE_VERY_SOFT_RADIANS > SUN_CONE_SOFT_RADIANS) };
    }

    /// D3's committed upsample-tap weight, in isolation: `exp(-(Δz/z_full)^2
    /// * 400)`, `Δz = |z_full - z_tap|`.
    fn bilateral_weight(z_full: f32, z_tap: f32) -> f32 {
        let dz = (z_full - z_tap) / z_full;
        (-(dz * dz) * 400.0).exp()
    }

    /// VOLUMETRIC_LIGHT_DESIGN.md V5: the committed upsample weights must
    /// not bleed the far side of a depth silhouette into the near side by
    /// more than 1%. Synthetic 2x2 half-res neighbourhood: three taps share
    /// the near-side depth (matching the full-res center pixel exactly,
    /// `Δz=0` → weight 1), the fourth sits on the far side of a modest 10%
    /// depth step. All four bilinear weights equal (0.25, the 2x2-block
    /// center) — the committed contract this test proves: the depth term
    /// alone crushes the far tap's contribution to under 1% of the
    /// renormalized sum, even for a step this small (a bigger step crushes
    /// it further, per the formula's `exp(-x^2*400)` shape).
    #[test]
    fn upsample_weight_does_not_bleed_across_a_depth_silhouette() {
        let z_near = 10.0f32;
        let z_far = 11.0f32; // 10% step
        let bilinear_w = 0.25f32; // 2x2-block center, all four taps equal

        let w_near = bilateral_weight(z_near, z_near); // Δz=0 -> weight 1
        let w_far = bilateral_weight(z_near, z_far);

        // Three near-side taps (color 1.0), one far-side tap (color 1.0) —
        // same color on both sides isolates the WEIGHT's contribution from
        // any color difference: the far tap's fractional contribution to
        // the final weighted sum is exactly its weight fraction.
        let weighted_far = bilinear_w * w_far;
        let weighted_near = 3.0 * bilinear_w * w_near;
        let weight_sum = weighted_near + weighted_far;
        let far_fraction = weighted_far / weight_sum;

        assert!(
            far_fraction < 0.01,
            "far-side tap must contribute <1% of the renormalized sum at a 10% depth step; \
             got {:.4}% (w_near={w_near:.6}, w_far={w_far:.6})",
            far_fraction * 100.0
        );
    }

    /// Same synthetic edge, but the fallback branch: when ALL taps are on
    /// the far side (so the near-side reference itself IS the "far" depth,
    /// i.e. no matching-depth tap exists at all) the committed weight sum
    /// can still collapse toward zero — D3's `weight_sum < 1e-4` fallback to
    /// plain bilinear exists exactly for this case, and the fallback must
    /// equal the plain (undepth-weighted) bilinear blend, not zero.
    #[test]
    fn upsample_weight_fallback_matches_plain_bilinear_when_all_taps_disagree() {
        // Every tap is equally far from `z_full` (an extreme case: a full-res
        // pixel whose own depth doesn't match ANY of its four half-res
        // neighbours) — weight collapses near zero for every tap.
        let z_full = 100.0f32;
        let z_tap = 5.0f32; // huge relative jump on all 4 taps
        let w = bilateral_weight(z_full, z_tap);
        assert!(w < 1e-4, "all four taps must collapse under the 1e-4 fallback threshold, got {w}");

        // The committed fallback (D3): plain bilinear, weights sum to 1 by
        // construction, so a 4-tap bilinear blend of equal-color taps must
        // reproduce that color exactly regardless of the (collapsed) depth
        // weights.
        let colors = [1.0f32, 1.0, 1.0, 1.0];
        let bilinear_ws = [0.09f32, 0.21, 0.21, 0.49]; // arbitrary, sums to 1
        assert!((bilinear_ws.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        let plain_bilinear: f32 = colors.iter().zip(&bilinear_ws).map(|(c, bw)| c * bw).sum();
        assert!((plain_bilinear - 1.0).abs() < 1e-6, "plain bilinear of identical-color taps must reproduce that color");
    }

    fn params_with(objects: f32, lights: f32) -> ParamValues {
        let mut p = ParamValues::default();
        p.insert(std::borrow::Cow::Borrowed("objects"), ParamValue::Float(objects));
        p.insert(std::borrow::Cow::Borrowed("lights"), ParamValue::Float(lights));
        p
    }

    #[test]
    fn instance_count_preserves_capacity_when_unwired() {
        assert_eq!(effective_instance_count(Some(4096), None), 4096);
        assert_eq!(effective_instance_count(None, None), 1);
    }

    #[test]
    fn instance_count_clamps_partial_and_above_capacity_values() {
        assert_eq!(effective_instance_count(Some(4096), Some(17.0)), 17);
        assert_eq!(effective_instance_count(Some(4096), Some(17.9)), 17);
        assert_eq!(effective_instance_count(Some(4096), Some(5000.0)), 4096);
        assert_eq!(effective_instance_count(Some(0), Some(2.0)), 0);
    }

    #[test]
    fn instance_count_invalid_values_are_safe_zero_draws() {
        for value in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                effective_instance_count(Some(4096), Some(value)),
                0,
                "value={value:?}"
            );
        }
    }

    #[test]
    fn zero_instance_draws_are_excluded_from_rt_and_shadow_tables() {
        let source = include_str!("../render_scene.rs");
        let rt = source
            .split_once("fn collect_rt_objects")
            .expect("RT object collection")
            .1;
        assert!(
            rt.contains("!d.routes_to_transparent() && d.instance_count > 0"),
            "zero-count objects must not enter RT geometry"
        );
        let evaluate = source
            .split_once("let opaque_draws: Vec<&ObjectDraw>")
            .expect("opaque draw collection")
            .1;
        assert!(
            evaluate.contains("!d.routes_to_transparent() && d.instance_count > 0"),
            "zero-count objects must not enter shadow/depth tables"
        );
    }

    #[test]
    fn defaults_to_two_objects_one_light() {
        let s = RenderScene::new();
        // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D4 (P2): camera + envmap +
        // atmosphere + render_mode + light_0 + object_0 + object_1 — ONE
        // `Object` port per object now, replacing the 21 legacy per-object
        // port families (mesh_n/material_n/17 maps/transform_n/instances_n).
        assert_eq!(s.inputs().len(), 4 + 1 + 2);
        assert!(s.inputs().iter().any(|p| p.name == "atmosphere"));
        assert!(!s.inputs().iter().find(|p| p.name == "atmosphere").unwrap().required);
        assert_eq!(
            s.inputs().iter().find(|p| p.name == "atmosphere").unwrap().ty,
            PortType::Atmosphere
        );
        // SCENE_RENDER_MODE_DESIGN.md D2: the optional render_mode input,
        // same shape as atmosphere (unwired = Rendered = byte-identical).
        assert!(s.inputs().iter().any(|p| p.name == "render_mode"));
        assert!(!s.inputs().iter().find(|p| p.name == "render_mode").unwrap().required);
        assert_eq!(
            s.inputs().iter().find(|p| p.name == "render_mode").unwrap().ty,
            PortType::RenderMode
        );
        let by_name = |n: &str| s.inputs().iter().find(|p| p.name == n).unwrap();
        assert!(!by_name("object_0").required);
        assert!(!by_name("object_1").required);
        assert_eq!(by_name("object_0").ty, PortType::Object);
        assert!(!s.inputs().iter().any(|p| p.name == "object_2"));
        assert!(s.inputs().iter().any(|p| p.name == "light_0"));
        assert!(!s.inputs().iter().any(|p| p.name == "light_1"));
        // `objects` + `lights` + `rt_enabled` (D14) + `temporal_upscale`
        // (section 5.2 P4) + `rt_reflections` (section 9 RD9) +
        // `rt_denoise_feed` (section 17.5 DN4) + `rt_shadows` +
        // `rt_ao` + `rt_gi` (RT term toggles) + `rt_firefly_clamp`
        // (RT-Stage-3 P1, BUG-mkgh) — per-object TRS moved to
        // `node.scene_object`'s `transform` input
        // (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md section 2 D3); the live
        // instance count is also an optional input on `node.scene_object`,
        // not a render_scene parameter. Neither toggle grows with object
        // count — this assertion is about object count, not the fixed
        // scene-level toggle set.
        assert_eq!(s.parameters().len(), 10);
        assert!(!s.parameters().iter().any(|p| p.name.contains("pos_x")));
    }

    #[test]
    fn reconfigure_grows_and_shrinks_ports_and_params() {
        let mut s = RenderScene::new();
        let node: &mut dyn EffectNode = &mut s;
        node.reconfigure(&params_with(5.0, 3.0));
        assert!(node.inputs().iter().any(|p| p.name == "object_4"));
        assert!(!node.inputs().iter().any(|p| p.name == "object_5"));
        assert!(node.inputs().iter().any(|p| p.name == "light_2"));
        assert!(!node.inputs().iter().any(|p| p.name == "light_3"));
        assert_eq!(node.parameters().len(), 10, "object count never grows the fixed scene-level toggle set");

        node.reconfigure(&params_with(1.0, 0.0));
        assert!(!node.inputs().iter().any(|p| p.name == "object_1"));
        assert!(!node.inputs().iter().any(|p| p.name == "light_0"));
        assert!(node.inputs().iter().any(|p| p.name == "object_0"));
    }

    #[test]
    fn reconfigure_clamps_objects_and_lights_to_their_slider_max() {
        let mut s = RenderScene::new();
        let node: &mut dyn EffectNode = &mut s;
        node.reconfigure(&params_with(9999.0, 999.0));
        // Objects clamp to OBJECT_SAFETY_MAX (D4: a real safety bound now,
        // not a UI convenience — a producer that would exceed it must error
        // at import time rather than rely on this silent clamp).
        let last_obj = OBJECT_SAFETY_MAX - 1;
        assert!(node.inputs().iter().any(|p| p.name == format!("object_{last_obj}")));
        assert!(!node
            .inputs()
            .iter()
            .any(|p| p.name == format!("object_{OBJECT_SAFETY_MAX}")));
        // Lights clamp to LIGHT_SLIDER_MAX — a soft UI bound now, NOT a
        // structural cap (lights ride a runtime-sized storage buffer).
        let last_light = LIGHT_SLIDER_MAX - 1;
        assert!(node.inputs().iter().any(|p| p.name == format!("light_{last_light}")));
        assert!(!node
            .inputs()
            .iter()
            .any(|p| p.name == format!("light_{LIGHT_SLIDER_MAX}")));
    }

    #[test]
    fn lights_generalize_well_past_the_old_cap_of_4() {
        // Prove 8 lights (twice the old
        // cap) wire cleanly.
        let mut s = RenderScene::new();
        let node: &mut dyn EffectNode = &mut s;
        node.reconfigure(&params_with(1.0, 8.0));
        assert!(node.inputs().iter().any(|p| p.name == "light_7"));
        assert!(!node.inputs().iter().any(|p| p.name == "light_8"));
    }

    #[test]
    fn objects_generalize_well_past_the_old_cap_of_8() {
        // Prove 32 objects wire cleanly.
        let mut s = RenderScene::new();
        let node: &mut dyn EffectNode = &mut s;
        node.reconfigure(&params_with(32.0, 2.0));
        assert!(node.inputs().iter().any(|p| p.name == "object_31"));
        // objects/lights + fixed scene-level toggles — object count never
        // grows the param list.
        assert_eq!(node.parameters().len(), 10);
    }

    #[test]
    fn camera_is_required_envmap_lights_and_objects_are_not() {
        let s = RenderScene::new();
        let by_name = |n: &str| s.inputs().iter().find(|p| p.name == n).unwrap();
        assert!(by_name("camera").required);
        assert!(!by_name("envmap").required);
        assert!(!by_name("light_0").required);
        // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D4: object_n is optional, not
        // required — an unwired object_n is a skip (no draw, no shadow),
        // not a render error. `node.scene_object`'s OWN `vertices`/
        // `material` inputs remain the structured-error path once an
        // object IS wired (see the draw-assembly loop's error branches).
        assert!(!by_name("object_0").required);
    }

    #[test]
    fn registers_with_palette_type_id() {
        let s = RenderScene::new();
        let node: &dyn EffectNode = &s;
        assert_eq!(node.type_id().as_str(), "node.render_scene");
    }

    #[test]
    fn model_matrix_identity_at_origin_no_rotation_unit_scale() {
        let m = model_matrix([0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let expected = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        for c in 0..4 {
            for r in 0..4 {
                assert!(
                    (m[c][r] - expected[c][r]).abs() < 1e-6,
                    "col {c} row {r}: got {} expected {}",
                    m[c][r],
                    expected[c][r]
                );
            }
        }
    }

    #[test]
    fn model_matrix_translates_a_point() {
        let m = model_matrix([2.0, 3.0, 4.0], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        // Column 3 (translation) should carry the position.
        assert_eq!(m[3], [2.0, 3.0, 4.0, 1.0]);
    }

    #[test]
    fn model_matrix_scales_columns_independently() {
        let m = model_matrix([0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [2.0, 3.0, 4.0]);
        assert!((m[0][0] - 2.0).abs() < 1e-6);
        assert!((m[1][1] - 3.0).abs() < 1e-6);
        assert!((m[2][2] - 4.0).abs() < 1e-6);
    }

    /// Asserts that a billboard transform makes local +Z point at the camera.
    fn assert_billboard_faces(object_pos: [f32; 3], camera_pos: [f32; 3]) {
        let t = Transform {
            pos: object_pos,
            billboard: true,
            ..Default::default()
        };
        let rot = t.billboard_rot_euler(camera_pos);
        let m = model_matrix(t.pos, rot, [1.0, 1.0, 1.0]);
        // Column 2 of the column-major model matrix is where local +Z maps.
        let actual = [m[2][0], m[2][1], m[2][2]];
        let dx = camera_pos[0] - object_pos[0];
        let dy = camera_pos[1] - object_pos[1];
        let dz = camera_pos[2] - object_pos[2];
        let len = (dx * dx + dy * dy + dz * dz).sqrt();
        let expected = [dx / len, dy / len, dz / len];
        for i in 0..3 {
            assert!(
                (actual[i] - expected[i]).abs() < 1e-5,
                "axis {i}: got {} expected {}",
                actual[i],
                expected[i]
            );
        }
    }

    #[test]
    fn billboard_faces_camera_directly_ahead() {
        assert_billboard_faces([0.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
    }

    #[test]
    fn billboard_faces_camera_behind() {
        assert_billboard_faces([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
    }

    #[test]
    fn billboard_faces_camera_to_the_right() {
        assert_billboard_faces([0.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
    }

    #[test]
    fn billboard_faces_camera_above() {
        assert_billboard_faces([0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
    }

    #[test]
    fn billboard_faces_camera_off_axis_with_offset_position() {
        assert_billboard_faces([1.0, -2.0, 3.0], [4.0, 2.0, -1.0]);
    }

    #[test]
    fn billboard_coincident_camera_falls_back_to_existing_rotation() {
        let t = Transform {
            rot_euler: [0.5, -0.25, 0.1],
            ..Default::default()
        };
        let rot = t.billboard_rot_euler(t.pos);
        assert_eq!(rot, t.rot_euler);
    }

    // ---- GBUFFER_DESIGN.md P1, I1 — lazy `depth` output ----
    //
    // Descriptor-level, no GPU device: `depth`'s step-output binding (and
    // therefore the R32Float resolve texture the backend would allocate for
    // it) only exists when the graph actually wires a consumer. Proven via
    // `compile()` alone — the SAME `consumed_outputs` mechanism that already
    // makes `render_mesh`'s `world_pos`/`world_normal` lazy (execution_plan.rs)
    // — rather than constructing a live `DepthMsaaPassDesc` (whose other
    // fields need real GPU textures the default test build can't create).

    /// Minimal stand-in producer: no inputs, one output of the given type.
    /// Just enough to satisfy `validate()`'s required-input check for
    /// `camera` — `compile()` never runs `evaluate()`, so this never needs
    /// to touch a GPU device.
    struct StubProducer {
        type_id: EffectNodeType,
        out: NodeOutput,
    }

    impl StubProducer {
        fn new(id: &'static str, ty: PortType) -> Self {
            Self {
                type_id: EffectNodeType::new(id),
                out: NodePort {
                    name: std::borrow::Cow::Borrowed("out"),
                    ty,
                    kind: PortKind::Output,
                    required: false,
                },
            }
        }
    }

    impl EffectNode for StubProducer {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &[]
        }
        fn outputs(&self) -> &[NodeOutput] {
            std::slice::from_ref(&self.out)
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
    }

    /// Build a one-object, zero-light `render_scene` fed by stub
    /// camera/mesh/material producers through a `node.scene_object` (the
    /// same shape a real graph wires post-P2), `color` always wired to a
    /// `FinalOutput`; `depth` wired to a second `FinalOutput` iff
    /// `wire_depth`. Returns the compiled plan plus the scene node's id so
    /// the caller can inspect its step's `outputs` list.
    fn compile_scene(wire_depth: bool) -> (crate::node_graph::ExecutionPlan, crate::node_graph::NodeInstanceId) {
        use crate::node_graph::primitives::scene_object::SceneObjectNode;
        use crate::node_graph::{FinalOutput, Graph, compile};

        let mut graph = Graph::new();
        let cam = graph.add_node(Box::new(StubProducer::new("stub.camera", PortType::Camera)));
        let mesh = graph.add_node(Box::new(StubProducer::new(
            "stub.mesh",
            PortType::Array(ArrayType::of_known::<MeshVertex>()),
        )));
        let mat = graph.add_node(Box::new(StubProducer::new("stub.material", PortType::Material)));
        let scene_object = graph.add_node(Box::new(SceneObjectNode::new()));

        // `Graph::add_node` (via `NodeInstance::new`) reconfigures the node
        // against its OWN default params right after insertion — a
        // pre-insertion `reconfigure` call would just get overwritten by
        // that. Set `objects`/`lights` through `Graph::set_param` instead,
        // which re-reconfigures against the new values (mirrors how every
        // real host — JSON load, editor slider — drives this node).
        let scene_id = graph.add_node(Box::new(RenderScene::new()));
        graph
            .set_param(scene_id, "objects", ParamValue::Float(1.0))
            .expect("objects param exists");
        graph
            .set_param(scene_id, "lights", ParamValue::Float(0.0))
            .expect("lights param exists");

        let color_sink = graph.add_node(Box::new(FinalOutput::new()));
        graph.connect((cam, "out"), (scene_id, "camera")).expect("camera wires");
        graph.connect((mesh, "out"), (scene_object, "vertices")).expect("mesh wires");
        graph.connect((mat, "out"), (scene_object, "material")).expect("material wires");
        graph
            .connect((scene_object, "object"), (scene_id, "object_0"))
            .expect("object wires");
        graph.connect((scene_id, "color"), (color_sink, "in")).expect("color wires");

        if wire_depth {
            let depth_sink = graph.add_node(Box::new(FinalOutput::new()));
            graph.connect((scene_id, "depth"), (depth_sink, "in")).expect("depth wires");
        }

        let plan = compile(&graph).expect("stub scene graph must compile");
        (plan, scene_id)
    }

    #[test]
    fn depth_output_unwired_gets_no_step_output_binding() {
        let (plan, scene_id) = compile_scene(false);
        let step = plan
            .steps()
            .iter()
            .find(|s| s.node == scene_id)
            .expect("render_scene step present");
        assert!(
            step.outputs.iter().any(|(name, _)| *name == "color"),
            "color must still get a binding"
        );
        assert!(
            !step.outputs.iter().any(|(name, _)| *name == "depth"),
            "I1: unwired depth must not get a step-output binding — the \
             backend would never allocate its R32Float resolve texture"
        );
    }

    #[test]
    fn depth_output_wired_gets_a_step_output_binding() {
        let (plan, scene_id) = compile_scene(true);
        let step = plan
            .steps()
            .iter()
            .find(|s| s.node == scene_id)
            .expect("render_scene step present");
        let depth_res = step
            .outputs
            .iter()
            .find(|(name, _)| *name == "depth")
            .map(|(_, res)| *res);
        assert!(depth_res.is_some(), "wired depth must get a step-output binding");
        assert_eq!(
            plan.resource_format(depth_res.unwrap()),
            Some(manifold_gpu::GpuTextureFormat::R32Float),
            "depth's resource_format must be R32Float at compile time"
        );
    }

    /// RAYTRACING_DESIGN.md section 10 addendum — gesture detection state
    /// machine. Two consecutive frames of key change arm a hold counter to 2;
    /// counter decrements each frame; gesture active while > 0.
    #[test]
    fn gesture_single_change_is_no_gesture() {
        // One frame change — not consecutive, no gesture.
        let (changed, gesture, _, counter) = gesture_detect(Some(0x100), 0x200, false, 0);
        assert!(changed, "key differs from stored");
        assert!(!gesture, "single change is not a gesture");
        assert_eq!(counter, 0, "counter stays 0 on first change");
    }

    #[test]
    fn gesture_two_consecutive_arms() {
        // Frame 1: changed, prev_changed=false.
        let (c1, g1, pc1, ct1) = gesture_detect(Some(0x100), 0x200, false, 0);
        assert!(c1, "frame 1 changed");
        assert!(!g1, "frame 1 is not a gesture yet");
        assert!(pc1, "prev_changed set true");
        assert_eq!(ct1, 0);
        // Frame 2: changed AND prev_changed=true → arm to 2.
        let (c2, g2, pc2, ct2) = gesture_detect(Some(0x200), 0x300, pc1, ct1);
        assert!(c2, "frame 2 changed");
        assert!(g2, "two consecutive changes = gesture");
        assert_eq!(ct2, 2, "counter armed to 2");
        assert!(pc2, "prev_changed still true");
    }

    #[test]
    fn gesture_hangover_survives_one_frame_gap() {
        // Simulate a mid-scrub throttled update: 2 changes, 1 frame pending
        // (key same for 1 frame), then changes resume.
        let (_, _, pc1, ct1) = gesture_detect(Some(0x100), 0x200, true, 2);
        assert_eq!(ct1, 2, "counter at 2 from prior gesture");
        // One frame gap (no change, counter decrements).
        let (c2, g2, pc2, ct2) = gesture_detect(Some(0x200), 0x200, pc1, ct1);
        assert!(!c2, "no change this frame");
        assert!(g2, "gesture still active with counter=1");
        assert_eq!(ct2, 1, "counter decremented from 2 to 1");
        assert!(!pc2);
        // Next frame: key changes again, gesture re-arms.
        let (c3, g3, _pc3, ct3) = gesture_detect(Some(0x200), 0x300, pc2, ct2);
        assert!(c3);
        assert!(g3);
        assert_eq!(ct3, 2, "re-armed on next change");
    }

    #[test]
    fn gesture_counter_ticks_down_to_zero() {
        // Armed to 2, then no further changes.
        let (c1, g1, pc1, ct1) = gesture_detect(Some(0x100), 0x200, true, 2);
        assert!(c1 && g1 && ct1 == 2);
        let (c2, g2, pc2, ct2) = gesture_detect(Some(0x200), 0x200, pc1, ct1);
        assert!(!c2 && g2 && ct2 == 1);
        let (c3, g3, pc3, ct3) = gesture_detect(Some(0x200), 0x200, pc2, ct2);
        assert!(!c3 && !g3 && ct3 == 0);
        assert_eq!((pc3, ct3), (false, 0));
    }

    #[test]
    fn gesture_static_never_arms() {
        for _ in 0..5 {
            let (c, g, _, ct) = gesture_detect(Some(0x100), 0x100, false, 0);
            assert!(!c && !g && ct == 0);
        }
    }

    #[test]
    fn gesture_first_frame_no_stored_key_never_changed() {
        // Before the first RT-ready frame, rt_lighting_key is None.
        let (changed, gesture, _, counter) = gesture_detect(None, 0x100, false, 0);
        assert!(!changed, "no previous key to compare against");
        assert!(!gesture);
        assert_eq!(counter, 0);
    }

    #[test]
    fn secondary_lighting_history_ignores_dark_light_motion() {
        let mut light = [[0.0; 4]; LIGHT_VEC4_STRIDE];
        light[0] = [0.0, 1.0, 0.0, 0.0];
        light[1][3] = -1.0; // no shadow slot
        let key = |data: &[[f32; 4]]| compute_rt_lighting_key(&[], &[1.0; 3], None, data);
        let dark = key(&light);
        light[0][0] = 0.6;
        assert_eq!(dark, key(&light), "dark light motion must preserve history");
        light[1][0] = 1.0;
        let lit = key(&light);
        assert_ne!(dark, lit, "turning the light on must invalidate history");
        light[0][0] = 0.2;
        assert_ne!(lit, key(&light), "moving an unshadowed lit source must invalidate history");
    }

    // ---- geo key (compute_rt_lighting_geo_key) ----

    #[test]
    fn geo_key_hashes_geometry_and_svt_slot() {
        let k1 = compute_rt_lighting_geo_key(&[], 0);
        let k2 = compute_rt_lighting_geo_key(&[], 1);
        assert_ne!(k1, k2, "svt slot swap must flip the geo key");
    }

    #[test]
    fn geo_key_ignores_color() {
        let c1 = manifold_gpu::raytrace::RtCasterParams::new([0.0, 0.0, 0.0], 0.0, [1.0, 0.0, 0.0], 0);
        let c2 = manifold_gpu::raytrace::RtCasterParams::new([0.0, 0.0, 0.0], 0.0, [0.0, 1.0, 0.0], 0);
        assert_eq!(
            compute_rt_lighting_geo_key(&[c1], 0),
            compute_rt_lighting_geo_key(&[c2], 0),
            "color change must NOT flip the geo key"
        );
    }

    #[test]
    fn geo_key_flips_on_position_change() {
        let c1 = manifold_gpu::raytrace::RtCasterParams::new([0.0, 0.0, 0.0], 0.0, [1.0, 1.0, 1.0], 0);
        let c2 = manifold_gpu::raytrace::RtCasterParams::new([0.1, 0.0, 0.0], 0.0, [1.0, 1.0, 1.0], 0);
        assert_ne!(
            compute_rt_lighting_geo_key(&[c1], 0),
            compute_rt_lighting_geo_key(&[c2], 0),
            "position change must flip the geo key"
        );
    }

// ---- SCENE_RENDER_MODE_DESIGN.md invariants (INV-R1 / INV-R3 / INV-R4) ----

#[test]
fn render_mode_default_is_rendered_and_fills() {
    // INV-R1: unwired = RenderMode::default() = Rendered = byte-identical
    // to no input. The default flows through the SAME branch a wired
    // Rendered value takes (mode 0 → no material substitution, Fill), so
    // unwired and wired-Rendered are the identical code path by
    // construction; these assertions pin the helper behavior both rely on.
    let default = crate::node_graph::render_mode::RenderMode::default();
    assert_eq!(default.mode, crate::node_graph::render_mode::RENDER_MODE_RENDERED);
    assert_eq!(
        color_pass_fill_mode(&default),
        manifold_gpu::GpuTriangleFillMode::Fill,
        "Rendered (and therefore unwired) must draw Fill"
    );
    let wired_rendered = crate::node_graph::render_mode::RenderMode {
        mode: crate::node_graph::render_mode::RENDER_MODE_RENDERED,
        line_color: [1.0, 0.0, 0.0, 1.0],
        line_brightness: 4.0,
        ..default
    };
    assert_eq!(
        color_pass_fill_mode(&wired_rendered),
        manifold_gpu::GpuTriangleFillMode::Fill,
        "a wired Rendered value must be indistinguishable from unwired"
    );
}

#[test]
fn render_mode_wireframe_draws_lines_and_substitutes_unlit_line_material() {
    // D6: color pass gets Lines; shading is unlit line_color × brightness.
    let mode = crate::node_graph::render_mode::RenderMode {
        mode: crate::node_graph::render_mode::RENDER_MODE_WIREFRAME,
        line_color: [0.5, 1.0, 0.25, 1.0],
        line_brightness: 2.0,
        ..Default::default()
    };
    assert_eq!(
        color_pass_fill_mode(&mode),
        manifold_gpu::GpuTriangleFillMode::Lines
    );
    let material = wireframe_material(&mode);
    assert_eq!(material.kind, crate::node_graph::material::MaterialKind::Unlit);
    assert_eq!(
        material.base_color,
        [0.5 * 2.0, 1.0 * 2.0, 0.25 * 2.0, 1.0],
        "unlit line color × line_brightness (D6)"
    );
}

#[test]
fn render_mode_solid_substitutes_phong_clay_material() {
    // D7: under Solid every object's effective material is a synthesized
    // Phong carrying the wire's flat clay_color with neutral specular, and
    // the color pass still draws Fill (Solid is a shading substitution,
    // not a topology change).
    let mode = crate::node_graph::render_mode::RenderMode {
        mode: crate::node_graph::render_mode::RENDER_MODE_SOLID,
        clay_color: [0.7, 0.4, 0.2, 1.0],
        ..Default::default()
    };
    assert_eq!(
        color_pass_fill_mode(&mode),
        manifold_gpu::GpuTriangleFillMode::Fill,
        "Solid draws filled triangles, not lines"
    );
    let material = clay_material(&mode);
    assert_eq!(
        material.kind,
        crate::node_graph::material::MaterialKind::Phong,
        "D7: Solid rides the existing Phong pipeline — every object's effective kind is Phong"
    );
    assert_eq!(
        material.base_color, mode.clay_color,
        "clay_color flows from the wire into the synthesized material"
    );
    // Neutral specular/emission — the struct defaults, independent of the
    // object's own material, with zero emission so the flat clay reads as
    // lit surface only.
    assert_eq!(material.specular_color, [1.0, 1.0, 1.0, 1.0]);
    assert_eq!(material.specular_power, 32.0);
    assert_eq!(material.emission, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(material.ambient, 0.0);
    assert_eq!(
        material.alpha_mode,
        crate::node_graph::material::AlphaMode::Opaque,
        "clay is opaque coverage regardless of the object's own alpha mode"
    );
}

#[test]
fn render_mode_solid_rt_enabled_collapses_to_rendered() {
    // INV-R4 for Solid: rt_enabled + Solid produces the Rendered uniform
    // set — the effective mode collapses to default, so no clay material
    // ever reaches the gi_materials table.
    let solid = crate::node_graph::render_mode::RenderMode {
        mode: crate::node_graph::render_mode::RENDER_MODE_SOLID,
        ..Default::default()
    };
    let effective = effective_render_mode(&solid, true);
    assert_eq!(
        effective,
        crate::node_graph::render_mode::RenderMode::default(),
        "rt_enabled must ignore the wire (D4)"
    );
    assert_eq!(
        color_pass_fill_mode(&effective),
        manifold_gpu::GpuTriangleFillMode::Fill
    );
    // Without RT the same wire applies — the gate is rt_enabled, not the
    // wire's presence.
    assert_eq!(
        effective_render_mode(&solid, false).mode,
        crate::node_graph::render_mode::RENDER_MODE_SOLID
    );
}

#[test]
fn render_mode_modes_compose_at_one_match() {
    // The substitution arms must live in ONE match at the gather site
    // (P2/P3 briefs): same site, same mechanism, one branch per mode — never
    // two scattered conditionals reading the mode separately.
    let source = include_str!("../render_scene.rs");
    let gather = source
        .split_once("fn collect_object_draws<'ctx, 'gpu>")
        .unwrap()
        .1
        .split_once("\n    fn ")
        .unwrap()
        .0;
    assert_eq!(
        gather.matches("let material = match render_mode.mode").count(),
        1,
        "the material substitution must be a single match on the mode"
    );
    assert!(
        gather.contains("RENDER_MODE_WIREFRAME => wireframe_material(&render_mode)"),
        "wireframe arm must live in the shared match"
    );
    assert!(
        gather.contains("RENDER_MODE_SOLID => clay_material(&render_mode)"),
        "clay arm must live in the shared match"
    );
    assert!(
        gather.contains("RENDER_MODE_POINTS => wireframe_material(&render_mode)"),
        "points arm must live in the shared match (no fourth scattered branch)"
    );
}

#[test]
fn render_mode_rt_enabled_ignores_the_wire() {
    // INV-R4: rt_enabled + wireframe produces the Rendered uniform set —
    // the effective mode collapses to default, so fill mode is Fill and
    // no wireframe material ever reaches the gi_materials table.
    let wireframe = crate::node_graph::render_mode::RenderMode {
        mode: crate::node_graph::render_mode::RENDER_MODE_WIREFRAME,
        ..Default::default()
    };
    let effective = effective_render_mode(&wireframe, true);
    assert_eq!(
        effective,
        crate::node_graph::render_mode::RenderMode::default(),
        "rt_enabled must ignore the wire (D4)"
    );
    assert_eq!(
        color_pass_fill_mode(&effective),
        manifold_gpu::GpuTriangleFillMode::Fill
    );
    // And without RT the same wire applies — the gate is rt_enabled, not
    // the wire's presence.
    assert_eq!(
        effective_render_mode(&wireframe, false).mode,
        crate::node_graph::render_mode::RENDER_MODE_WIREFRAME
    );
}

#[test]
fn depth_and_shadow_passes_force_fill_regardless_of_carried_fill_mode() {
    // INV-R3: the depth-only batch entry (shadow maps + opaque depth
    // prepass) forces GpuTriangleFillMode::Fill per draw in the encoder —
    // a DepthMsaaDraw carrying Lines can never leak into a depth pass.
    // Structural source check, same pattern as the topology-order tests
    // above: the forcing site must name Fill and must NOT read the
    // per-draw field.
    let source = include_str!("../render_scene.rs");
    let _ = source;
    let encoder = include_str!("../../../../../manifold-gpu/src/metal/encoder.rs");
    let depth_only = encoder
        .split_once("pub fn draw_instanced_depth_only_batch")
        .expect("depth-only batch entry must exist")
        .1
        .split_once("\n    /// ")
        .unwrap()
        .0;
    let force = depth_only
        .split_once("setTriangleFillMode")
        .expect("depth-only pass must force a fill mode per draw (INV-R3)")
        .1;
    assert!(
        force.contains("GpuTriangleFillMode::Fill"),
        "depth-only pass must force Fill, not read the draw's fill_mode"
    );
    assert!(
        !force.split_once(')').unwrap().0.contains("draw.fill_mode"),
        "depth-only pass must NOT read draw.fill_mode (INV-R3)"
    );
    // The color-pass entries must apply the per-draw field (D6) — proving
    // the flag reaches color draws and ONLY color draws.
    for entry in ["pub fn draw_instanced_depth_msaa_batch_desc", "pub fn draw_instanced_depth_batch"] {
        let body = encoder
            .split_once(entry)
            .expect("colour batch entry must exist")
            .1
            .split_once("\n    /// ")
            .unwrap()
            .0;
        assert!(
            body.contains("setTriangleFillMode(format::to_mtl_triangle_fill_mode(draw.fill_mode))"),
            "{entry} must apply the per-draw fill mode"
        );
    }
}

#[test]
fn render_mode_points_draws_point_topology_with_unlit_line_shading() {
    // D8: Points = the same vertex buffers as point primitives, shaded with
    // the SAME unlit line surface as wireframe (D2: line_color ×
    // line_brightness serve Points too) — a shading substitution plus a
    // topology flag, not a new material family.
    let mode = crate::node_graph::render_mode::RenderMode {
        mode: crate::node_graph::render_mode::RENDER_MODE_POINTS,
        line_color: [0.25, 0.5, 1.0, 1.0],
        line_brightness: 2.0,
        point_size: 5.0,
        ..Default::default()
    };
    assert!(
        color_pass_points(&mode),
        "Points mode must select point topology"
    );
    assert_eq!(
        color_pass_fill_mode(&mode),
        manifold_gpu::GpuTriangleFillMode::Fill,
        "Points is not a fill-mode change — triangles stay filled"
    );
    let material = wireframe_material(&mode);
    assert_eq!(
        material.kind,
        crate::node_graph::material::MaterialKind::Unlit,
        "Points rides the synthesized unlit line material, same as wireframe"
    );
    assert_eq!(
        material.base_color,
        [0.25 * 2.0, 0.5 * 2.0, 1.0 * 2.0, 1.0],
        "point shading = line_color × line_brightness (D2)"
    );
    // Unwired/Rendered never selects points — INV-R1 parity by construction.
    assert!(
        !color_pass_points(&crate::node_graph::render_mode::RenderMode::default()),
        "Rendered (and therefore unwired) must draw triangles"
    );
    // The wire's point_size must reach the uniform slot every draw carries
    // (build_uniforms is the sole RenderSceneUniforms producer).
    let material = crate::node_graph::material::Material::default_unlit_white();
    let cam = crate::node_graph::camera::Camera::default_perspective();
    let uniforms = build_uniforms(
        [[0.0; 4]; 4],
        [[0.0; 4]; 4],
        &cam,
        &material,
        0.0,
        0.0,
        &crate::node_graph::atmosphere::Atmosphere::default(),
        [[0.0; 4]; 4],
        [[0.0; 4]; 4],
        1.0,
        false,
        mode.point_size,
    );
    assert_eq!(
        uniforms.render_mode,
        [5.0, 0.0, 0.0, 0.0],
        "point_size must flow wire → uniform render_mode.x (D8)"
    );
}

#[test]
fn render_mode_points_rt_enabled_collapses_to_rendered() {
    // INV-R4 for Points: rt_enabled + Points produces the Rendered uniform
    // set — the effective mode collapses to default, so no point topology
    // and no unlit line material ever reach the raster path.
    let points = crate::node_graph::render_mode::RenderMode {
        mode: crate::node_graph::render_mode::RENDER_MODE_POINTS,
        ..Default::default()
    };
    let effective = effective_render_mode(&points, true);
    assert_eq!(
        effective,
        crate::node_graph::render_mode::RenderMode::default(),
        "rt_enabled must ignore the wire (D4)"
    );
    assert!(
        !color_pass_points(&effective),
        "rt_enabled + Points must not select point topology"
    );
    assert_eq!(
        effective_render_mode(&points, false).mode,
        crate::node_graph::render_mode::RENDER_MODE_POINTS,
        "without RT the same wire applies"
    );
}

#[test]
fn color_passes_apply_draw_topology_and_depth_forces_triangles() {
    // INV-R3, topology half: the depth-only batch entry (shadow maps +
    // opaque depth prepass) hardcodes MTLPrimitiveType::Triangle — a
    // DepthMsaaDraw carrying Point can never leak into a depth pass. The
    // colour batches map the per-draw field (D8), same shape as the
    // fill-mode check above.
    let encoder = include_str!("../../../../../manifold-gpu/src/metal/encoder.rs");
    let depth_only = encoder
        .split_once("pub fn draw_instanced_depth_only_batch")
        .expect("depth-only batch entry must exist")
        .1
        .split_once("\n    /// ")
        .unwrap()
        .0;
    let draw_call = depth_only
        .split_once("drawPrimitives_vertexStart_vertexCount_instanceCount")
        .expect("depth-only pass must encode a draw (INV-R3)")
        .1;
    assert!(
        draw_call.contains("MTLPrimitiveType::Triangle"),
        "depth-only pass must force Triangle topology"
    );
    assert!(
        !draw_call.split_once(')').unwrap().0.contains("draw.primitive"),
        "depth-only pass must NOT read draw.primitive (INV-R3)"
    );
    for entry in ["pub fn draw_instanced_depth_msaa_batch_desc", "pub fn draw_instanced_depth_batch"] {
        let body = encoder
            .split_once(entry)
            .expect("colour batch entry must exist")
            .1
            .split_once("\n    /// ")
            .unwrap()
            .0;
        assert!(
            body.contains("to_mtl_primitive_type(draw.primitive)"),
            "{entry} must apply the per-draw topology"
        );
    }
    // The points constructor must set Point topology + Fill fill mode.
    let ctor = encoder
        .split_once("pub fn depth_msaa_draw_points")
        .expect("points draw constructor must exist")
        .1
        .split_once("\n    /// ")
        .unwrap()
        .0;
    assert!(
        ctor.contains("GpuPrimitiveType::Point"),
        "depth_msaa_draw_points must carry Point topology"
    );
    assert!(
        ctor.contains("GpuTriangleFillMode::Fill"),
        "points draws must not carry a fill mode"
    );
}

#[test]
fn points_mode_switches_the_scene_pass_to_real_attachments() {
    // D8 root fix: the tiling parameter buffer caps TOTAL point primitives
    // per memoryless-attachment pass at a few thousand (measured on Apple
    // Silicon: 2048 points pass, 4096 fault), so a Points-active frame must
    // swap the scene pass to real-storage MSAA attachments, and must NOT
    // declare memoryless aux attachments (they would reimpose the budget and
    // mismatch the single-attachment points pipeline). Structural checks on
    // both sites.
    let source = include_str!("../render_scene.rs");
    let ensure = source
        .split_once("fn ensure_msaa_targets(")
        .expect("ensure_msaa_targets must exist")
        .1
        .split_once("\n    fn ")
        .unwrap()
        .0;
    assert!(
        ensure.contains("create_texture_msaa("),
        "Points-active frames must allocate REAL MSAA attachments"
    );
    assert!(
        ensure.contains("create_texture_msaa_memoryless("),
        "triangle-mode frames keep the memoryless pair"
    );
    assert!(
        ensure.contains("self.msaa_real == points_active"),
        "the flavor must flip back to memoryless when Points turns off"
    );
    assert!(
        source.matches("!points_active").count() >= 7,
        "every aux-attachment pairing (velocity, ao_mask, five denoise feeds) \
         must be gated off under Points"
    );
}

#[test]
fn render_mode_branch_touches_no_uniforms_outside_the_draw_flags() {
    // INV-R1 (parity half): the mode branch may only (a) substitute the
    // object's material, (b) set the draw's fill/topology flags, and (c) put
    // the wire's point_size into the always-carried uniform slot (inert
    // unless the points pipeline reads it) — nothing else in
    // collect_object_draws may read render_mode, so a Rendered/default
    // value leaves every uniform and pipeline decision untouched.
    let source = include_str!("../render_scene.rs");
    let gather = source
        .split_once("fn collect_object_draws<'ctx, 'gpu>")
        .unwrap()
        .1
        .split_once("\n    fn ")
        .unwrap()
        .0;
    let mentions: Vec<&str> = gather
        .lines()
        .filter(|line| line.contains("render_mode"))
        .collect();
    for line in &mentions {
        let allowed = line.contains("objects, cam, envmap_wired, atmosphere, render_mode,")
            || line.contains("let render_mode = effective_render_mode(render_mode, *rt_enabled)")
            || line.contains("let material = match render_mode.mode")
            || line.contains("wireframe_material(&render_mode)")
            || line.contains("clay_material(&render_mode)")
            || line.contains("fill_mode: color_pass_fill_mode(&render_mode)")
            || line.contains("let points = color_pass_points(&render_mode)")
            || line.contains("render_mode.point_size,")
            || line.trim() == "points,";
        assert!(
            allowed,
            "collect_object_draws touched render_mode outside the mode branch: {line}"
        );
    }
    assert!(
        mentions.len() >= 9,
        "the mode branch must exist: destructure, effective-mode gate, \
         substitution match, three arms, fill_mode field, points flag, \
         point_size uniform"
    );
    // INV-R1 (Rendered parity): the match's fall-through arm hands the
    // object's own material through by name — Rendered and Points leave the
    // gathered material untouched, so a Rendered/default value is
    // byte-identical to no mode at all.
    assert!(
        gather.contains("_ => material,"),
        "the substitution match must pass the object's material through \
         unchanged for Rendered/Points (INV-R1)"
    );
}
