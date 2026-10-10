#!/usr/bin/env python3
"""Scope selection: given these touched paths, these filters are chosen."""

import json
import unittest
from unittest import mock
from pathlib import Path
import tempfile
import shutil
import subprocess

import gpu_scope as g
from gate_policy import WATER_BROAD_FILTERS

R = "crates/manifold-nodes/src/"
E = "crates/manifold-node-engine/src/"
T = "crates/manifold-nodes-water/src/"
W = T + "primitives/"
LT = "crates/manifold-water-liquid/src/"
LW = LT + "primitives/"
FT = "crates/manifold-water-gpu-flip/src/"
FW = FT + "primitives/"
P = E + "primitives/"


def plan(paths, users=None, repo=None):
    repo = repo or Path("/nonexistent")
    workspace = None if (repo / "Cargo.toml").is_file() else fixture_workspace(repo)
    return g.plan_for_paths(paths, repo, shader_users=users or (lambda p: []), workspace=workspace)


def fixture_workspace(repo):
    """Metadata-only workspace for path-rule tests without a Cargo checkout."""
    repo = repo.resolve()
    packages = []
    rows = {
        "manifold-app": ("crates/manifold-app", True, ["renderer_contracts", "renderer_gpu_proofs"]),
        "manifold-compositor": ("crates/manifold-compositor", True, ["gpu_proofs"]),
        "manifold-nodes-scene": ("crates/manifold-nodes-scene", True, ["gpu_proofs"]),
        "manifold-nodes-image": ("crates/manifold-nodes-image", True, []),
        "manifold-nodes": ("crates/manifold-nodes", True, ["gpu_proofs", "glb_conformance", "main"]),
        "manifold-node-engine": ("crates/manifold-node-engine", True, []),
        "manifold-nodes-water": ("crates/manifold-nodes-water", True, []),
        "manifold-water-rigid": ("crates/manifold-water-rigid", True, []),
        "manifold-water-liquid": ("crates/manifold-water-liquid", True, []),
        "manifold-water-gpu-flip": ("crates/manifold-water-gpu-flip", True, []),
        "manifold-ui-paint": ("crates/manifold-ui-paint", True, ["main"]),
        "manifold-gpu": ("crates/manifold-gpu", False, []),
    }
    for index, (name, (root, gpu, tests)) in enumerate(rows.items()):
        targets = [{"name": name, "kind": ["lib"], "src_path": str(repo / root / "src/lib.rs"),
                    "required-features": []}]
        targets.extend({"name": target, "kind": ["test"],
                        "src_path": str(repo / root / "tests" / ("renderer_contracts/gpu_proofs/main.rs" if target == "renderer_gpu_proofs" else "gpu_proofs/main.rs" if target == "gpu_proofs" else f"{target}.rs")),
                        "required-features": [] if (name in ("manifold-ui-paint", "manifold-nodes") and target == "main") or target == "renderer_contracts" else ["gpu-proofs"]}
                       for target in tests)
        packages.append({
            "id": f"path+file://{repo}/{root}#{name}@0.1.0",
            "name": name,
            "manifest_path": str(repo / root / "Cargo.toml"),
            "features": {"gpu-proofs": []} if gpu else {},
            "dependencies": [],
            "targets": targets,
        })
    return g.Workspace(repo, metadata={
        "workspace_members": [package["id"] for package in packages],
        "packages": packages,
    })


class ScopeTests(unittest.TestCase):
    def test_ui_projection_keeps_cpu_and_flow_coverage_without_renderer_proofs(self):
        import cpu_scope
        import run_ui_flows
        repo = Path(__file__).resolve().parents[1]
        workspace = g.Workspace(repo)
        manifest = json.loads((repo / 'scripts/ui-flows/manifest.json').read_text())
        sources = [repo / p for p in g.UI_PROJECTION_PATHS if not p.endswith('/')]
        sources.extend((repo / g.UI_PROJECTION_PATHS[0]).glob('*.rs'))
        proof_sources = set()
        for target in workspace.targets('manifold-app', 'test'):
            if 'gpu-proofs' in target.get('required-features', []) or target['name'] == 'renderer_contracts':
                proof_sources.update(g.module_mounts(target['src_path']))
        for source in sources:
            path = source.relative_to(repo).as_posix()
            with self.subTest(path=path):
                self.assertNotIn(source.resolve(), proof_sources)
                cpu = cpu_scope.plan_for_paths([path], repo, workspace)
                self.assertIn('manifold-app', cpu.packages)
                self.assertFalse(g.plan_for_paths([path], repo, workspace=workspace, cpu_plan=cpu).active)
                if source.name in {'timeline.rs', 'inspector.rs', 'cards.rs', 'scene.rs', 'material.rs'}:
                    self.assertTrue(run_ui_flows.filters_for_paths([path], manifest)[0])

    def test_ui_projection_does_not_hide_a_rendering_change(self):
        path = 'crates/manifold-app/src/ui_bridge/projection/timeline.rs'
        self.assertTrue(g.is_gpu_path(path.replace('.rs', '.wgsl'), fixture_workspace(Path('/nonexistent'))))
        for rendering in ('crates/manifold-app/src/app_render.rs',
                          'crates/manifold-app/src/content_commands.rs',
                          'crates/manifold-ui-paint/src/clip_content_gpu.rs'):
            with self.subTest(rendering=rendering):
                result = plan([path, rendering])
                self.assertEqual(result.paths, [rendering])
                self.assertTrue(result.active)
                self.assertTrue(set(g.SMOKE_FILTERS) <= set(result.final_filters()))

    def test_compositor_gpu_helpers_do_not_force_default_suite(self):
        import cpu_scope
        repo = Path(__file__).resolve().parents[1]
        path = 'crates/manifold-compositor/src/layer_compositor.rs'
        workspace = g.Workspace(repo)
        cpu = cpu_scope.plan_for_paths([path], repo, workspace)
        self.assertNotIn('manifold-compositor', cpu.packages)
        self.assertIn('contracts::node_graph::catalog_tests::layer_compositor::', cpu.filterset)
        gpu = g.plan_for_paths([path], repo, workspace=workspace, cpu_plan=cpu)
        self.assertIn('layer_compositor::', gpu.filters)
        self.assertIn('node_graph::catalog_tests::layer_compositor::', gpu.filters)

    def test_audited_catalog_edit_selects_three_owned_harnesses(self):
        repo = Path(__file__).resolve().parents[1]
        result = g.plan_for_paths([
            "crates/manifold-nodes/src/bundled_presets.rs",
            "crates/manifold-nodes/src/bundled_generator_presets.rs",
        ], repo)
        self.assertEqual(
            {(run["package"], run["target"]) for run in result.runs()},
            {("manifold-nodes", "lib"), ("manifold-nodes", "gpu_proofs"),
             ("manifold-nodes-scene", "gpu_proofs")},
        )
        self.assertTrue(all(run["filters"] == result.final_filters() for run in result.runs()))

    def test_unknown_filter_keeps_all_filtered_harnesses(self):
        workspace = fixture_workspace(Path("/nonexistent"))
        result = g.Plan(paths=["synthetic"], workspace=workspace,
                        filters=set(g.SMOKE_FILTERS) | {"future::filter"})
        runs = result.runs()
        self.assertEqual(len(runs), 17)
        self.assertTrue(all(run["filters"] == result.final_filters() for run in runs))

    def test_whole_package_override_survives_filter_pruning(self):
        workspace = fixture_workspace(Path("/nonexistent"))
        result = g.Plan(paths=["synthetic"], workspace=workspace,
                        filters=set(g.SMOKE_FILTERS), whole_packages={"manifold-compositor"})
        run = next(run for run in result.runs() if run["package"] == "manifold-compositor")
        self.assertEqual(run["filters"], [])

    def test_whole_package_runs_still_skip_reporter_skips(self):
        workspace = fixture_workspace(Path("/nonexistent"))
        result = g.Plan(paths=["synthetic"], workspace=workspace,
                        filters=set(g.SMOKE_FILTERS), whole_packages={"manifold-compositor"})
        run = next(run for run in result.runs() if run["package"] == "manifold-compositor")
        self.assertEqual(run["filters"], [])
        for name in g.REPORTER_SKIPS:
            self.assertIn(name, run["skips"])

    def test_missing_audited_owner_fails_before_pruning(self):
        workspace = fixture_workspace(Path("/nonexistent"))
        workspace.packages["manifold-nodes-scene"]["targets"] = [
            target for target in workspace.packages["manifold-nodes-scene"]["targets"]
            if target["name"] != "gpu_proofs"
        ]
        result = g.Plan(paths=["synthetic"], workspace=workspace,
                        filters=set(g.SMOKE_FILTERS))
        with self.assertRaisesRegex(ValueError, "camera_conformance.*manifold-nodes-scene/gpu_proofs"):
            result.runs()

    def test_audited_filter_owners_match_the_source_harnesses(self):
        repo = Path(__file__).resolve().parents[1]
        workspace = g.Workspace(repo)
        for name, (package, target) in g.GPU_FILTER_TARGETS.items():
            module, _, function = name.partition('::')
            root = next(Path(row['src_path']) for row in workspace.targets(package)
                        if ('lib' in row['kind'] if target == 'lib' else row['name'] == target))
            self.assertIn(f'mod {module};', root.read_text())
            source = (root.parent / (module + '.rs')).read_text()
            self.assertIn(f'fn {function}(' if function else '#[test]', source)

    def test_scope_report_exposes_unfiltered_package_and_target_expansion(self):
        result = g.Plan(paths=[R + '../Cargo.toml'],
                        whole_packages={'manifold-nodes'},
                        required_binaries={('manifold-app', 'renderer_contracts')})
        report = result.describe()
        self.assertIn('whole packages (filters do not limit these): manifold-nodes', report)
        self.assertIn('whole test binaries: manifold-app/renderer_contracts', report)

    def test_every_primitive_directory_selects_both_catalog_layout_proofs(self):
        import cpu_scope
        from gate_policy import PRIMITIVE_PATHS, CATALOG_PATHS
        root = Path(__file__).resolve().parents[1]
        directories = {str(p.relative_to(root)) + '/'
                       for p in (root / 'crates').rglob('primitives')
                       if p.is_dir() and 'src' in p.relative_to(root).parts
                       and any(p.rglob('*.rs'))}
        self.assertEqual(set(PRIMITIVE_PATHS), directories)
        workspace = fixture_workspace(Path('/nonexistent'))
        for directory in sorted(directories):
            self.assertIn(directory, CATALOG_PATHS)
            for suffix in ('.rs', '.wgsl'):
                source = next((p for p in sorted((root / directory).rglob('*' + suffix))
                               if p.name != 'mod.rs'), None)
                if source is None:
                    continue  # rigid primitives own no WGSL
                with self.subTest(source=str(source.relative_to(root))):
                    selected = cpu_scope.plan_for_paths(
                        [source.relative_to(root).as_posix()], Path('/nonexistent'), workspace)
                    for module in ('uniform_layout_proof', 'uniform_layout_extended'):
                        self.assertIn(f'(package(=manifold-nodes) & test(/^{module}::/))', selected.filters)
                        self.assertIn(f'mod {module};', (root / 'crates/manifold-nodes/tests/main.rs').read_text())

    def test_whole_crate_deletion_keeps_base_ownership_in_nested_cpu_plan(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            old = 'crates/retired-catalog'
            deleted = [old + '/Cargo.toml', old + '/src/lib.rs',
                       old + '/tests/gpu_proofs/proof.rs']
            current = 'crates/manifold-app/tests/renderer_contracts/gpu_proofs/proof.rs'
            contents = {
                'Cargo.toml': '[workspace]\nmembers = ["crates/retired-catalog", "crates/manifold-app"]\n',
                deleted[0]: '[package]\nname = "retired-catalog"\nversion = "0.1.0"\n',
                deleted[1]: '', deleted[2]: '#[test]\nfn old_proof() {}\n',
                'crates/manifold-app/Cargo.toml': '[package]\nname = "manifold-app"\nversion = "0.1.0"\n',
            }
            for path, text in contents.items():
                file = repo / path
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text(text)
            def git(*args):
                return subprocess.run(['git', '-C', str(repo), '-c', 'user.name=Scope Test',
                                       '-c', 'user.email=scope@example.invalid',
                                       '-c', 'core.hooksPath=/dev/null', *args],
                                      check=True, capture_output=True, text=True)
            git('init', '-q')
            git('add', '--', *contents)
            git('commit', '-qm', 'Base workspace with catalog')
            shutil.rmtree(repo / old)
            (repo / 'Cargo.toml').write_text('[workspace]\nmembers = ["crates/manifold-app"]\n')
            proof = repo / current
            proof.parent.mkdir(parents=True, exist_ok=True)
            proof.write_text('#[test]\nfn moved_proof() {}\n')
            workspace = fixture_workspace(repo)
            result = g.plan_for_paths(deleted + [current], repo, base='HEAD', workspace=workspace)
            self.assertEqual(result.paths, [current])
            self.assertFalse(result.unmapped)
            self.assertTrue(any(run['package'] == 'manifold-app'
                                and run['target'] == 'renderer_gpu_proofs' for run in result.runs()))
            with self.assertRaisesRegex(ValueError, 'no current or base Cargo workspace owner'):
                g.plan_for_paths(['crates/never-owned/src/lib.rs'], repo, base='HEAD', workspace=workspace)

    def test_moved_contract_harnesses_and_nested_proofs_keep_gpu_ownership(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            workspace = fixture_workspace(repo)
            for package, target in g.GPU_CONTRACT_TARGETS.items():
                harness = repo / f"crates/{package}/tests/{target}.rs"
                proof = harness.parent / 'contracts/proof.rs'
                proof.parent.mkdir(parents=True)
                harness.write_text('mod contracts;\n')
                (proof.parent / 'mod.rs').write_text('mod proof;\n')
                proof.write_text('#[cfg(feature = "gpu-proofs")]\n#[test]\nfn value_proof() {}\n')
                path = proof.relative_to(repo).as_posix()
                selected = g.plan_for_paths([path], repo, workspace=workspace, cpu_plan=None)
                self.assertTrue(selected.active)
                self.assertFalse(selected.unmapped)
                run = next(row for row in selected.runs() if (row['package'], row['target']) == (package, target))
                self.assertIn('contracts::proof::', run['filters'])
            nested = 'crates/manifold-app/tests/renderer_contracts/gpu_proofs/liquid_conformance.rs'
            self.assertTrue(g.is_gpu_path(nested.replace('.rs', '.json'), workspace))
            selected = g.plan_for_paths([nested], repo, workspace=workspace, cpu_plan=None)
            self.assertTrue(selected.active)
            self.assertFalse(selected.unmapped)
            run = next(row for row in selected.runs() if (row['package'], row['target']) == ('manifold-app', 'renderer_gpu_proofs'))
            self.assertIn('liquid_conformance::', run['filters'])

    def test_retired_crate_proof_requires_confirmed_git_deletion(self):
        path = "crates/retired-catalog/tests/gpu_proofs/proof.rs"
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            (repo / '.git').touch()
            workspace = fixture_workspace(repo)
            for output, expected in [(path + '\n', []), ('', [path])]:
                with self.subTest(deleted=bool(output)), mock.patch.object(
                    g.subprocess, 'run', return_value=mock.Mock(returncode=0, stdout=output, stderr='')
                ):
                    result = g.plan_for_paths([path], repo, shader_users=lambda _: [], workspace=workspace,
                                              cpu_plan=None)
                    self.assertEqual([item[0] for item in result.unmapped], expected)

    def test_deleted_contract_test_requires_confirmed_git_deletion(self):
        path = "crates/manifold-nodes/tests/contracts/water/race_probe.rs"
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            (repo / '.git').touch()
            workspace = fixture_workspace(repo)
            for output, expected in [(path + '\n', []), ('', [path])]:
                with self.subTest(deleted=bool(output)), mock.patch.object(
                    g.subprocess, 'run', return_value=mock.Mock(returncode=0, stdout=output, stderr='')
                ):
                    result = g.plan_for_paths([path], repo, shader_users=lambda _: [], workspace=workspace,
                                              cpu_plan=None)
                    self.assertEqual([item[0] for item in result.unmapped], expected)

    def test_path_attr_filter_finds_testkit_visible_mount(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            mount = repo / E / "fixture" / "mod.rs"
            mount.parent.mkdir(parents=True)
            mount.write_text(
                'manifold_core::testkit_visible! {\n'
                '    #[path = "tests/wrapped.rs"] mod wrapped;\n'
                '}\n'
            )
            path = E + "fixture/tests/wrapped.rs"
            self.assertEqual(g.path_attr_filters(path, repo), ["fixture::wrapped::"])

    def test_p2_catalog_contracts_follow_leaf_sources(self):
        cases = {
            "crates/manifold-compositor/src/layer_compositor.rs": ["layer_compositor"],
            "crates/manifold-compositor/src/preset_thumbnail.rs": ["preset_thumbnail"],
            "crates/manifold-nodes-scene/src/node_graph/scene_modifier_legacy_migration/loop_upgrade.rs": ["loop_upgrade"],
            "crates/manifold-nodes-scene/src/node_graph/gltf_import/assembly.rs":
                ["gltf_import", "gltf_card_precedence", "gltf_upgrade", "gltf_upgrade_project"],
        }
        import cpu_scope
        for path, modules in cases.items():
            with self.subTest(path=path):
                workspace = fixture_workspace(Path("/nonexistent"))
                cpu = cpu_scope.plan_for_paths([path], Path("/nonexistent"), workspace)
                gpu = plan([path])
                for module in modules:
                    prefix = "node_graph::catalog_tests::" + module + "::"
                    self.assertIn(prefix, cpu.filterset)
                    self.assertIn(prefix, gpu.filters)
                if path.startswith(("crates/manifold-compositor/src/",
                                    "crates/manifold-nodes-scene/src/")):
                    self.assertNotIn(gpu.workspace.owner(path), gpu.whole_packages)
        gltf = plan(["crates/manifold-nodes-scene/src/node_graph/gltf_import/assembly.rs"])
        self.assertTrue({"render_scene_material_upgrade::", "rt_bug318_import_toggle::",
                         "rt_bug326_fix_gate::", "rt_bugmajv_kernel_toggle::",
                         "rt_normal_tangent_mirror::", "rt_r3_heldout_gltf::"}.issubset(gltf.filters))
        for owner in ("manifold-nodes-image", "manifold-nodes-scene"):
            path = f"crates/{owner}/src/node_graph/primitives/mod.rs"
            cpu = cpu_scope.plan_for_paths([path], Path("/nonexistent"), fixture_workspace(Path("/nonexistent")))
            self.assertIn("binary(=main)", cpu.filterset)

    def test_mapped_sources_keep_local_module_proofs_without_whole_package(self):
        cases = {
            "crates/manifold-compositor/src/layer_compositor.rs": "layer_compositor::",
            "crates/manifold-compositor/src/preset_thumbnail.rs": "preset_thumbnail::",
            "crates/manifold-compositor/src/generator_renderer.rs": "generator_renderer::",
            "crates/manifold-nodes-scene/src/node_graph/scene_modifier_legacy_migration/loop_upgrade.rs":
                "node_graph::scene_modifier_legacy_migration::loop_upgrade::",
        }
        for path, local in cases.items():
            with self.subTest(path=path):
                result = plan([path])
                self.assertIn(local, result.filters)
                self.assertIn("node_graph::catalog_tests::", " ".join(result.filters))
                self.assertNotIn(result.workspace.owner(path), result.whole_packages)

    def test_mapped_source_and_broad_gpu_change_keep_broad_override(self):
        result = plan([
            "crates/manifold-compositor/src/layer_compositor.rs",
            "crates/manifold-gpu/src/metal/device.rs",
        ])
        self.assertIn("layer_compositor::", result.filters)
        self.assertTrue(set(g.BROAD_FILTERS) <= result.filters)
        self.assertIn("manifold-gpu", result.whole_packages)
        self.assertNotIn("manifold-compositor", result.whole_packages)

    def test_unmapped_source_stays_conservative(self):
        path = "crates/manifold-compositor/src/unmapped_source.rs"
        result = plan([path])
        self.assertIn("manifold-compositor", result.whole_packages)
        self.assertNotIn("unmapped_source::", result.filters)

    def test_all_p2_extraction_rows_select_catalog_home(self):
        import cpu_scope
        for prefix, module, cpu_expected in g.CATALOG_TEST_ROWS:
            self.assertTrue(list(Path(__file__).resolve().parents[1].glob(prefix + "*")), prefix)
            path = prefix + ("mod.rs" if prefix.endswith("/") else ".rs")
            with self.subTest(path=path, module=module):
                gpu = plan([path])
                self.assertIn("node_graph::catalog_tests::" + module + "::", gpu.filters)
                if cpu_expected:
                    cpu = cpu_scope.plan_for_paths([path], Path("/nonexistent"), fixture_workspace(Path("/nonexistent")))
                    self.assertIn("node_graph::catalog_tests::" + module + "::", cpu.filterset)

    def test_moved_shared_harness_keeps_broad_and_readback_consumers(self):
        selected = plan(["crates/manifold-nodes-scene/src/testkit/gpu_harness.rs"])
        self.assertFalse(selected.unmapped)
        self.assertTrue(set(g.BROAD_FILTERS).issubset(selected.filters))
        self.assertTrue({"rt_t2b_temporal_wiring::", "rt_bug318_import_toggle::",
                         "rt_bugmajv_kernel_toggle::"}.issubset(selected.filters))
        self.assertTrue(selected.broad)


    def test_moved_proof_uses_metadata_owner_and_module_scope(self):
        path = "crates/manifold-nodes-scene/tests/gpu_proofs/rt_t2b_temporal_wiring.rs"
        selected = plan([path])
        self.assertFalse(selected.unmapped)
        self.assertIn("rt_t2b_temporal_wiring::", selected.filters)
        run = next(row for row in selected.runs()
                   if row["package"] == "manifold-nodes-scene" and row["target"] == "gpu_proofs")
        self.assertIn("rt_t2b_temporal_wiring::", run["filters"])

    def test_moved_proof_root_selects_its_metadata_target(self):
        selected = plan(["crates/manifold-nodes-scene/tests/gpu_proofs/main.rs"])
        self.assertFalse(selected.unmapped)
        self.assertIn(("manifold-nodes-scene", "gpu_proofs"), selected.required_binaries)


    def test_image_legacy_heightfield_shader_routes_to_owning_primitive(self):
        selected = plan(["crates/manifold-nodes-image/src/node_graph/primitives/shaders/heightfield_shadow.wgsl"])
        self.assertIn("node_graph::primitives::heightfield_shadow::", selected.filters)


    def folded_glb_workspace(self, repo, module='glb_conformance'):
        workspace = fixture_workspace(repo)
        renderer = workspace.packages['manifold-nodes']
        standalone = next(t for t in renderer['targets'] if t['name'] == 'glb_conformance')
        renderer['targets'].remove(standalone)
        owner = next(target for target in renderer['targets'] if target['name'] == 'gpu_proofs')
        catalog_source = owner['src_path']
        owner['src_path'] = str(repo.resolve() / 'crates/manifold-nodes/tests/gpu_proofs/owner_main.rs')
        scene = dict(renderer, name='manifold-nodes', targets=[{
            'name': 'catalog_gpu_checks', 'kind': ['test'], 'required-features': ['gpu-proofs'],
            'src_path': catalog_source,
        }, *renderer['targets']])
        workspace.packages[scene['name']] = scene
        workspace.roots[scene['name']] = 'crates/manifold-nodes'
        root = Path(next(target['src_path'] for target in scene['targets']
                         if target['name'] == 'catalog_gpu_checks'))
        root.parent.mkdir(parents=True)
        attribute = '#[path = "glb_conformance.rs"]\n' if module != 'glb_conformance' else ''
        root.write_text(f'{attribute}mod {module};\nmod other;\n')
        (root.parent / 'owner_main.rs').write_text('mod other;\n')
        (root.parent / 'glb_conformance.rs').write_text('#[test] fn glb_conformance_sweep() {}\n')
        return workspace

    def test_standalone_glb_stays_with_metadata_discovered_catalog_owner(self):
        workspace = fixture_workspace(Path("/nonexistent"))
        target = next(t for t in workspace.packages["manifold-nodes"]["targets"]
                      if t["name"] == "glb_conformance")
        target["name"] = "catalog_glb_checks"
        target["src_path"] = str(workspace.repo / "crates/manifold-nodes/tests/gpu_proofs/glb_conformance.rs")
        self.assertEqual(g.glb_conformance_route(workspace),
                         ("manifold-nodes", "catalog_glb_checks", ""))
        result = g.Plan(paths=["gltf"], glb=True, workspace=workspace)
        self.assertEqual(result.runs()[-1]["package"], "manifold-nodes")
        self.assertEqual(result.runs()[-1]["targets"], ["catalog_glb_checks"])
        self.assertFalse(result.runs()[-1]["budgeted"])
        self.assertEqual(sum("catalog_glb_checks" in run["targets"] for run in result.runs()), 1)
        result.glb = False
        self.assertFalse(any("catalog_glb_checks" in run["targets"] for run in result.runs()))

    def test_folded_glb_uses_discovered_owner_target_and_prefix(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace = self.folded_glb_workspace(Path(directory))
            result = g.Plan(paths=['gltf'], glb=True, workspace=workspace)
            for whole, required in ((set(), set()), ({'manifold-nodes'}, set()),
                                    (set(), {('manifold-nodes', 'catalog_gpu_checks')})):
                result.whole_packages, result.required_binaries = whole, required
                runs = result.runs()
                self.assertEqual(runs[-1], {
                    'package': 'manifold-nodes', 'targets': ['catalog_gpu_checks'],
                    'lib': False, 'target': 'catalog_gpu_checks', 'filters': ['glb_conformance::'],
                    'skips': [], 'budgeted': False,
                })
                regular = next(r for r in runs if r['package'] == 'manifold-nodes'
                               and r['target'] == 'catalog_gpu_checks' and r['budgeted'])
                self.assertIn('glb_conformance::', regular['skips'])
            result.glb = False
            self.assertTrue(all(r['budgeted'] for r in result.runs()))
            regular = next(r for r in result.runs()
                           if r['package'] == 'manifold-nodes'
                           and r['target'] == 'catalog_gpu_checks')
            self.assertIn('glb_conformance::', regular['skips'])

    def test_folded_glb_resolves_module_alias(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace = self.folded_glb_workspace(Path(directory), module='conformance')
            self.assertEqual(g.glb_conformance_route(workspace),
                             ('manifold-nodes', 'catalog_gpu_checks', 'conformance::'))

    def test_glb_missing_or_ambiguous_target_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace = self.folded_glb_workspace(Path(directory))
            root = Path(workspace.packages['manifold-nodes']['targets'][0]['src_path'])
            root.write_text('mod other;\n')
            result = g.Plan(paths=['gltf'], glb=True, workspace=workspace)
            with self.assertRaisesRegex(ValueError, 'no glb_conformance'):
                result.runs()
            root.write_text('mod glb_conformance;\n')
            duplicate = dict(workspace.packages['manifold-nodes']['targets'][0], name='duplicate')
            workspace.packages['manifold-nodes']['targets'].append(duplicate)
            with self.assertRaisesRegex(ValueError, 'ambiguous glb_conformance'):
                result.runs()

    def test_moved_lattice_shaders_select_concrete_fill_pits_proofs(self):
        repo = Path(__file__).resolve().parent.parent
        names = g.read_times(repo / "scripts/gpu_test_times.json")
        for shader in ("offset_lattice_body.wgsl", "redistance_lattice_body.wgsl"):
            path = LW + "shaders/" + shader
            result = plan([path], repo=repo, users=lambda _: [LW + shader.replace("_body.wgsl", ".rs")])
            self.assertIn("fluid_fill_pits", result.filters)
            relevant = [name for name in names if "fluid_fill_pits" in name]
            self.assertTrue(relevant, "the owning proof inventory must be nonempty")
            self.assertTrue(all(any(f in name for f in result.filters) for name in relevant))
            self.assertFalse(result.unmapped)

    def test_repathed_shader_rows_select_existing_owning_proofs(self):
        repo = Path(__file__).resolve().parent.parent
        # Reviewed allowances are a timing sample, not a complete test inventory.
        names = set()
        for source in [*(repo / W).rglob("*.rs"), *(repo / FW).rglob("*.rs")]:
            lines = len(source.read_text().splitlines())
            names.update(g.changed_test_filters(source.relative_to(repo).as_posix(), repo,
                         "HEAD", patch=f"@@ -0,0 +1,{lines} @@"))
        cases = {
            "liquid_fill": "gpu_flip_",
            "face_sample_component": "face_grid_tests::",
            "count_surface_edges": "count_surface_edges::gpu_tests::",
            "surface_edge_": "volume_surface_mesh::gpu_tests::",
            "volume_surface_mesh": "volume_surface_mesh::gpu_tests::",
            "relax_surface_mesh": "volume_surface_mesh::gpu_tests::",
            "surface_mesh_": "volume_surface_mesh::gpu_tests::",
            "grid_to_matter": "matter_",
            "push_out_of_solid": "push_out_of_solid::gpu_tests::",
            "liquid_frame_faces": "liquid_frame::gpu_tests::",
        }
        for prefix, owning in cases.items():
            paths = [*(repo / W / "shaders").glob(prefix + "*.wgsl"),
                     *(repo / LW / "shaders").glob(prefix + "*.wgsl"),
                     *(repo / FW / "shaders").glob(prefix + "*.wgsl")]
            self.assertTrue(paths, prefix)
            relevant = [name for name in names if owning in name]
            self.assertTrue(relevant, f"{prefix}: owning proof inventory is empty")
            for path in paths:
                result = plan([path.relative_to(repo).as_posix()], repo=repo,
                              users=lambda shader: g.default_shader_users(repo, shader))
                self.assertTrue(any(owning in f or f in owning for f in result.filters), (path, result.filters))
                self.assertTrue(all(any(f in name for f in result.filters) for name in relevant))
                self.assertFalse(result.unmapped)

    def test_water_pressure_fixtures_select_their_consuming_proofs(self):
        for name in ("dambreak_pressure_problems.bin.zst", "deep_pool_pressure_problems.bin.zst",
                     "deep_pool_density_problems.bin.zst", "gpu_flip_pressure_golden.txt"):
            path = "crates/manifold-water-gpu-flip/tests/fixtures/" + name
            result = plan([path])
            self.assertEqual(result.paths, [path])
            self.assertEqual(result.filters, {"primitives::gpu_flip_pressure_tests::"})
            self.assertFalse(result.unmapped)

    def test_contract_mounts_select_real_module_names(self):
        repo = Path(__file__).resolve().parent.parent
        path = "crates/manifold-nodes/tests/contracts/freeze/install.rs"
        result = plan([path], repo=repo)
        self.assertIn(path, result.paths)
        self.assertIn("contracts::freeze::install::", result.filters)
        self.assertNotIn("engine_contract_tests::freeze_install::", result.filters)
        names = g.read_times(repo / "scripts/gpu_test_times.json")
        self.assertTrue(any(key.split("/", 2)[-1].startswith("contracts::freeze::install::tests::")
                            for key in names))
        self.assertFalse(result.unmapped)

    def test_unmounted_contract_is_unmapped(self):
        path = "crates/manifold-nodes/tests/contracts/freeze/orphan.rs"
        result = plan([path], repo=self._repo_with(path))
        self.assertEqual([row[0] for row in result.unmapped], [path])

    def test_ui_paint_selects_own_lib_proofs_and_renderer_smoke(self):
        result = plan(["crates/manifold-ui-paint/src/native_text.rs"])
        self.assertFalse(result.unmapped)
        renderer = next(run for run in result.runs()
                        if run.get("package") == "manifold-nodes" and run["target"] == "gpu_proofs")
        self.assertEqual(renderer["filters"], sorted(g.SMOKE_FILTERS))
        self.assertEqual({(run['package'], run['target']) for run in result.runs()}, {
            ('manifold-nodes', 'gpu_proofs'), ('manifold-nodes-scene', 'gpu_proofs'),
            ('manifold-ui-paint', 'lib'), ('manifold-ui-paint', 'main'),
        })
        paint = next(run for run in result.runs()
                     if run.get("package") == "manifold-ui-paint" and run["target"] == "lib")
        self.assertEqual(paint["package"], "manifold-ui-paint")
        self.assertTrue(paint["lib"])
        self.assertEqual(paint["targets"], [])
        self.assertEqual(paint["filters"], g.UI_PAINT_FILTERS)
        contracts = next(run for run in result.runs()
                         if run["package"] == "manifold-ui-paint" and run["target"] == "main")
        self.assertFalse(contracts["lib"])
        self.assertIn("contracts::", contracts["filters"])

    def test_gpu_core_also_selects_ui_paint(self):
        result = plan(["crates/manifold-gpu/src/testkit.rs"])
        self.assertTrue(result.ui_paint)

    def setUp(self):
        self.real_learned_times_path = g.learned_times_path
        self.enterContext(mock.patch.object(g, "learned_times_path", return_value=None))

    def test_step_order_cpu_reference_selects_gpu_value_proofs(self):
        path = FW + 'gpu_flip_extension_tests.rs'
        result = plan([path], repo=self._repo_with(path))
        self.assertIn("gpu_flip_step_order_", result.filters)
        self.assertIn("gpu_flip_extend_faces_", result.filters)
        self.assertFalse(result.unmapped)
    def test_mesh_grid_sources_select_native_value_proofs(self):
        for path in (LT + 'lattice.rs', W + 'liquid_frame.rs',
                     FW + 'liquid_solid_distance.rs', FW + 'shaders/liquid_solid_distance_body.wgsl'):
            result = plan([path], users=lambda _: [FW + 'liquid_solid_distance.rs'],
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
                base = LW if name.rsplit("/", 1)[-1].startswith(("particle_identity", "particle_publication")) else W
                source = base + name.rsplit("/", 1)[-1].replace(".wgsl", ".rs")
                result = plan([base + name], users=lambda _: [source],
                              repo=self._repo_with(base + name))
                self.assertTrue(required <= result.filters)
                self.assertFalse(result.unmapped)
                self.assertFalse(result.broad)

    def test_live_clock_and_duration_atoms_select_value_proofs(self):
        for name in ("gpu_flip_clock.rs", "shaders/gpu_flip_clock.wgsl"):
            result = plan([FW + name], users=lambda _: [FW + 'gpu_flip_clock.rs'],
                          repo=self._repo_with(FW + name))
            self.assertIn("gpu_flip_clock::gpu_tests::", result.filters)
            self.assertNotIn("gpu_flip_", result.filters)
            self.assertFalse(result.unmapped)
        for name in ("emission_count.rs", "spawn_whitewater.rs",
                     "shaders/emission_count_body.wgsl", "shaders/spawn_whitewater_body.wgsl"):
            result = plan([W + name], users=lambda _: [W + 'emission_count.rs'],
                          repo=self._repo_with(W + name))
            self.assertIn("whitewater_particle_tests::", result.filters)
            self.assertFalse(result.unmapped)

    def test_blob_bounds_selects_dense_and_sparse_consumers(self):
        for path in (W + "blob_bounds.rs", W + "shaders/blob_bounds.wgsl"):
            result = plan([path], users=lambda _: [W + "blob_bounds.rs"],
                          repo=self._repo_with(path))
            self.assertTrue({"primitives::blob_bounds::",
                             "liquid_surface_tests::",
                             "liquid_bricks::tests::gpu_tests::"} <= result.filters)
            self.assertFalse(result.unmapped)

    def test_narrow_band_isolated_passes_select_their_value_proofs(self):
        for path in (FW + 'gpu_flip_narrow_band_tests.rs',
                     FW + 'gpu_flip_narrow_band.rs',
                     FW + 'shaders/gpu_flip_narrow_band.wgsl'):
            result = plan([path], users=lambda _: [FW + 'gpu_flip_narrow_band_tests.rs'],
                          repo=self._repo_with(path))
            self.assertTrue(set(g.SMOKE_FILTERS + ["narrow_band"]) <= set(result.final_filters()))
            self.assertNotIn("gpu_flip_", result.filters)
            self.assertFalse(result.broad)
            self.assertFalse(result.unmapped)

    def test_whitewater_emitters_select_shared_value_and_fusion_proofs(self):
        expected = "primitives::whitewater_emitter_gpu_tests::"
        for atom in ("turbulence_field", "inside_turbulence_potential",
                     "turbulence_emission_count", "whitewater_emitter_velocity",
                     "whitewater_obstacle_source", "whitewater_influence", "dust_potential"):
            source = W + atom + ".rs"
            shader = W + "shaders/" + atom + "_body.wgsl"
            for path in (source, shader):
                result = plan([path], users=lambda _: [source], repo=self._repo_with(path))
                self.assertIn(expected, result.filters)
                self.assertFalse(result.unmapped)
                self.assertFalse(result.broad)

    def test_whitewater_step_and_its_fused_shader_run_the_golden_fingerprints(self):
        step = W + 'whitewater_step.rs'
        shader = W + 'shaders/whitewater_fused.wgsl'
        for path in (step, shader):
            result = plan([path], users=lambda _: [step], repo=self._repo_with(path))
            self.assertIn("contracts::water::primitives::whitewater_golden_tests::", result.filters)
            self.assertIn("contracts::water::primitives::whitewater_step::", result.filters)
            self.assertIn("primitives::whitewater_step_tests::", result.filters)
            self.assertFalse(result.unmapped)

    def test_water_sources_and_shaders_keep_module_scope(self):
        source = LW + "running_total.rs"
        shader = LW + "shaders/running_total.wgsl"
        for path in (source, shader):
            result = plan([path], users=lambda _: [source], repo=self._repo_with(path))
            self.assertEqual(result.filters, {"primitives::running_total::", *WATER_BROAD_FILTERS})
            self.assertFalse(result.whole_packages)
            self.assertFalse(result.unmapped)
            water = next(run for run in result.runs() if run['package'] == 'manifold-water-liquid')
            self.assertIn("primitives::running_total::", water['filters'])
        root = plan([T + "lib.rs"])
        self.assertEqual(root.whole_packages, {"manifold-nodes-water"})

    def test_liquid_root_runs_the_cs_liquid_suite_not_everything(self):
        result = plan([LT + "lib.rs"])
        self.assertEqual(result.filters, set(WATER_BROAD_FILTERS))
        self.assertFalse(result.whole_packages)
        self.assertFalse(result.broad)
        self.assertFalse(result.unmapped)

    def test_rigid_sources_run_their_module_and_the_cs_rigid_suite(self):
        result = plan(["crates/manifold-water-rigid/src/physics.rs"])
        self.assertEqual(result.filters, {"physics::", "liquid_conformance::liquid_coupled_",
                                          "physics_boxes::", "physics_solids::"})
        self.assertFalse(result.whole_packages)
        self.assertFalse(result.unmapped)

    def test_non_gpu_paths_run_nothing(self):
        p = plan(["docs/X.md", "scripts/a.py", "crates/manifold-ui/src/lib.rs"])
        self.assertFalse(p.active)
        self.assertEqual(p.runs(), [])

    def test_primitive_maps_to_its_own_proofs_plus_smoke(self):
        p = plan([P + "gain.rs"])
        self.assertEqual(p.final_filters(), sorted(g.SMOKE_FILTERS + ["primitives::gain::"]))
        self.assertFalse(p.glb)
        self.assertEqual(p.broad, [])

    def test_primitive_gpu_tests_file_maps_to_parent_primitive(self):
        filters = g.module_filters(P + "fixture_gpu_tests.rs")
        self.assertIn("primitives::fixture::", filters)

    def test_primitive_directory_maps_to_dir_module(self):
        p = plan([P + "gain/extent.rs"])
        self.assertIn("primitives::gain::extent::", p.filters)

    def test_primitive_shader_maps_through_its_user(self):
        p = plan([P + "shaders/gain.wgsl"], users=lambda s: [P + "gain.rs"],
                 repo=self._repo_with(P + "shaders/gain.wgsl"))
        self.assertIn("primitives::gain::", p.filters)
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
        runs = p.runs()
        expected = sum(bool(p.workspace.targets(package, "lib"))
                       + len([target for target in p.workspace.targets(package, "test")
                              if "gpu-proofs" in target.get("required-features", [])
                              and target["name"] not in g.GLB_TESTS
                              and target["name"] != "glb_conformance"])
                       for package in p.workspace.feature_packages("gpu-proofs"))
        # The ordinary UI, app and catalog harnesses also own device proofs.
        self.assertEqual(len(runs), expected + 3)
        self.assertIn("manifold-ui-paint", {run["package"] for run in runs})

    def test_broad_set_is_bounded(self):
        # Never a bare gpu_proofs/lib sweep: every filter names something specific.
        for f in g.BROAD_FILTERS + g.SMOKE_FILTERS:
            self.assertGreaterEqual(len(f), len("freeze::"))

    def test_freeze_and_runtime(self):
        p = plan([E + 'freeze/codegen/fused.rs'])
        self.assertIn("freeze::", p.filters)
        p = plan([E + 'exec/execution/foo.rs'])
        self.assertTrue(set(g.RUNTIME_FILTERS) <= p.filters)

    def test_rt_row_keeps_union_with_freeze_without_muting_particletext(self):
        p = plan(["crates/manifold-gpu/src/metal/raytrace.rs", E + 'freeze/x.rs'])
        self.assertTrue({"rt_", "freeze::"} <= p.filters)
        self.assertEqual(sorted(set(p.final_skips()) - {n for n, _ in g.slow_tests()}), [])
        self.assertEqual(p.broad, [])

    def test_skip_dropped_when_it_would_hide_a_selected_filter(self):
        p = plan(["crates/manifold-gpu/src/metal/raytrace.rs", "crates/manifold-nodes-image/src/node_graph/primitives/particletext.rs"])
        self.assertEqual(sorted(set(p.final_skips()) - {n for n, _ in g.slow_tests()}), [])

    def test_matter_row(self):
        p = plan([W + 'matter_fill.rs'])
        self.assertTrue({"matter_", "substeps_"} <= p.filters)

    def test_gpu_flip_row_reaches_the_scene_proofs(self):
        for path in (FW + 'gpu_flip_step.rs', FW + 'liquid_state.rs', LT + 'extent.rs'):
            self.assertTrue({"gpu_flip_", "face_grid_tests::"} <= plan([path]).filters, path)
        shader = FW + 'shaders/gpu_flip_step.wgsl'
        p = plan([shader], users=lambda s: [FW + 'gpu_flip_step.rs'], repo=self._repo_with(shader))
        self.assertIn("gpu_flip_", p.filters)

    def test_clock_and_fields_get_force_proofs_not_body_or_step(self):
        for path in (LT + 'clock.rs', LT + 'fields.rs',
                     LT + 'fields/tests.rs'):
            p = plan([path])
            self.assertIn("gpu_flip_face_gravity", p.filters, path)
            self.assertNotIn("gpu_flip_", p.filters, path)
            self.assertFalse(any(f.startswith("gpu_flip_body") for f in p.filters), path)

    def test_domain_nodes_are_narrow(self):
        p = plan([FW + 'gpu_flip_domain.rs', W + 'matter_domain.rs'])
        self.assertIn("gpu_flip_domain_", p.filters)
        self.assertIn("matter_scene::", p.filters)
        self.assertNotIn("gpu_flip_", p.filters)
        self.assertNotIn("matter_", p.filters)

    def test_body_step_and_pressure_paths_still_pull_the_body_proofs(self):
        for path in (FW + 'gpu_flip_bodies.rs', FW + 'gpu_flip_body_tests.rs',
                     FW + 'gpu_flip_step.rs', FW + 'gpu_flip_pressure.rs',
                     LT + 'bodies.rs', LT + 'coupling.rs'):
            self.assertIn("gpu_flip_", plan([path]).filters, path)

    def test_gated_sort_scan_and_inverse_pull_the_inactive_slot_proof(self):
        name = "gpu_flip_inactive_slots_match_the_ungated_step"
        for path in (LW + 'sort_particles_into_cells.rs', LW + 'prefix_scan.rs'):
            p = plan([path])
            self.assertIn(name, p.filters, path)
            self.assertNotIn("gpu_flip_", p.filters, path)
        for path, user in ((LW + 'shaders/prefix_scan.wgsl', LW + 'prefix_scan.rs'),
                           (LW + 'shaders/sort_particles_into_cells.wgsl', LW + 'sort_particles_into_cells.rs'),
                           (FW + 'shaders/coarse_inverse.wgsl', FW + 'gpu_flip_pressure.rs')):
            p = plan([path], users=lambda _, u=user: [u], repo=self._repo_with(path))
            self.assertIn(name, p.filters, path)

    def test_sort_and_scan_pull_the_sort_oracle_proof(self):
        wanted = {"sort_particles_into_cells::gpu_tests::", "fluid_sort_particles_into_cells_",
                  "gpu_flip_step_order_cell_cap_compacts_preserving_ids"}
        for path in (LW + 'sort_particles_into_cells.rs', W + 'sort_particles_into_cells_gpu_tests.rs',
                     LW + 'prefix_scan.rs'):
            self.assertTrue(wanted <= plan([path]).filters, path)
        for path, user in ((LW + 'shaders/sort_particles_into_cells.wgsl', LW + 'sort_particles_into_cells.rs'),
                           (LW + 'shaders/prefix_scan.wgsl', LW + 'prefix_scan.rs')):
            p = plan([path], users=lambda _, u=user: [u], repo=self._repo_with(path))
            self.assertTrue(wanted <= p.filters, path)

    def test_mixed_diff_keeps_the_broad_row_whole(self):
        p = plan([LT + 'clock.rs', FW + 'gpu_flip_step.rs'])
        self.assertIn("gpu_flip_", p.filters)

    def test_long_proofs_skip_unless_their_own_test_is_named(self):
        for name in g.REPORTER_SKIPS:
            self.assertIn(name, plan([FW + 'gpu_flip_step.rs']).final_skips() +
                          plan([W + 'matter_fill.rs']).final_skips())
        own = plan([W + 'matter_fill.rs'])
        own.filters.add("matter_look::matter_look_volume_drift")
        self.assertNotIn("matter_look_volume_drift", own.final_skips())

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
        p = plan([W + 'matter_fill.rs'])
        # Timing is a warning only; an owning filter is never dropped.
        self.assertNotIn("a::slow", p.final_skips())
        self.assertNotIn("a::fast", p.final_skips())
        self.assertNotIn("a::exact", p.final_skips())
        p.filters.add("a::")
        self.assertNotIn("GPU-PROOFS DEFERRED", p.describe())
        self.assertEqual(p.runs()[0]["skips"], p.final_skips())

    def test_glb_sweep_time_never_skips_or_reports_the_sweep(self):
        self.with_times({"glb_conformance_sweep": 930.0, "a::slow": 61.0})
        p = plan(["crates/manifold-nodes-scene/src/node_graph/gltf_import/mod.rs"])
        self.assertTrue(p.glb)
        self.assertNotIn("glb_conformance_sweep", p.final_skips())
        self.assertNotIn("glb_conformance_sweep", p.describe())
        self.assertEqual(p.runs()[-1]["skips"], [])
        self.assertNotIn("a::slow", p.final_skips())

    def test_test_missing_from_times_file_runs(self):
        self.with_times({"a::slow": 500.0})
        self.assertNotIn("brand::new_test", plan([W + 'matter_fill.rs']).final_skips())

    def test_no_row_names_a_measured_slow_test(self):
        # A name in a row selects past the deferral; slow proofs are nightly
        # and changed-body only (every water landing paid ~4 minutes for one).
        for row in g.NARROW_ROWS + g.EXPLICIT_ROWS:
            for name in row[1][0]:
                self.assertLessEqual(g.read_times(g.TIMES_PATH).get(name, 0), g.SLOW_THRESHOLD_S, name)

    def test_slow_exact_filter_runs(self):
        name = "liquid_conformance::liquid_floating_rest"
        self.with_times({name: 222})
        p = plan([LT + 'clock.rs'])
        p.filters.add(name)
        self.assertIn(name, p.filters)
        self.assertNotIn(name, p.final_skips())

    def test_changed_bodies_are_exact_but_helper_edits_stay_module_wide(self):
        path = g.PROOFS_DIR + "liquid_conformance.rs"
        repo = self._repo_with(path)
        (repo / path).write_text(
            "fn helper() {\n    shared();\n}\n"
            "#[test]\nfn slow() {\n    old();\n}\n"
            "mod nested {\n    #[test]\n    fn slower() {\n        old();\n    }\n}\n")
        self.with_times({"liquid_conformance::slow": 100,
                         "liquid_conformance::nested::slower": 200,
                         "unrelated::slow": 300})
        for hunk, expected in [("@@ -6 +6 @@", {"liquid_conformance::slow"}),
                               ("@@ -11 +11 @@", {"liquid_conformance::nested::slower"}),
                               ("@@ -6 +6,0 @@", {"liquid_conformance::slow"}),
                               ("@@ -2 +2 @@", set())]:
            patch_text = f"diff --git a/{path} b/{path}\n+++ b/{path}\n{hunk}\n"
            with mock.patch.object(g.subprocess, "run", return_value=mock.Mock(
                    returncode=0, stdout=patch_text)):
                p = plan([path], repo=repo)
            exact = {f for f in p.filters if not f.endswith("::")}
            self.assertEqual(exact, expected)
            self.assertTrue(expected.isdisjoint(p.final_skips()))
            self.assertEqual(p.deferred(), [])
            self.assertNotIn("GPU-PROOFS DEFERRED", p.describe())

    def test_result_returning_test_bodies_are_promoted(self):
        path = g.PROOFS_DIR + "liquid_conformance.rs"
        repo = self._repo_with(path)
        (repo / path).write_text("#[test]\nfn fallible() -> Result<(), String> {\n    old()\n}\n")
        with mock.patch.object(g.subprocess, "run", return_value=mock.Mock(
                returncode=0, stdout="@@ -3 +3 @@")):
            self.assertEqual(g.changed_test_filters(path, repo, "base"),
                             {"liquid_conformance::fallible"})

    def test_non_renderer_test_files_promote_nothing(self):
        path = "crates/manifold-gpu/src/queue.rs"
        repo = self._repo_with(path)
        (repo / path).write_text("#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n    }\n}\n")
        with mock.patch.object(g.subprocess, "run") as run:
            self.assertEqual(g.changed_test_filters(path, repo, "base"), set())
            run.assert_not_called()

    def test_learned_path_resolves_worktree_git_common_directory(self):
        # Exercise the real resolver independently of this suite's cache patch.
        resolver = self.real_learned_times_path
        with tempfile.TemporaryDirectory() as d:
            # macOS temp dirs sit behind the /var -> /private/var symlink,
            # and git reports the resolved path.
            root = Path(d).resolve()
            worktree = root / "slot"
            worktree.mkdir()
            common = root / "main/.git"
            admin = common / "worktrees/slot"
            admin.mkdir(parents=True)
            (common / "objects").mkdir()
            (common / "refs").mkdir()
            (common / "HEAD").write_text("ref: refs/heads/main\n")
            (admin / "HEAD").write_text("ref: refs/heads/work\n")
            (admin / "commondir").write_text("../..\n")
            (worktree / ".git").write_text(f"gitdir: {admin}\n")
            with mock.patch.object(g, "TIMES_PATH", worktree / "scripts/times.json"):
                self.assertEqual(resolver(), common / "gpu-test-times.json")

    def test_shared_measurements_replace_seed(self):
        self.with_times({"cold": 90, "seed_only": 70})
        with tempfile.TemporaryDirectory() as d:
            cache = Path(d) / "times.json"
            cache.write_text(json.dumps({"tests": {"cold": 1, "learned": 100}}))
            with mock.patch.object(g, "learned_times_path", return_value=cache):
                self.assertEqual(g.load_times(), {"cold": 1, "seed_only": 70, "learned": 100})

    def test_corrupt_shared_measurements_warn_and_run_unknown_tests(self):
        import contextlib, io
        self.with_times({"seed_only": 70})
        with tempfile.TemporaryDirectory() as d:
            cache = Path(d) / "times.json"
            for data in ['{', '{"tests": {"bad": -1}}', '{"tests": {"bad": NaN}}']:
                cache.write_text(data)
                with mock.patch.object(g, "learned_times_path", return_value=cache), \
                        contextlib.redirect_stderr(io.StringIO()) as out:
                    self.assertEqual(g.load_times(), {"seed_only": 70})
                self.assertIn("timing cache unreadable", out.getvalue())

    def test_missing_times_file_skips_nothing(self):
        with mock.patch.object(g, "TIMES_PATH", Path("/nonexistent/t.json")):
            self.assertEqual(g.slow_tests(), [])

    def test_hand_list_is_gone(self):
        self.assertFalse(hasattr(g, "NIGHTLY_ONLY"))

    def test_committed_times_file_seeds_the_known_slow_tests(self):
        names = {n for n, _ in g.slow_tests()}
        self.assertIn("manifold-nodes/gpu_proofs/matter_bodies::matter_fill_skips_colliders", names)
        self.assertIn("manifold-nodes/gpu_proofs/matter_look::matter_momentum_conserved_free_blob", names)

    def test_proof_file_maps_to_its_own_module(self):
        p = plan([g.PROOFS_DIR + "render_scene_fog.rs"])
        self.assertIn("render_scene_fog::", p.filters)
        p = plan([g.PROOFS_DIR + "water_basin/helpers.rs"])
        self.assertIn("water_basin::", p.filters)

    def test_catalog_proof_mount_keeps_original_test_prefix(self):
        repo = Path(__file__).resolve().parent.parent
        path = g.PROOFS_DIR + "catalog/rt_bug318_import_toggle.rs"
        result = plan([path], repo=repo)
        self.assertIn("rt_bug318_import_toggle::", result.filters)
        self.assertNotIn("catalog::", result.filters)
        selected = g.changed_test_filters(path, repo, "HEAD", patch="@@ -0,0 +1,99999 @@")
        self.assertTrue(selected)
        self.assertTrue(all(name.startswith("rt_bug318_import_toggle::") for name in selected))

    def test_harness_is_broad(self):
        p = plan(["crates/manifold-nodes-scene/src/testkit/gpu_harness.rs"])
        self.assertTrue(set(g.BROAD_FILTERS) <= p.filters)

    def test_glb_runs_only_for_gltf_paths(self):
        self.assertFalse(plan([P + "gain.rs"]).glb)
        self.assertFalse(plan(["crates/manifold-gpu/src/metal/device.rs"]).glb)
        for path in ["crates/manifold-nodes/tests/gpu_proofs/glb_conformance.rs",
                     "tests/fixtures/gltf/khronos/manifest.json",
                     "crates/manifold-nodes-scene/src/node_graph/gltf_import/mod.rs"]:
            p = plan([path])
            self.assertTrue(p.glb, path)
            runs = p.runs()
            self.assertEqual(runs[-1]["targets"], ["glb_conformance"])
            self.assertFalse(runs[-1]["budgeted"])
            self.assertTrue(runs[0]["budgeted"])

    def test_unknown_file_type_in_gpu_dir_is_unmapped(self):
        p = plan(["crates/manifold-nodes/tests/contracts/node_graph/something.bin"])
        self.assertEqual(p.unmapped[0][0], "crates/manifold-nodes/tests/contracts/node_graph/something.bin")

    def test_main_run_targets_lib_and_gpu_proofs_never_glb(self):
        runs = plan([P + "gain.rs"]).runs()
        run = next(run for run in runs
                   if run["package"] == "manifold-nodes" and run["target"] == "lib")
        proof = next(run for run in runs
                     if run["package"] == "manifold-nodes" and run["target"] == "gpu_proofs")
        self.assertTrue(run["lib"])
        self.assertEqual(run["targets"], [])
        self.assertFalse(proof["lib"])
        self.assertEqual(proof["targets"], ["gpu_proofs"])
        self.assertTrue(all(run.get("package") for run in runs))
        self.assertFalse(any("glb_conformance" in run["targets"] for run in runs))

    def test_preset_runtime_test_file_maps_to_its_declared_module(self):
        repo = self._repo_with(E + 'runtime/mod.rs')
        (repo / E / "runtime/mod.rs").write_text(
            '#[cfg(test)]\n#[path = "tests/layer_skin.rs"]\nmod layer_skin_tests;\n')
        p = plan([E + 'runtime/tests/layer_skin.rs'], repo=repo)
        self.assertTrue(p.active)
        self.assertIn("runtime::layer_skin_tests::", p.filters)
        self.assertTrue(p.runs()[0]["lib"])

    def test_undeclared_preset_runtime_test_file_falls_back_to_the_module(self):
        p = plan([E + 'runtime/tests/unknown.rs'])
        self.assertIn("runtime::", p.filters)

    def test_layer_skin_source_selects_its_lib_proofs(self):
        p = plan([E + 'runtime/layer_skin.rs'])
        self.assertTrue(p.active)
        self.assertTrue({"runtime::layer_skin::", "runtime::layer_skin_tests::"} <= p.filters)

    def _repo_with(self, rel):
        d = tempfile.mkdtemp()
        self.addCleanup(lambda: __import__("shutil").rmtree(d, ignore_errors=True))
        f = Path(d) / rel
        f.parent.mkdir(parents=True)
        f.write_text("")
        return Path(d)


if __name__ == "__main__":
    unittest.main()
