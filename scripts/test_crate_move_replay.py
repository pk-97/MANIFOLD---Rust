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
        self.pin()

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
        self.pin()
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

    def test_identity_split_requires_separate_reviewed_commit(self):
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
        self.rejects_replay()
        self.assertIn('body edits require a separate commit', self.output)

    def test_other_crate_names(self):
        config = json.loads((self.plan/'plan.json').read_text())
        config.update(source_crate='crates/manifold-image', destination_crate='crates/manifold-nodes',
                      rewrite_roots=['crates/manifold-image', 'crates/manifold-nodes'])
        (self.plan/'plan.json').write_text(json.dumps(config))
        (self.plan/'moves.tsv').write_text('crates/manifold-image/src/foo.rs\tcrates/manifold-nodes/src/foo.rs\n')
        (self.plan/'rewrites.tsv').write_text('')
        self.original = {
            'crates/manifold-image/src/foo.rs': ('100644', b'pub const X: u8 = 1;\n'),
            'crates/manifold-image/src/lib.rs': ('100644', b'use manifold_image::foo::X;\n')}
        self.pin()
        actual = self.moved()
        self.assertEqual(actual['crates/manifold-image/src/lib.rs'][1], b'use manifold_nodes::foo::X;\n')
        self.assertEqual(self.run_tool('verify', self.commit(actual, self.base)), 0, self.output)

    def test_family_reference_keeps_owner(self):
        replay.CONFIG = {}
        text = 'crate::node_graph::primitives::blob_bounds::BlobBounds::new();\n'
        actual = replay.rewrite_rust(text, 'crates/manifold-renderer/src/node_graph/primitives/liquid_surface_tests.rs',
                                    'crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs', {}, {})
        self.assertEqual(actual, text.replace('crate::', 'manifold_renderer::', 1))

    def pin(self):
        self.original.update({'plans/p1/'+p:v for p,v in replay.files(self.plan).items()})
        self.base = self.commit(self.original)

    def rejects_replay(self):
        dest = self.repo/'rejected'
        self.assertEqual(self.run_tool('replay', '--source', self.base, '--dest', str(dest)), 1, self.output)
        self.assertFalse(dest.exists())

    def test_case_collision_parent_loses_unplanned_file(self):
        self.original.update(CaseFile=('100644', b'original'), casefile=('100644', b'overwritten'))
        self.pin()
        self.rejects_replay()

    def test_unicode_normalization_parent_loss(self):
        self.original.update({'é-file': ('100644', b'one'), 'e\u0301-file': ('100644', b'two')})
        self.pin()
        self.rejects_replay()

    def test_case_collision_move_destinations_silently_overwrite(self):
        self.original['second'] = ('100644', b'other')
        (self.plan/'moves.tsv').write_bytes(b'crates/manifold-renderer/src/foo.rs\tOut\nsecond\tout\n')
        self.pin()
        self.rejects_replay()

    def test_materialization_case_alias_follows_symlink(self):
        outside = self.repo/'outside-file'
        outside.write_bytes(b'untouched')
        self.original.update(Alias=('120000', os.fsencode(outside)), alias=('100644', b'overwritten'))
        self.pin()
        self.rejects_replay()
        self.assertEqual(outside.read_bytes(), b'untouched')

    def test_parent_plan_mutation_accepted(self):
        (self.plan/'README.md').write_bytes(b'reviewed\n')
        self.pin()
        rows = self.moved()
        rows['plans/p1/README.md'] = ('100644', b'unreviewed\n')
        (self.plan/'README.md').write_bytes(b'unreviewed\n')
        self.assertEqual(self.run_tool('verify', self.commit(rows, self.base)), 1, self.output)

    def test_parent_plan_deletion_accepted(self):
        (self.plan/'README.md').write_bytes(b'reviewed\n')
        self.pin()
        rows = self.moved()
        del rows['plans/p1/README.md']
        (self.plan/'README.md').unlink()
        self.assertEqual(self.run_tool('verify', self.commit(rows, self.base)), 1, self.output)

    def test_same_commit_plan_can_authorize_body_fix(self):
        self.pin()
        rows = self.moved()
        payload = json.dumps([{'path':'crates/manifold-node-engine/src/foo.rs', 'before':'= 7', 'after':'= 999'}]).encode()
        rows['plans/p1/declarations.json'] = ('100644', payload)
        rows['crates/manifold-node-engine/src/foo.rs'] = ('100644', b'pub const X: u32 = 999;\n')
        (self.plan/'declarations.json').write_bytes(payload)
        self.assertEqual(self.run_tool('verify', self.commit(rows, self.base)), 1, self.output)

    def test_each_patch_channel_accepts_arbitrary_body_change(self):
        for name in ('manifests.json', 'declarations.json', 'finish.json'):
            with self.subTest(channel=name):
                (self.plan/name).write_bytes(json.dumps([{'path':'crates/manifold-renderer/src/foo.rs', 'before':'= 7', 'after':'= 999'}]).encode())
                self.rejects_replay()
                (self.plan/name).unlink()

    def test_rewrite_nonpath_code_injection(self):
        (self.plan/'rewrites.tsv').write_bytes(b'manifold_renderer::foo::X\tstd::process::exit(42)\n')
        self.rejects_replay()

    def test_path_rewrites_modify_string_literals(self):
        replay.CONFIG = {}
        text = '// external::call\n/* outer /* nested */ external::call */\nconst S: &str = r###"external::call"###;\nconst T: &str = "external::call";\nexternal::call();\n'
        actual = replay.rewrite_rust(text, 'x.rs', 'x.rs', {'external::call':'other::call'}, {})
        self.assertEqual(actual, text[:-len('external::call();\n')]+'other::call();\n')

    def test_template_carries_executable_code(self):
        p = self.plan/'templates/crates/manifold-node-engine/build.rs'
        p.parent.mkdir(parents=True)
        p.write_bytes(b'fn main() { std::process::exit(42); }\n')
        self.pin()
        rows = self.moved()
        self.assertEqual(self.run_tool('verify', self.commit(rows, self.base)), 0, self.output)
        import hashlib
        self.assertIn(hashlib.sha256(p.read_bytes()).hexdigest(), self.output)
        self.assertIn('review template crates/manifold-node-engine/build.rs', self.output)

    def test_manifest_can_change_dependency_not_move(self):
        row = {'path':'Cargo.toml', 'before':'members = []', 'after':'members = []\n[patch.crates-io]\nserde = { path = "elsewhere" }'}
        (self.plan/'manifests.json').write_bytes(json.dumps([row]).encode())
        self.pin()
        rows = self.moved()
        self.assertEqual(self.run_tool('verify', self.commit(rows, self.base)), 0, self.output)
        import hashlib
        digest = hashlib.sha256(json.dumps(row, sort_keys=True, ensure_ascii=True, separators=(',', ':')).encode('utf-8')).hexdigest()
        self.assertIn(digest, self.output)
        self.assertIn('review manifest Cargo.toml', self.output)

    def test_duplicate_rewrite_order_changes_body(self):
        (self.plan/'rewrites.tsv').write_bytes(b'external::call\tfirst::call\nexternal::call\tsecond::call\n')
        self.rejects_replay()

    def test_symlink_move_parent_creates_outside_directory(self):
        outside = self.repo/'outside'
        outside.mkdir()
        self.original['escape'] = ('120000', os.fsencode(outside))
        (self.plan/'moves.tsv').write_bytes(b'crates/manifold-renderer/src/foo.rs\tescape/leaked/sub/foo.rs\n')
        self.pin()
        self.rejects_replay()
        self.assertEqual(list(outside.iterdir()), [])

    def test_temporary_cleanup_success_and_failure(self):
        self.pin()
        rows = self.moved()
        self.assertEqual(self.run_tool('verify', self.commit(rows, self.base)), 0, self.output)
        rows['binary'] = ('100644', b'changed')
        self.assertEqual(self.run_tool('verify', self.commit(rows, self.base)), 1, self.output)
        (self.plan/'manifests.json').write_bytes(b'[{"path":"Cargo.toml","before":"absent","after":"x"}]')
        self.rejects_replay()
        self.assertEqual(list((self.repo/'target').glob('crate-move-*')), [])

    def test_locale_ascii_without_utf8_mode(self):
        self.original['crates/manifold-renderer/src/foo.rs'] = ('100644', '// café\npub const X: u32 = 7;\n'.encode('utf-8'))
        self.pin()
        import sys
        env = dict(os.environ, LC_ALL='C', PYTHONUTF8='0', PYTHONCOERCECLOCALE='0')
        proc = subprocess.run([sys.executable, '-B', str(Path(replay.__file__).resolve()), 'replay', '--source', self.base, '--plan', str(self.plan), '--dest', str(self.repo/'ascii')], capture_output=True, env=env)
        self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_directory_component_aliases(self):
        for paths in (('A/one', 'a/two'), ('é/one', 'e\u0301/two'), ('A', 'a/child')):
            with self.subTest(paths=paths), self.assertRaisesRegex(ValueError, 'collision'):
                replay.validate_paths(paths)

    def test_template_aliases_existing_tree(self):
        p = self.plan/'templates/BINARY'
        p.parent.mkdir()
        p.write_bytes(b'overwrite')
        self.rejects_replay()
        self.assertIn('collision', self.output)

    def test_template_aliases_move_destination(self):
        p = self.plan/'templates/crates/manifold-node-engine/src/Foo.rs'
        p.parent.mkdir(parents=True)
        p.write_bytes(b'overwrite')
        self.rejects_replay()
        self.assertIn('collision', self.output)

    def test_write_files_never_overwrites_symlink(self):
        root = self.repo/'materialized'
        root.mkdir()
        outside = self.repo/'sentinel'
        outside.write_bytes(b'untouched')
        (root/'alias').symlink_to(outside)
        with self.assertRaises(ValueError):
            replay.write_files(root, {'alias': ('100644', b'overwrite')})
        self.assertEqual(outside.read_bytes(), b'untouched')

    def test_output_symlink_ancestor_before_mkdir(self):
        outside = self.repo/'outside'
        outside.mkdir()
        (self.repo/'escape').symlink_to(outside)
        self.assertEqual(self.run_tool('replay', '--source', self.base, '--dest', str(self.repo/'escape/leaked/result')), 1, self.output)
        self.assertEqual(list(outside.iterdir()), [])

    def test_parent_materialization_roundtrip_required(self):
        from unittest.mock import patch
        original_files = replay.files
        def wrong_inventory(root):
            rows = original_files(root)
            if root.name == 'parent': rows.pop('binary', None)
            return rows
        with patch.object(replay, 'files', side_effect=wrong_inventory):
            self.rejects_replay()
        self.assertIn('materialization differs', self.output)

    def test_plan_must_exist_before_move(self):
        rows = self.moved()
        parent = self.commit({p:v for p,v in self.original.items() if not p.startswith('plans/')})
        self.assertEqual(self.run_tool('verify', self.commit(rows, parent)), 1, self.output)
        self.assertIn('before the move', self.output)

    def test_plan_mode_change_rejected(self):
        rows = self.moved()
        mode, data = rows['plans/p1/plan.json']
        rows['plans/p1/plan.json'] = ('100755', data)
        self.assertEqual(self.run_tool('verify', self.commit(rows, self.base)), 1, self.output)
        self.assertIn('move commit changes plan', self.output)

    def test_conflicting_derived_mapping_rejected(self):
        (self.plan/'rewrites.tsv').write_bytes(b'manifold_renderer::foo\texternal::foo\n')
        self.rejects_replay()
        self.assertIn('conflicting derived rewrite', self.output)

    def test_invalid_alias_rejected(self):
        config = json.loads((self.plan/'plan.json').read_bytes())
        config['aliases'] = {'manifold_renderer::foo': 'std::process::exit(42)'}
        (self.plan/'plan.json').write_bytes(json.dumps(config).encode('utf-8'))
        self.rejects_replay()

    def test_rewrite_preserves_crlf_and_literal_assets(self):
        replay.CONFIG = {}
        text = '// include_str!("old.txt")\r\nconst S: &str = "external::call";\r\nexternal::call();\r\ninclude_str!("old.txt");\r\n'
        actual = replay.rewrite_rust(text, 'src/a.rs', 'src/a.rs', {'external::call':'other::call'}, {'src/old.txt':'src/new.txt'})
        self.assertEqual(actual, text.replace('external::call();', 'other::call();').replace('\r\ninclude_str!("old.txt")', '\r\ninclude_str!("new.txt")'))

    def declarations(self, rows, source='mod foo;\nfn unchanged() {}\n'):
        self.original['crates/manifold-renderer/src/lib.rs'] = ('100644', source.encode())
        (self.plan/'declarations.tsv').write_text(''.join('\t'.join(row)+'\n' for row in rows))
        self.pin()

    def test_declarations_remove_move_add_mod_cfg_and_use(self):
        old = 'crates/manifold-renderer/src/lib.rs'
        new = 'crates/manifold-node-engine/src/lib.rs'
        template = self.plan/'templates'/new
        template.parent.mkdir(parents=True)
        template.write_bytes(b'// engine\r\n')
        rows = [(old, 'remove', 'mod foo;', 'mod foo;'),
                (new, 'add', '@end', '#[cfg_attr(test, cfg(any(test, feature = "proof")), doc(hidden))]'),
                (new, 'add', '@end', 'pub mod foo;'),
                (old, 'add', '@start', 'use manifold_node_engine::foo::{self, X as Renamed};')]
        self.declarations(rows)
        actual = self.moved()
        self.assertEqual(actual[old][1], b'use manifold_node_engine::foo::{self, X as Renamed};\nfn unchanged() {}\n')
        self.assertEqual(actual[new][1], b'// engine\r\n#[cfg_attr(test, cfg(any(test, feature = "proof")), doc(hidden))]\r\npub mod foo;\r\n')
        self.assertEqual(self.run_tool('verify', self.commit(actual, self.base)), 0, self.output)
        for row in rows: self.assertIn('review declaration '+json.dumps(row), self.output)

    def test_declarations_reject_non_wiring_grammar(self):
        for line in ('fn injected() {}', 'const X: u8 = 1;', 'impl X {}',
                     '#[path = "evil.rs"]', '#[macro_use]', '#[allow(dead_code)]',
                     '#[cfg_attr(test, path = "evil.rs")]', '#[cfg_attr(test, macro_use)]',
                     'mod foo {}', 'mod foo; mod other;', 'use crate::foo; fn evil() {}',
                     '// mod foo;', 'mod foo; // fn evil() {}', 'use crate::foo/* hidden */;',
                     '#[cfg(test)] #[path = "evil.rs"]', '#[cfg_attr(test, cfg('):
            with self.subTest(line=line):
                self.declarations([('crates/manifold-renderer/src/lib.rs', 'add', '@start', line)])
                self.rejects_replay()

    def test_declarations_reject_body_use(self):
        file = 'crates/manifold-renderer/src/lib.rs'
        for source in ('fn f() {\n    use manifold_node_engine::foo::X;\n}\n',
                       'impl X {\n    use manifold_node_engine::foo::X;\n}\n',
                       'macro_rules! x { () => {\n    use manifold_node_engine::foo::X;\n} }\n',
                       'const X: () = {\n    use manifold_node_engine::foo::X;\n};\n'):
            for op in ('add', 'remove'):
                with self.subTest(source=source, op=op):
                    self.declarations([(file, op, '    use manifold_node_engine::foo::X;', '    use manifold_node_engine::foo::X;')], source)
                    self.rejects_replay()
                    self.assertIn('not module wiring', self.output)

    def test_declarations_reject_missing_module_path_and_remove(self):
        file = 'crates/manifold-renderer/src/lib.rs'
        for op, anchor, line in (('add', '@start', 'mod absent;'),
                                 ('add', '@start', 'use manifold_node_engine::foo::Absent;'),
                                 ('remove', 'mod absent;', 'mod absent;'),
                                 ('remove', 'mod foo;', 'mod absent;')):
            with self.subTest(op=op, line=line):
                self.declarations([(file, op, anchor, line)])
                self.rejects_replay()

    def test_declarations_attributes_cannot_transfer_to_bodies(self):
        file = 'crates/manifold-renderer/src/lib.rs'
        cases = [([('remove', 'mod foo;', 'mod foo;')], '#[cfg(test)]\nmod foo;\nfn f() {}\n'),
                 ([('add', '@start', '#[cfg(test)]')], 'fn f() {}\n'),
                 ([('add', 'mod foo;', 'use manifold_node_engine::foo::X;')], '#[cfg(test)]\nmod foo;\n'),
                 ([('remove', 'mod foo;', 'mod foo;')], '#[path = "foo.rs"]\nmod foo;\n')]
        for operations, source in cases:
            with self.subTest(source=source, operations=operations):
                self.declarations([(file, *op) for op in operations], source)
                self.rejects_replay()

    def test_declarations_remove_cfg_group_and_preserve_other_bytes(self):
        file = 'crates/manifold-renderer/src/lib.rs'
        self.declarations([(file, 'remove', 'mod foo;', 'mod foo;'),
                           (file, 'remove', '#[cfg(test)]', '#[cfg(test)]')],
                          '// keep\r\n#[cfg(test)]\r\nmod foo;\r\nfn unchanged() {}\r\n')
        actual = self.moved()
        self.assertEqual(actual[file][1], b'// keep\r\nfn unchanged() {}\r\n')
        actual[file] = ('100644', b'// keep\r\nfn changed() {}\r\n')
        self.assertEqual(self.run_tool('verify', self.commit(actual, self.base)), 1, self.output)

    def test_declarations_inline_module_scope(self):
        file = 'crates/manifold-renderer/src/lib.rs'
        self.declarations([(file, 'add', '    mod gone;', '    use manifold_node_engine::foo::X;'),
                           (file, 'remove', '    mod gone;', '    mod gone;')],
                          'mod nested {\n    mod gone;\n}\n')
        actual = self.moved()
        self.assertEqual(actual[file][1], b'mod nested {\n    use manifold_node_engine::foo::X;\n}\n')

    def test_declarations_cfg_mod_visibility_and_empty_root(self):
        file = 'crates/manifold-node-engine/src/lib.rs'
        template = self.plan/'templates'/file
        template.parent.mkdir(parents=True)
        template.write_bytes(b'')
        self.declarations([(file, 'add', '@end', '#[cfg(test)]'),
                           (file, 'add', '@end', '#[doc(hidden)]'),
                           (file, 'add', '@end', 'pub(crate) mod foo;')])
        actual = self.moved()
        self.assertEqual(actual[file][1], b'#[cfg(test)]\n#[doc(hidden)]\npub(crate) mod foo;\n')
        for visibility in ('', 'pub ', 'pub(crate) ', 'pub(super) ', 'pub(in crate::nested) '):
            self.assertEqual(replay.declaration(visibility+'mod foo;'), ('mod', ['foo']))
            self.assertEqual(replay.declaration(visibility+'use crate::foo;'), ('use', [['crate', 'foo']]))

    def test_declarations_module_symlink_is_not_existence(self):
        file = 'crates/manifold-renderer/src/lib.rs'
        self.original['crates/manifold-renderer/src/fake.rs'] = ('120000', b'lib.rs')
        self.declarations([(file, 'add', '@start', 'mod fake;')])
        self.rejects_replay()
        self.assertIn('module does not exist', self.output)

    def test_declarations_do_not_edit_comment_or_literal_contents(self):
        file = 'crates/manifold-renderer/src/lib.rs'
        line = 'use manifold_node_engine::foo::X;'
        for source in ('/*\n'+line+'\n*/\n', 'const S: &str = r#"\n'+line+'\n"#;\n'):
            for op in ('add', 'remove'):
                with self.subTest(source=source, op=op):
                    self.declarations([(file, op, line, line)], source)
                    self.rejects_replay()
                    self.assertIn('not module wiring', self.output)


if __name__ == '__main__':
    unittest.main()
