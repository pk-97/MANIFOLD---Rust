#!/usr/bin/env python3
"""Full-tree replay proofs in an isolated synthetic Git workspace; no GPU."""
import contextlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

import crate_move_replay as replay


class ReplayTests(unittest.TestCase):
    def test_module_readers_see_both_testkit_visible_arms(self):
        source = (
            'manifold_core::testkit_visible! { mod plain; }\n'
            'testkit_visible! {\n'
            '    testkit { mod test_only; }\n'
            '    production { mod production_only; }\n'
            '}\n'
        )
        names = [re.search(r'\bmod\s+(\w+)', source[head:end])[1]
                 for _start, end, head, scope in replay.module_items(source)
                 if not scope]
        self.assertEqual(names, ['plain', 'production_only'])

    def test_production_expansion_preserves_lines_and_discards_nested_test_arm(self):
        source = (
            'testkit_visible ! {\n'
            '    testkit { testkit_visible! { mod nested_test; } }\n'
            '    production { manifold_core :: testkit_visible ! { mod kept; } }\n'
            '}\n'
        )
        expanded = replay.production_text(source)
        self.assertEqual(expanded.count("\n"), source.count("\n"))
        self.assertNotIn("nested_test", expanded)
        self.assertIn("kept", expanded)
        self.assertEqual([re.search(r'\bmod\s+(\w+)', source[head:end])[1]
                          for _start, end, head, scope in replay.module_items(source)
                          if not scope], ['kept'])

    def test_macro_reader_keeps_plain_struct_named_production_and_other_delimiters(self):
        source = (
            'testkit_visible! { struct production { value: u8 } }\n'
            'testkit_visible ! ( ignored )\n'
            'manifold_core :: testkit_visible ! [ ignored ]\n'
        )
        expanded = replay.production_text(source)
        self.assertIn('struct production', expanded)
        self.assertEqual(len(replay._testkit_calls(source)), 3)

    def test_wrapped_module_mount_mutation_fails_closed(self):
        source = 'testkit_visible! {\n    testkit { mod old; }\n    production { mod old; }\n}\n'
        with self.assertRaisesRegex(ValueError, 'separate reviewed fix'):
            replay.mount_items(source, 'old')

    def test_path_modules_see_path_mount_inside_testkit_visible(self):
        sources = {
            'crates/a/src/lib.rs': 'testkit_visible! { #[path = "child.rs"] mod child; }\n',
            'crates/a/src/child.rs': 'pub const VALUE: u8 = 1;\n',
        }
        self.assertEqual(
            replay.path_modules(sources, {}, sources.__contains__),
            {'crates/a/src/child.rs': 'a::child'},
        )

    def test_shared_map_derivation_is_rooted_and_scoped(self):
        moves = {'crates/a/src/foo.rs': 'crates/b/src/bar.rs'}
        mapping = replay.mappings(moves, [], {})
        self.assertEqual(mapping, {'a::foo': 'b::bar'})
        self.assertIs(replay.file_mapping('crates/a/src/foo.rs', 'crates/b/src/bar.rs',
                                          moves, mapping, ['crates/a', 'crates/b']), mapping)
        self.assertIsNone(replay.file_mapping('crates/c/src/foo.rs', 'crates/c/src/foo.rs',
                                              moves, mapping, ['crates/a', 'crates/b']))
        self.assertIsNone(replay.file_mapping('crates/a/src/other.rs', 'crates/b/src/bar.rs',
                                              moves, mapping, ['crates/a', 'crates/b']))
        # A consumer crate's local crate::foo is not the source crate's foo.
        self.assertEqual(replay.rewrite_rust('crate::foo::X;', 'crates/c/src/lib.rs',
                                             'crates/c/src/lib.rs', mapping, moves), 'crate::foo::X;')

    def test_shared_include_module_discovery_does_not_mutate_globals(self):
        sources = {'crates/a/src/foo.rs': 'include!("fragment.rs");\n',
                   'crates/a/src/fragment.rs': 'pub const X: u8 = 1;\n'}
        moves = {'crates/a/src/foo.rs': 'crates/b/src/bar.rs',
                 'crates/a/src/fragment.rs': 'crates/b/src/fragment.rs'}
        previous = dict(replay.MODULES)
        modules = replay.path_modules(sources, moves, sources.__contains__)
        self.assertEqual(modules, {'crates/a/src/fragment.rs': 'a::foo',
                                   'crates/b/src/fragment.rs': 'b::bar'})
        self.assertEqual(replay.MODULES, previous)
        self.assertEqual(replay.mappings(moves, [], modules), {'a::foo': 'b::bar'})

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
            'crates/manifold-renderer/src/lib.rs': ('100644', b'mod foo;\nuse crate::foo::{X, Y};\ncrate::primitive! {}\ncrate::param_tooltips! {}\n'),
            'run': ('100755', b'#!/bin/sh\nexit 0\n'),
            'link': ('120000', b'run'),
            'binary': ('100644', b'\0\xff\n'),
        }
        self.template('crates/manifold-node-engine/src/lib.rs', 'mod foo;\n')
        self.pin()

    def template(self, path, text):
        p = self.plan/'templates'/path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(text.encode())

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
        expected['crates/manifold-node-engine/src/lib.rs'] = ('100644', b'mod foo;\n')
        expected.update({'plans/p1/'+p:v for p,v in replay.files(self.plan).items()})
        self.assertEqual(actual, expected)

    def test_directory_source_preserves_applied_residual(self):
        source = self.repo / 'draft'
        entries = dict(self.original)
        entries['crates/manifold-renderer/src/foo.rs'] = ('100644', b'pub const X: u32 = 8;\n')
        replay.write_files(source, entries)
        dest = self.repo / 'directory-result'
        self.assertEqual(self.run_tool('replay', '--source', str(source), '--dest', str(dest)), 0, self.output)
        self.assertEqual(replay.files(dest)['crates/manifold-node-engine/src/foo.rs'],
                         entries['crates/manifold-renderer/src/foo.rs'])
        self.assertEqual(replay.files(source), entries)
        self.assertEqual(self.run_tool('verify', str(source)), 1, self.output)

    def test_directory_source_rejects_unsafe_paths(self):
        for forbidden in ('.git', 'target'):
            with self.subTest(forbidden=forbidden):
                source = self.repo / ('draft-' + forbidden)
                replay.write_files(source, self.original)
                (source / forbidden).mkdir()
                (source / forbidden / 'private').write_text('must not copy')
                dest = self.repo / ('result-' + forbidden)
                self.assertEqual(self.run_tool('replay', '--source', str(source), '--dest', str(dest)), 1)
                self.assertFalse(dest.exists())

    def test_directory_source_rejects_symlink_and_overlap(self):
        source = self.repo / 'draft'
        replay.write_files(source, self.original)
        alias = self.repo / 'alias'
        alias.symlink_to(source, target_is_directory=True)
        for src, dest in ((alias, self.repo / 'result'), (source, source),
                          (source, source / 'result'), (source, self.repo)):
            with self.subTest(source=src, destination=dest):
                self.assertEqual(self.run_tool('replay', '--source', str(src), '--dest', str(dest)), 1)
        self.assertEqual(replay.files(source), self.original)

    def test_residual_artifacts_are_inert_and_verified_as_metadata(self):
        after = self.plan / 'after/crates/manifold-node-engine/src/foo.rs'
        after.parent.mkdir(parents=True)
        after.write_text('pub const X: u32 = 999;\n')
        (self.plan / 'residual-files.txt').write_text('crates/manifold-node-engine/src/foo.rs\n')
        (self.plan / 'residual-deleted.txt').write_text('binary\n')
        self.pin()
        moved = self.moved()
        self.assertEqual(moved['crates/manifold-node-engine/src/foo.rs'],
                         self.original['crates/manifold-renderer/src/foo.rs'])
        self.assertEqual(moved['binary'], self.original['binary'])
        self.assertEqual(self.run_tool('verify', self.commit(moved, self.base)), 0, self.output)
        moved['plans/p1/residual-deleted.txt'] = ('100644', b'run\n')
        self.assertEqual(self.run_tool('verify', self.commit(moved, self.base)), 1, self.output)
        self.assertIn('move commit changes plan', self.output)

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
        templates.mkdir(parents=True, exist_ok=True)
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
        self.original['crates/manifold-nodes-scene/build.rs'] = ('100644', build.encode())
        self.base = self.commit(self.original)
        config = json.loads((self.plan/'plan.json').read_text())
        config['split_identity'] = {'renderer_build':'crates/manifold-nodes-scene/build.rs',
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
        self.template('crates/manifold-nodes/src/lib.rs', 'mod foo;\n')
        self.original = {
            'crates/manifold-image/src/foo.rs': ('100644', b'pub const X: u8 = 1;\n'),
            'crates/manifold-image/src/lib.rs': ('100644', b'mod foo;\nuse manifold_image::foo::X;\n')}
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
        p.parent.mkdir(parents=True, exist_ok=True)
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
        p.parent.mkdir(exist_ok=True)
        p.write_bytes(b'overwrite')
        self.rejects_replay()
        self.assertIn('collision', self.output)

    def test_template_aliases_move_destination(self):
        p = self.plan/'templates/crates/manifold-node-engine/src/Foo.rs'
        p.parent.mkdir(parents=True, exist_ok=True)
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

    def test_review_reproducers_all_rejected(self):
        cases = [
            ('macro_path', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', 'use crate::good::evil!();')]),
            ('extern_crate', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', 'extern crate evil;')]),
            ('unicode_identifier', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', 'use crate::gоod::value;')]),
            ('unicode_space', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', 'use\xa0crate::good::value;')]),
            ('multiline_use', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', 'use crate::good::{\nvalue\n};')]),
            ('cfg_attr_path', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', '#[cfg_attr(all(), path = "evil.rs")]')]),
            ('cfg_attr_macro_use', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', '#[cfg_attr(all(), macro_use)]')]),
            ('cfg_attr_nested_path', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', '#[cfg_attr(all(), cfg_attr(all(), path = "evil.rs"))]')]),
            ('cfg_attr_derive', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', '#[cfg_attr(all(), derive(Evil))]')]),
            ('cfg_attr_multi_macro', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', '#[cfg_attr(all(), doc(hidden), macro_use)]')]),
            ('cr_embedded', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', 'use crate::good::value;\rfn evil() {}')]),
            ('unicode_line_separator', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', 'use crate::good::value;\u2028fn evil() {}')]),
            ('attribute_on_function', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', 'fn main() {}', '#[cfg(any())]')]),
            ('attribute_orphan', '#[cfg(all())]\nuse crate::good::value;\nmod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('remove', 'use crate::good::value;', 'use crate::good::value;')]),
            ('anchor_function', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {\n    let x = 1;\n}\n', [('add', '    let x = 1;', '    use crate::good::value;')]),
            ('anchor_impl', 'mod good { pub fn value() -> u32 { 2 } }\nstruct S;\nimpl S {\n    fn x() {}\n}\nfn main() {}\n', [('add', '    fn x() {}', '    use crate::good::value;')]),
            ('macro_use_removal', '#[macro_use]\nmod side;\nfn main() {}\n', [('remove', 'mod side;', 'mod side;')]),
            ('raw_identifier', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', 'use crate::good::value as r#type;')]),
            ('raw_path', 'mod good { pub fn value() -> u32 { 2 } }\nfn main() {}\n', [('add', '@start', 'use crate::r#good::value;')]),
            ('rename_shadow', 'fn value() -> u32 { 1 }\nmod evil { pub fn value() -> u32 { 2 } }\nmod nested {\n    use super::*;\n    pub fn run() -> u32 { value() }\n}\nfn main() { println!("{}", nested::run()); }\n', [('add', '    pub fn run() -> u32 { value() }', '    use crate::evil::value as value;')]),
            ('glob_resolution', 'mod evil { pub fn Some(_: u32) -> Option<u32> { None } }\nfn main() { println!("{:?}", Some(1)); }\n', [('add', '@start', 'use crate::evil::*;')]),
            ('remove_trait', 'struct S;\ntrait Fallback { fn value(&self) -> u32 { 1 } }\nimpl Fallback for S {}\nmod evil { pub trait Preferred { fn value(self) -> u32; } impl Preferred for crate::S { fn value(self) -> u32 { 2 } } }\nuse crate::evil::Preferred;\nfn main() { println!("{}", S.value()); }\n', [('remove', 'use crate::evil::Preferred;', 'use crate::evil::Preferred;')]),
            ('cfg_disable_use', 'struct S;\ntrait Fallback { fn value(&self) -> u32 { 1 } }\nimpl Fallback for S {}\nmod evil { pub trait Preferred { fn value(self) -> u32; } impl Preferred for crate::S { fn value(self) -> u32 { 2 } } }\nuse crate::evil::Preferred;\nfn main() { println!("{}", S.value()); }\n', [('add', 'use crate::evil::Preferred;', '#[cfg(any())]')]),
            ('cfg_attr_disable_use', 'struct S;\ntrait Fallback { fn value(&self) -> u32 { 1 } }\nimpl Fallback for S {}\nmod evil { pub trait Preferred { fn value(self) -> u32; } impl Preferred for crate::S { fn value(self) -> u32 { 2 } } }\nuse crate::evil::Preferred;\nfn main() { println!("{}", S.value()); }\n', [('add', 'use crate::evil::Preferred;', '#[cfg_attr(all(), cfg(any()))]')]),
            ('remove_cfg_enable_use', 'struct S;\ntrait Fallback { fn value(&self) -> u32 { 1 } }\nimpl Fallback for S {}\nmod evil { pub trait Preferred { fn value(self) -> u32; } impl Preferred for crate::S { fn value(self) -> u32 { 2 } } }\n#[cfg(any())]\nuse crate::evil::Preferred;\nfn main() { println!("{}", S.value()); }\n', [('remove', '#[cfg(any())]', '#[cfg(any())]')]),
            ('remove_side_effect_module', 'struct S;\ntrait Fallback { fn value(&self) -> u32 { 1 } }\nimpl Fallback for S {}\nmod side;\nfn main() { println!("{}", S.value()); }\n', [('remove', 'mod side;', 'mod side;')]),
            ('crlf_resolution', 'mod evil { pub fn Some(_: u32) -> Option<u32> { None } }\r\nfn main() { println!("{:?}", Some(1)); }\r\n', [('add', '@start', 'use crate::evil::*;')]),
            ('different_crate', 'fn value() -> u32 { 1 }\nmod nested {\n    use super::*;\n    pub fn run() -> u32 { value() }\n}\nfn main() { println!("{}", nested::run()); }\n', [('add', '    pub fn run() -> u32 { value() }', '    use evil::value;')]),
            ('inventory_module_removal', 'mod registration;\nfn main() {}\n', [('remove', 'mod registration;', 'mod registration;')]),
            ('multiline_use_removal', 'use crate::good::{\n    value,\n};\nfn main() {}\n', [('remove', 'use crate::good::{', 'use crate::good::{')]),
            ('full_verify_glob', 'mod evil { pub fn Some(_: u32) -> Option<u32> { None } }\nfn main() {}\n', [('add', '@start', 'use crate::evil::*;')]),
        ]
        for name, source, operations in cases:
            with self.subTest(case=name):
                self.declarations([('crates/manifold-renderer/src/lib.rs', *op) for op in operations], source)
                self.rejects_replay()
                self.assertIn('unsupported plan file: declarations.tsv', self.output)

    def test_derived_template_mount_preserves_cfg_visibility_and_crlf(self):
        item = '#[cfg_attr(test, cfg(any(test, feature = "proof")))]\r\n#[doc(hidden)]\r\npub(crate) mod foo;\r\n'
        old = 'crates/manifold-renderer/src/lib.rs'
        new = 'crates/manifold-node-engine/src/lib.rs'
        self.original[old] = ('100644', ('// keep\r\n'+item+'use crate::foo::X as Alias;\r\n').encode())
        self.template(new, item)
        self.pin()
        actual = self.moved()
        self.assertEqual(actual[old][1], b'// keep\r\nuse manifold_node_engine::foo::X as Alias;\r\n')
        self.assertEqual(actual[new][1], item.encode())
        self.assertEqual(self.run_tool('verify', self.commit(actual, self.base)), 0, self.output)

    def test_derived_existing_parent_mount(self):
        old = 'crates/manifold-renderer/src/lib.rs'
        new = 'crates/manifold-node-engine/src/lib.rs'
        item = '#[cfg(test)]\npub(crate) mod foo;\n'
        self.original[old] = ('100644', (item+'fn keep() {}\n').encode())
        self.original[new] = ('100644', b'#![allow(unused)]\nfn existing() {}\n')
        (self.plan/'templates'/new).unlink()
        self.pin()
        actual = self.moved()
        self.assertEqual(actual[old][1], b'fn keep() {}\n')
        self.assertEqual(actual[new][1], ('#![allow(unused)]\nfn existing() {}\n'+item).encode())

    def test_derived_comoved_parent_and_renamed_child(self):
        old = 'crates/manifold-renderer/src/'
        new = 'crates/manifold-node-engine/src/'
        item = '#[cfg(test)]\npub(crate) mod child;\n'
        self.original[old+'foo.rs'] = ('100644', item.encode())
        self.original[old+'foo/child.rs'] = ('100644', b'pub const X: u8 = 1;\n')
        with (self.plan/'moves.tsv').open('a') as f:
            f.write(old+'foo/child.rs\t'+new+'foo/child.rs\n')
        self.pin()
        actual = self.moved()
        self.assertEqual(actual[new+'foo.rs'][1], item.encode())

    def test_derived_identifier_rename_preserves_attribute_literal(self):
        old = 'crates/manifold-renderer/src/'
        new = 'crates/manifold-node-engine/src/'
        item = '#[doc = "mod foo;"]\npub(crate) mod foo;\n'
        self.original[old+'lib.rs'] = ('100644', item.encode())
        (self.plan/'moves.tsv').write_text(old+'foo.rs\t'+new+'bar.rs\n')
        self.template(new+'lib.rs', '#[doc = "mod foo;"]\npub(crate) mod bar;\n')
        self.pin()
        actual = self.moved()
        self.assertEqual(actual[new+'lib.rs'][1], b'#[doc = "mod foo;"]\npub(crate) mod bar;\n')

    def test_complementary_mount_pair_moves_and_renames_together(self):
        old = 'crates/manifold-renderer/src/'
        new = 'crates/manifold-node-engine/src/'
        first = '#[cfg(not(any(test, feature = "testkit")))]\nmod foo;\n'
        second = '#[cfg(any(test, feature = "testkit"))]\npub mod foo;\n'
        self.original[old+'lib.rs'] = ('100644', (first+'fn keep() {}\n'+second).encode())
        (self.plan/'moves.tsv').write_text(old+'foo.rs\t'+new+'bar.rs\n')
        self.template(new+'lib.rs', (first+second).replace('mod foo;', 'mod bar;'))
        self.pin()
        actual = self.moved()
        self.assertEqual(actual[old+'lib.rs'][1], b'fn keep() {}\n')
        self.assertEqual(actual[new+'lib.rs'][1], (first+second).replace('mod foo;', 'mod bar;').encode())
        self.assertEqual(actual[new+'bar.rs'][1], self.original[old+'foo.rs'][1])

    def test_noncomplementary_mount_pairs_rejected(self):
        for second in ('#[cfg(feature = "other")]\npub mod foo;\n',
                       '#[cfg(feature = "testkit")]\npub mod foo;\n',
                       '#[cfg(not(feature = "other"))]\npub mod foo;\n',
                       '#[cfg(not(feature = "testkit"))]\n#[cfg(any())]\npub mod foo;\n'):
            with self.subTest(second=second):
                source = '#[cfg(feature = "testkit")]\nmod foo;\n'+second
                self.original['crates/manifold-renderer/src/lib.rs'] = ('100644', source.encode())
                self.pin()
                self.rejects_replay()

    def test_complementary_mount_pair_with_different_files_rejected(self):
        source = ('#[cfg(test)]\n#[path = "foo.rs"]\nmod foo;\n'
                  '#[cfg(not(test))]\n#[path = "other.rs"]\nmod foo;\n')
        self.original['crates/manifold-renderer/src/lib.rs'] = ('100644', source.encode())
        self.original['crates/manifold-renderer/src/other.rs'] = ('100644', b'pub const X: u32 = 8;\n')
        self.pin()
        self.rejects_replay()
        self.assertIn('path/include module mounts are forbidden', self.output)

    def test_derived_mount_rejects_unproven_source(self):
        for source in ('fn keep() {}\n', 'mod foo {}\n', 'mod foo;\nmod foo;\n',
                       '#[path = "foo.rs"]\nmod foo;\n',
                       '#[cfg_attr(test, path = "foo.rs")]\nmod foo;\n',
                       'fn f() { mod foo; }\n', '/* mod foo; */\n'):
            with self.subTest(source=source):
                self.original['crates/manifold-renderer/src/lib.rs'] = ('100644', source.encode())
                self.pin()
                self.rejects_replay()

    def test_derived_mount_rejects_template_drift(self):
        for mount in ('', 'pub mod foo;\n', '#[cfg(any())]\nmod foo;\n', 'mod foo {}\n'):
            with self.subTest(mount=mount):
                self.template('crates/manifold-node-engine/src/lib.rs', mount)
                self.pin()
                self.rejects_replay()

    def test_use_rewrite_keeps_single_cfg_alias_glob_item(self):
        replay.CONFIG = {}
        source = '#[cfg(any())]\npub(crate) use crate::{foo::X as Alias, other::*};\n'
        actual = replay.rewrite_rust(source, 'crates/demo/src/lib.rs', 'crates/demo/src/lib.rs',
                                     {'demo::foo': 'engine::foo'}, {})
        self.assertEqual(actual, '#[cfg(any())]\npub(crate) use {engine::foo::X as Alias, crate::other::*};\n')


if __name__ == '__main__':
    unittest.main()
