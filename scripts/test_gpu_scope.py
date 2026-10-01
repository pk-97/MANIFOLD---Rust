#!/usr/bin/env python3
"""Scope selection: given these touched paths, these filters are chosen."""

import unittest
from pathlib import Path
import tempfile

import gpu_scope as g

R = "crates/manifold-renderer/src/"
P = R + "node_graph/primitives/"


def plan(paths, users=None, repo=None):
    return g.plan_for_paths(paths, repo or Path("/nonexistent"), shader_users=users or (lambda p: []))


class ScopeTests(unittest.TestCase):
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
        self.assertEqual(sorted(set(p.final_skips()) - set(g.NIGHTLY_ONLY)), ["particletext"])
        self.assertEqual(p.broad, [])

    def test_skip_dropped_when_it_would_hide_a_selected_filter(self):
        p = plan(["crates/manifold-gpu/src/metal/raytrace.rs", P + "particletext.rs"])
        self.assertEqual(sorted(set(p.final_skips()) - set(g.NIGHTLY_ONLY)), [])

    def test_matter_row(self):
        p = plan([P + "matter_fill.rs"])
        self.assertTrue({"matter_", "substeps_"} <= p.filters)

    def test_matter_path_skips_nightly_only_tests(self):
        p = plan([P + "matter_fill.rs"])
        self.assertEqual(len(g.NIGHTLY_ONLY), 3)
        for t in g.NIGHTLY_ONLY:
            self.assertIn(t, p.final_skips())
        self.assertIn("run nightly only", p.describe())
        self.assertEqual(p.runs()[0]["skips"], p.final_skips())

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

    def _repo_with(self, rel):
        d = tempfile.mkdtemp()
        self.addCleanup(lambda: __import__("shutil").rmtree(d, ignore_errors=True))
        f = Path(d) / rel
        f.parent.mkdir(parents=True)
        f.write_text("")
        return Path(d)


if __name__ == "__main__":
    unittest.main()
