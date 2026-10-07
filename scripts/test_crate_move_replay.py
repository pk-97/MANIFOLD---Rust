#!/usr/bin/env python3
"""Full-tree replay proofs in an isolated synthetic Git workspace; no GPU."""
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import crate_move_replay as replay


class ReplayTests(unittest.TestCase):
    def setUp(self):
        scratch = Path(__file__).resolve().parents[1] / 'target'
        scratch.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix='test-crate-move-', dir=scratch)
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git('init', '-q')
        self.plan = self.repo / 'plans/p1'
        self.plan.mkdir(parents=True)
        config = {'version': 1, 'source_crate': 'crates/manifold-renderer',
                  'destination_crate': 'crates/manifold-node-engine',
                  'rewrite_roots': ['crates/manifold-renderer', 'crates/manifold-node-engine']}
        (self.plan / 'plan.json').write_text(json.dumps(config)+'\n')
        (self.plan / 'moves.tsv').write_text('crates/manifold-renderer/src/foo.rs\tcrates/manifold-node-engine/src/foo.rs\n')
        (self.plan / 'rewrites.tsv').write_text('manifold_renderer::primitive!::\tmanifold_node_engine::primitive!::\nmanifold_renderer::param_tooltips!::\tmanifold_node_engine::param_tooltips!::\n')
        self.original = {
            'Cargo.toml': ('100644', b'[workspace]\nmembers = []\n'),
            'crates/manifold-renderer/src/foo.rs': ('100644', b'pub const X: u32 = 7;\n'),
            'crates/manifold-renderer/src/lib.rs': ('100644', b'use crate::foo::{X, Y};\ncrate::primitive! {}\ncrate::param_tooltips! {}\n'),
            'run': ('100755', b'#!/bin/sh\nexit 0\n'),
            'link': ('120000', b'run'),
            'binary': ('100644', b'\0\xff\n'),
        }
        self.base = self.commit(self.original)

    def git(self, *args, data=None):
        env = dict(os.environ, GIT_AUTHOR_NAME='Replay Test', GIT_AUTHOR_EMAIL='replay@example.invalid',
                   GIT_COMMITTER_NAME='Replay Test', GIT_COMMITTER_EMAIL='replay@example.invalid',
                   GIT_AUTHOR_DATE='2000-01-01T00:00:00Z', GIT_COMMITTER_DATE='2000-01-01T00:00:00Z')
        return subprocess.check_output(['git', '-C', str(self.repo), *args], input=data, env=env)

    def commit(self, entries, parent=None):
        # Plumbing creates only this test repository's objects; no index or staging.
        root = {}
        for path, (mode, content) in entries.items():
            current = root
            parts = path.split('/')
            for part in parts[:-1]: current = current.setdefault(part, {})
            oid = self.git('hash-object', '-w', '--stdin', data=content).strip()
            current[parts[-1]] = (mode.encode(), oid)
        def build(node):
            records = []
            for name, value in sorted(node.items()):
                mode, oid = (b'040000', build(value)) if isinstance(value, dict) else value
                kind = b'tree' if mode == b'040000' else b'blob'
                records.append(mode+b' '+kind+b' '+oid+b'\t'+os.fsencode(name)+b'\0')
            return self.git('mktree', '-z', data=b''.join(records)).strip()
        args = ['commit-tree', build(root).decode()]
        if parent: args += ['-p', parent]
        return self.git(*args, data=b'synthetic move\n').decode().strip()

    def run_tool(self, *args):
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            result = replay.main([*args, '--plan', str(self.plan)])
        self.output = out.getvalue()
        return result

    def moved(self):
        dest = self.repo / 'result'
        self.assertEqual(self.run_tool('replay', '--source', self.base, '--dest', str(dest)), 0, self.output)
        return replay.files(dest)

    def test_replay_expected_tree(self):
        actual = self.moved()
        expected = dict(self.original)
        expected['crates/manifold-node-engine/src/foo.rs'] = expected.pop('crates/manifold-renderer/src/foo.rs')
        expected['crates/manifold-renderer/src/lib.rs'] = ('100644', b'use manifold_node_engine::foo::{X, Y};\nmanifold_node_engine::primitive! {}\nmanifold_node_engine::param_tooltips! {}\n')
        expected.update({'plans/p1/'+p:v for p,v in replay.files(self.plan).items()})
        self.assertEqual(actual, expected)

    def test_verify_pure_replay(self):
        moved = self.commit(self.moved(), self.base)
        self.assertEqual(self.run_tool('verify', moved), 0, self.output)

    def reject_edit(self, edit):
        result = self.moved()
        edit(result)
        moved = self.commit(result, self.base)
        self.assertEqual(self.run_tool('verify', moved), 1, self.output)
        self.assertIn('different:', self.output)

    def test_reject_one_byte(self):
        self.reject_edit(lambda rows: rows.__setitem__('binary', ('100644', b'\0\xfe\n')))

    def test_reject_extra_file(self):
        self.reject_edit(lambda rows: rows.__setitem__('extra', ('100644', b'x')))

    def test_reject_missing_file(self):
        self.reject_edit(lambda rows: rows.pop('binary'))

    def test_reject_mode_change(self):
        self.reject_edit(lambda rows: rows.__setitem__('run', ('100644', self.original['run'][1])))

    def test_reject_symlink_change(self):
        self.reject_edit(lambda rows: rows.__setitem__('link', ('120000', b'binary')))

    def test_reject_changed_plan(self):
        moved = self.commit(self.moved(), self.base)
        with (self.plan/'moves.tsv').open('a') as f: f.write('# changed\n')
        self.assertEqual(self.run_tool('verify', moved), 1)
        self.assertIn('plan differs from commit', self.output)

    def test_reject_missing_move_input(self):
        with (self.plan/'moves.tsv').open('a') as f: f.write('missing\tnew\n')
        self.assertEqual(self.run_tool('replay', '--source', self.base, '--dest', str(self.repo/'result')), 1)
        self.assertIn('missing/non-regular', self.output)
        self.assertFalse((self.repo/'result').exists())

    def test_reject_duplicate_destination(self):
        with (self.plan/'moves.tsv').open('a') as f: f.write('binary\tcrates/manifold-node-engine/src/foo.rs\n')
        self.assertEqual(self.run_tool('replay', '--source', self.base, '--dest', str(self.repo/'result')), 1)
        self.assertIn('duplicate move', self.output)

    def test_reject_existing_destination(self):
        self.moved()
        self.assertEqual(self.run_tool('replay', '--source', self.base, '--dest', str(self.repo/'result')), 1)
        self.assertIn('destination must not exist', self.output)

    def test_reject_wiring_context_drift(self):
        (self.plan/'manifests.json').write_text(json.dumps([{'path':'Cargo.toml','before':'absent','after':'changed'}]))
        self.assertEqual(self.run_tool('replay', '--source', self.base, '--dest', str(self.repo/'result')), 1)
        self.assertIn('wiring context changed', self.output)

    def test_reject_parentless_commit(self):
        result = self.commit(self.moved())
        self.assertEqual(self.run_tool('verify', result), 1)
        self.assertIn('exactly one parent', self.output)

    def test_deterministic(self):
        first = self.moved()
        self.assertEqual(self.run_tool('replay', '--source', self.base, '--dest', str(self.repo/'second')), 0, self.output)
        self.assertEqual(first, replay.files(self.repo/'second'))

    def test_templates_and_context_edits(self):
        templates = self.plan / 'templates/crates/manifold-node-engine'
        templates.mkdir(parents=True)
        (templates/'Cargo.toml').write_text('[package]\nname = "manifold-node-engine"\n')
        (self.plan/'manifests.json').write_text(json.dumps([{
            'path':'Cargo.toml', 'before':'members = []',
            'after':'members = ["crates/manifold-node-engine"]'}]))
        actual = self.moved()
        self.assertEqual(actual['crates/manifold-node-engine/Cargo.toml'][1],
                         b'[package]\nname = "manifold-node-engine"\n')
        self.assertIn(b'members = ["crates/manifold-node-engine"]', actual['Cargo.toml'][1])
        self.assertEqual(self.run_tool('verify', self.commit(actual, self.base)), 0, self.output)

    def test_reject_symlink_plan_input(self):
        config = self.plan / 'plan.json'
        config.rename(self.repo / 'external-config')
        config.symlink_to('../../external-config')
        self.assertEqual(self.run_tool('replay', '--source', self.base, '--dest', str(self.repo/'result')), 1)
        self.assertIn('plan inputs must not be symlinks', self.output)

    def test_identity_split_keeps_only_family_emission(self):
        build = ('fn main() {\n'
                 '    native_source_identity::emit_source_identity(\n'
                 '        &root, &["src/water.rs"], "INTEGRATION",\n'
                 '    )\n    .expect("integration");\n'
                 '    native_source_identity::emit_source_identity(\n'
                 '        &root, &["src/gltf.rs"], "FAMILY",\n'
                 '    )\n    .expect("family");\n}\n')
        self.original['crates/manifold-renderer/build.rs'] = ('100644', build.encode())
        self.base = self.commit(self.original)
        config = json.loads((self.plan/'plan.json').read_text())
        config['split_identity'] = {'renderer_build':'crates/manifold-renderer/build.rs',
                                    'integration_key':'INTEGRATION', 'family_source':'src/gltf.rs'}
        (self.plan/'plan.json').write_text(json.dumps(config))
        actual = self.moved()['crates/manifold-renderer/build.rs'][1]
        self.assertNotIn(b'INTEGRATION', actual)
        self.assertIn(b'"src/gltf.rs"], "FAMILY"', actual)

    def test_other_crate_names(self):
        config = json.loads((self.plan/'plan.json').read_text())
        config.update(source_crate='crates/manifold-image', destination_crate='crates/manifold-nodes',
                      rewrite_roots=['crates/manifold-image', 'crates/manifold-nodes'])
        (self.plan/'plan.json').write_text(json.dumps(config))
        (self.plan/'moves.tsv').write_text('crates/manifold-image/src/foo.rs\tcrates/manifold-nodes/src/foo.rs\n')
        (self.plan/'rewrites.tsv').write_text('')
        self.base = self.commit({
            'crates/manifold-image/src/foo.rs': ('100644', b'pub const X: u8 = 1;\n'),
            'crates/manifold-image/src/lib.rs': ('100644', b'use manifold_image::foo::X;\n')})
        actual = self.moved()
        self.assertEqual(actual['crates/manifold-image/src/lib.rs'][1], b'use manifold_nodes::foo::X;\n')
        self.assertEqual(self.run_tool('verify', self.commit(actual, self.base)), 0, self.output)

    def test_family_reference_keeps_owner(self):
        replay.CONFIG = {}
        text = 'crate::node_graph::primitives::blob_bounds::BlobBounds::new();\n'
        actual = replay.rewrite_rust(text, 'crates/manifold-renderer/src/node_graph/primitives/liquid_surface_tests.rs',
                                    'crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs', {}, {})
        self.assertEqual(actual, text.replace('crate::', 'manifold_renderer::', 1))


if __name__ == '__main__':
    unittest.main()
