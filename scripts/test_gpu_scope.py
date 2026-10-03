#!/usr/bin/env python3
"""Scope selection: given these touched paths, these filters are chosen."""

import json
import unittest
from unittest import mock
from pathlib import Path
import tempfile

import gpu_scope as g

R = "crates/manifold-renderer/src/"
P = R + "node_graph/primitives/"


def plan(paths, users=None, repo=None):
    return g.plan_for_paths(paths, repo or Path("/nonexistent"), shader_users=users or (lambda p: []))


class ScopeTests(unittest.TestCase):
    def test_step_order_cpu_reference_selects_gpu_value_proofs(self):
        path = P + "gpu_flip_extension_tests.rs"
        result = plan([path], repo=self._repo_with(path))
        self.assertIn("gpu_flip_step_order_", result.filters)
        self.assertIn("gpu_flip_extend_faces_", result.filters)
        self.assertFalse(result.unmapped)
    def test_mesh_grid_sources_select_native_value_proofs(self):
        for path in (R + "node_graph/liquid/lattice.rs", P + "liquid_frame.rs",
                     P + "liquid_solid_distance.rs", P + "shaders/liquid_solid_distance_body.wgsl"):
            result = plan([path], users=lambda _: [P + "liquid_solid_distance.rs"],
                          repo=self._repo_with(path))
            self.assertIn("fluid_mesh_grid_native_", result.filters)
            self.assertIn("mesh_contact_oblique_wall_and_thin_plate_match_cpu_reference", result.filters)
            self.assertFalse(result.unmapped)
    def test_particle_publication_selects_identity_and_pass_one_proofs(self):
        required = {"particle_publication_gpu_tests::",
                    "particle_frame_blend_tests::gpu_tests::",
                    "interpolate_particle_frames::gpu_tests::",
                    "push_out_of_solid::gpu_tests::", "mix_arrays::gpu_tests::",
                    "gpu_flip_inflow_emits_at_empty_sites_into_free_slots",
                    "gpu_flip_narrow_band_publication_repeats_failed_ticks"}
        for name in ("particle_identity.rs", "particle_publication.rs",
                     "particle_publication_gpu_tests.rs", "liquid_frame.rs",
                     "shaders/particle_identity.wgsl", "shaders/particle_publication.wgsl"):
            with self.subTest(path=name):
                source = P + name.rsplit("/", 1)[-1].replace(".wgsl", ".rs")
                result = plan([P + name], users=lambda _: [source],
                              repo=self._repo_with(P + name))
                self.assertTrue(required <= result.filters)
                self.assertFalse(result.unmapped)
                self.assertFalse(result.broad)

    def test_live_clock_and_duration_atoms_select_value_proofs(self):
        for name in ("gpu_flip_clock.rs", "shaders/gpu_flip_clock.wgsl"):
            result = plan([P + name], users=lambda _: [P + "gpu_flip_clock.rs"],
                          repo=self._repo_with(P + name))
            self.assertIn("gpu_flip_clock::gpu_tests::", result.filters)
            self.assertNotIn("gpu_flip_", result.filters)
            self.assertFalse(result.unmapped)
        for name in ("emission_count.rs", "spawn_whitewater.rs",
                     "shaders/emission_count_body.wgsl", "shaders/spawn_whitewater_body.wgsl"):
            result = plan([P + name], users=lambda _: [P + "emission_count.rs"],
                          repo=self._repo_with(P + name))
            self.assertIn("whitewater_particle_tests::", result.filters)
            self.assertFalse(result.unmapped)

    def test_blob_bounds_selects_dense_and_sparse_consumers(self):
        for path in (P + "blob_bounds.rs", P + "shaders/blob_bounds.wgsl"):
            result = plan([path], users=lambda _: [P + "blob_bounds.rs"],
                          repo=self._repo_with(path))
            self.assertTrue({"node_graph::primitives::blob_bounds::",
                             "liquid_surface_tests::",
                             "liquid_bricks::tests::gpu_tests::"} <= result.filters)
            self.assertFalse(result.unmapped)

    def test_narrow_band_isolated_passes_select_their_value_proofs(self):
        for path in (P + "gpu_flip_narrow_band_tests.rs",
                     P + "gpu_flip_narrow_band.rs",
                     P + "shaders/gpu_flip_narrow_band.wgsl"):
            result = plan([path], users=lambda _: [P + "gpu_flip_narrow_band_tests.rs"],
                          repo=self._repo_with(path))
            self.assertTrue(set(g.SMOKE_FILTERS + ["narrow_band", "face_grid_demo_gpu_flip_and_matter_side_by_side"]) <= set(result.final_filters()))
            self.assertNotIn("gpu_flip_", result.filters)
            self.assertFalse(result.broad)
            self.assertFalse(result.unmapped)

    def test_whitewater_emitters_select_shared_value_and_fusion_proofs(self):
        expected = "node_graph::primitives::whitewater_emitter_gpu_tests::"
        for atom in ("turbulence_field", "inside_turbulence_potential",
                     "turbulence_emission_count", "whitewater_emitter_velocity",
                     "whitewater_obstacle_source", "whitewater_influence", "dust_potential"):
            source = P + atom + ".rs"
            shader = P + "shaders/" + atom + "_body.wgsl"
            for path in (source, shader):
                result = plan([path], users=lambda _: [source], repo=self._repo_with(path))
                self.assertIn(expected, result.filters)
                self.assertFalse(result.unmapped)
                self.assertFalse(result.broad)

    def test_non_gpu_paths_run_nothing(self):
        p = plan(["docs/X.md", "scripts/a.py", "crates/manifold-ui/src/lib.rs"])
        self.assertFalse(p.active)
        self.assertEqual(p.runs(), [])

    def test_primitive_maps_to_its_own_proofs_plus_smoke(self):
        p = plan([P + "invert.rs"])
        self.assertEqual(p.final_filters(), sorted(g.SMOKE_FILTERS + ["node_graph::primitives::invert::"]))
        self.assertFalse(p.glb)
        self.assertEqual(p.broad, [])

    def test_primitive_gpu_tests_file_maps_to_parent_primitive(self):
        p = plan([P + "analytic_echo_instances_gpu_tests.rs"])
        self.assertIn("node_graph::primitives::analytic_echo_instances::", p.filters)

    def test_primitive_directory_maps_to_dir_module(self):
        p = plan([P + "render_scene/lights.rs"])
        self.assertIn("node_graph::primitives::render_scene::lights::", p.filters)

    def test_primitive_shader_maps_through_its_user(self):
        p = plan([P + "shaders/invert.wgsl"], users=lambda s: [P + "invert.rs"],
                 repo=self._repo_with(P + "shaders/invert.wgsl"))
        self.assertIn("node_graph::primitives::invert::", p.filters)
        self.assertEqual(p.broad, [])

    def test_shader_included_by_another_shader_reaches_the_rust_user(self):
        # A touched .wgsl that another .wgsl includes walks the frontier (was a crash: list.add).
        repo = self._repo_with("crates/x/a.wgsl")
        (repo / "crates/x/b.wgsl").write_text("// uses a.wgsl\n")
        (repo / "crates/x/c.rs").write_text('include_str!("b.wgsl");\n')
        self.assertEqual(g.default_shader_users(repo, "crates/x/a.wgsl"), ["crates/x/c.rs"])

    def test_shader_with_no_user_is_unmapped_and_named(self):
        p = plan([P + "shaders/orphan.wgsl"], repo=self._repo_with(P + "shaders/orphan.wgsl"))
        self.assertEqual([x[0] for x in p.unmapped], [P + "shaders/orphan.wgsl"])
        msg = g.unmapped_message(p)
        self.assertIn(P + "shaders/orphan.wgsl", msg)
        self.assertIn("scripts/gpu_scope.py", msg)

    def test_shared_wgsl_maps_to_named_bounded_broad_set(self):
        many = [P + f"p{i}.rs" for i in range(g.SHARED_WGSL_USERS + 1)]
        p = plan([P + "shaders/noise.wgsl"], users=lambda s: many,
                 repo=self._repo_with(P + "shaders/noise.wgsl"))
        self.assertEqual(p.final_filters(), sorted(set(g.SMOKE_FILTERS + g.BROAD_FILTERS)))
        self.assertEqual(p.broad[0][0], P + "shaders/noise.wgsl")

    def test_manifold_gpu_core_maps_to_broad_not_everything(self):
        p = plan(["crates/manifold-gpu/src/metal/device.rs"])
        self.assertEqual(p.final_filters(), sorted(set(g.SMOKE_FILTERS + g.BROAD_FILTERS)))
        self.assertFalse(p.glb)
        self.assertEqual(len(p.runs()), 1)

    def test_broad_set_is_bounded(self):
        # Never a bare gpu_proofs/lib sweep: every filter names something specific.
        for f in g.BROAD_FILTERS + g.SMOKE_FILTERS:
            self.assertGreater(len(f), 8)

    def test_freeze_and_runtime(self):
        p = plan([R + "node_graph/freeze/codegen/fused.rs"])
        self.assertIn("freeze::", p.filters)
        p = plan([R + "node_graph/execution/foo.rs"])
        self.assertTrue(set(g.RUNTIME_FILTERS) <= p.filters)

    def test_rt_row_keeps_particletext_skip_and_union_with_freeze(self):
        p = plan(["crates/manifold-gpu/src/metal/raytrace.rs", R + "node_graph/freeze/x.rs"])
        self.assertTrue({"rt_", "freeze::"} <= p.filters)
        self.assertEqual(sorted(set(p.final_skips()) - {n for n, _ in g.slow_tests()}), ["particletext"])
        self.assertEqual(p.broad, [])

    def test_skip_dropped_when_it_would_hide_a_selected_filter(self):
        p = plan(["crates/manifold-gpu/src/metal/raytrace.rs", P + "particletext.rs"])
        self.assertEqual(sorted(set(p.final_skips()) - {n for n, _ in g.slow_tests()}), [])

    def test_matter_row(self):
        p = plan([P + "matter_fill.rs"])
        self.assertTrue({"matter_", "substeps_"} <= p.filters)

    def test_batch_two_paths_select_face_grid_demo(self):
        name = "node_graph::primitives::face_grid_scene_tests::face_grid_demo_gpu_flip_and_matter_side_by_side"
        for path in (P + "gpu_flip_step.rs", P + "shaders/gpu_flip_step.wgsl",
                     P + "gpu_flip_pressure.rs", P + "shaders/gpu_flip_pressure.wgsl",
                     P + "gpu_flip_lentine.rs", P + "shaders/gpu_flip_lentine.wgsl",
                     P + "gpu_flip_narrow_band.rs", P + "shaders/gpu_flip_narrow_band.wgsl"):
            result = plan([path], users=lambda _: [P + "gpu_flip_step.rs"],
                          repo=self._repo_with(path))
            self.assertTrue(any(f in name for f in result.final_filters()), path)
            self.assertFalse(any(s in name for s in result.final_skips()), path)

    def test_gpu_flip_row_reaches_the_scene_proofs(self):
        for path in (P + "gpu_flip_step.rs", P + "liquid_state.rs", R + "node_graph/liquid/extent.rs"):
            self.assertTrue({"gpu_flip_", "face_grid_tests::"} <= plan([path]).filters, path)
        shader = P + "shaders/gpu_flip_step.wgsl"
        p = plan([shader], users=lambda s: [P + "gpu_flip_step.rs"], repo=self._repo_with(shader))
        self.assertIn("gpu_flip_", p.filters)

    def test_clock_and_fields_get_force_proofs_not_body_or_step(self):
        for path in (R + "node_graph/liquid/clock.rs", R + "node_graph/liquid/fields.rs",
                     R + "node_graph/liquid/fields/tests.rs"):
            p = plan([path])
            self.assertIn("gpu_flip_face_gravity", p.filters, path)
            self.assertNotIn("gpu_flip_", p.filters, path)
            self.assertFalse(any(f.startswith("gpu_flip_body") for f in p.filters), path)

    def test_domain_nodes_are_narrow(self):
        p = plan([P + "gpu_flip_domain.rs", P + "matter_domain.rs"])
        self.assertIn("gpu_flip_domain_", p.filters)
        self.assertIn("matter_scene::", p.filters)
        self.assertNotIn("gpu_flip_", p.filters)
        self.assertNotIn("matter_", p.filters)

    def test_body_step_and_pressure_paths_still_pull_the_body_proofs(self):
        for path in (P + "gpu_flip_bodies.rs", P + "gpu_flip_body_tests.rs",
                     P + "gpu_flip_step.rs", P + "gpu_flip_pressure.rs",
                     R + "node_graph/liquid/bodies.rs", R + "node_graph/liquid/coupling.rs"):
            self.assertIn("gpu_flip_", plan([path]).filters, path)

    def test_mixed_diff_keeps_the_broad_row_whole(self):
        p = plan([R + "node_graph/liquid/clock.rs", P + "gpu_flip_step.rs"])
        self.assertIn("gpu_flip_", p.filters)

    def test_reporters_skip_unless_their_own_file_is_touched(self):
        for name in g.REPORTER_SKIPS:
            self.assertIn(name, plan([P + "gpu_flip_step.rs"]).final_skips() +
                          plan([P + "matter_fill.rs"]).final_skips())
        own = plan(["crates/manifold-renderer/tests/gpu_proofs/matter_cost_probe.rs"])
        self.assertNotIn("matter_cost_probe", own.final_skips())

    def with_times(self, times):
        d = tempfile.mkdtemp()
        self.addCleanup(lambda: __import__("shutil").rmtree(d, ignore_errors=True))
        f = Path(d) / "t.json"
        f.write_text(json.dumps({"tests": times}))
        patcher = mock.patch.object(g, "TIMES_PATH", f)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_over_threshold_measured_test_is_skipped_and_reported(self):
        self.with_times({"a::slow": 61.0, "a::fast": 59.0, "a::exact": 60.0})
        p = plan([P + "matter_fill.rs"])
        self.assertIn("a::slow", p.final_skips())
        self.assertNotIn("a::fast", p.final_skips())
        self.assertNotIn("a::exact", p.final_skips())
        self.assertIn("a::slow: skipped, run nightly only (measured 61s)", p.describe())
        self.assertEqual(p.runs()[0]["skips"], p.final_skips())

    def test_glb_sweep_time_never_skips_or_reports_the_sweep(self):
        self.with_times({"glb_conformance_sweep": 930.0, "a::slow": 61.0})
        p = plan([R + "node_graph/gltf_import/mod.rs"])
        self.assertTrue(p.glb)
        self.assertNotIn("glb_conformance_sweep", p.final_skips())
        self.assertNotIn("glb_conformance_sweep", p.describe())
        self.assertEqual(p.runs()[-1]["skips"], [])
        self.assertIn("a::slow", p.final_skips())

    def test_test_missing_from_times_file_runs(self):
        self.with_times({"a::slow": 500.0})
        self.assertNotIn("brand::new_test", plan([P + "matter_fill.rs"]).final_skips())

    def test_missing_times_file_skips_nothing(self):
        with mock.patch.object(g, "TIMES_PATH", Path("/nonexistent/t.json")):
            self.assertEqual(g.slow_tests(), [])

    def test_hand_list_is_gone(self):
        self.assertFalse(hasattr(g, "NIGHTLY_ONLY"))

    def test_committed_times_file_seeds_the_known_slow_tests(self):
        names = {n for n, _ in g.slow_tests()}
        self.assertIn("matter_bodies::matter_fill_skips_colliders", names)
        self.assertIn("matter_look::matter_momentum_conserved_free_blob", names)

    def test_proof_file_maps_to_its_own_module(self):
        p = plan([g.PROOFS_DIR + "render_scene_fog.rs"])
        self.assertIn("render_scene_fog::", p.filters)
        p = plan([g.PROOFS_DIR + "water_basin/helpers.rs"])
        self.assertIn("water_basin::", p.filters)

    def test_harness_is_broad(self):
        p = plan([g.PROOFS_DIR + "harness.rs"])
        self.assertTrue(set(g.BROAD_FILTERS) <= p.filters)

    def test_glb_runs_only_for_gltf_paths(self):
        self.assertFalse(plan([P + "invert.rs"]).glb)
        self.assertFalse(plan(["crates/manifold-gpu/src/metal/device.rs"]).glb)
        for path in ["crates/manifold-renderer/tests/glb_conformance.rs",
                     "tests/fixtures/gltf/khronos/manifest.json",
                     R + "node_graph/gltf_import/mod.rs"]:
            p = plan([path])
            self.assertTrue(p.glb, path)
            runs = p.runs()
            self.assertEqual(runs[-1]["targets"], ["glb_conformance"])
            self.assertFalse(runs[-1]["budgeted"])
            self.assertTrue(runs[0]["budgeted"])

    def test_unknown_file_type_in_gpu_dir_is_unmapped(self):
        p = plan([R + "node_graph/something.bin"])
        self.assertEqual(p.unmapped[0][0], R + "node_graph/something.bin")

    def test_main_run_targets_lib_and_gpu_proofs_never_glb(self):
        run = plan([P + "invert.rs"]).runs()[0]
        self.assertTrue(run["lib"])
        self.assertEqual(run["targets"], ["gpu_proofs"])

    def test_preset_runtime_test_file_maps_to_its_declared_module(self):
        repo = self._repo_with(R + "preset_runtime/mod.rs")
        (repo / R / "preset_runtime/mod.rs").write_text(
            '#[cfg(test)]\n#[path = "tests/layer_skin.rs"]\nmod layer_skin_tests;\n')
        p = plan([R + "preset_runtime/tests/layer_skin.rs"], repo=repo)
        self.assertTrue(p.active)
        self.assertIn("preset_runtime::layer_skin_tests::", p.filters)
        self.assertTrue(p.runs()[0]["lib"])

    def test_undeclared_preset_runtime_test_file_falls_back_to_the_module(self):
        p = plan([R + "preset_runtime/tests/unknown.rs"])
        self.assertIn("preset_runtime::", p.filters)

    def test_layer_skin_source_selects_its_lib_proofs(self):
        p = plan([R + "layer_skin.rs"])
        self.assertTrue(p.active)
        self.assertTrue({"layer_skin::", "preset_runtime::layer_skin_tests::"} <= p.filters)

    def _repo_with(self, rel):
        d = tempfile.mkdtemp()
        self.addCleanup(lambda: __import__("shutil").rmtree(d, ignore_errors=True))
        f = Path(d) / rel
        f.parent.mkdir(parents=True)
        f.write_text("")
        return Path(d)


if __name__ == "__main__":
    unittest.main()
