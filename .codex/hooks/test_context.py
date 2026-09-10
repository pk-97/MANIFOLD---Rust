#!/usr/bin/env python3
"""Verify advisory context delivery separately from permission enforcement."""
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]

def load(name, file):
    spec = importlib.util.spec_from_file_location(name, file)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod

context = load('context', Path(__file__).with_name('context.py'))
guard = load('guard_context_test', Path(__file__).with_name('guard.py'))

class ContextTests(unittest.TestCase):
    def event(self, tool, args):
        return {'tool_name': tool, 'tool_input': args, 'cwd': str(ROOT), 'session_id': 'test'}

    def test_patch_and_dispatch_have_subsystem_guidance(self):
        path = 'crates/manifold-ui/src/param_surface.rs'
        for tool, args in [('apply_patch', {'command': '*** Begin Patch\n*** Update File: ' + path + '\n*** End Patch'}),
                           ('collaboration.spawn_agent', {'message': 'Own ' + path})]:
            text = context.context_for_event(self.event(tool, args), ROOT)
            self.assertIn('param_surface', text)
            self.assertIn('advisory', text)

    def test_unrelated_tools_and_paths_emit_nothing(self):
        self.assertEqual(context.context_for_event(self.event('exec_command', {'cmd': 'pwd'}), ROOT), '')
        self.assertEqual(context.context_for_event(self.event('spawn_agent', {'message': 'Own scripts/unmapped.py'}), ROOT), '')

    def test_advice_once_per_session_and_changed_advice(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(guard, 'state_path', return_value=Path(tmp)/'session.json'):
            event = self.event('spawn_agent', {'message': 'Own crates/manifold-ui/src/param_surface.rs'})
            self.assertTrue(guard.advisory_context(event))
            self.assertEqual(guard.advisory_context(event), '')

    def test_denial_has_priority_over_advice(self):
        with patch.object(guard, 'evaluate', return_value='denied'), patch.object(guard, 'advisory_context') as advice, \
             patch('sys.stdin', io.StringIO(json.dumps(self.event('spawn_agent', {})))), patch('sys.stdout', new_callable=io.StringIO) as output:
            guard.main()
            self.assertEqual(json.loads(output.getvalue())['hookSpecificOutput']['permissionDecision'], 'deny')
            advice.assert_not_called()

    def test_missing_guidance_reported_without_permission_override(self):
        with patch.object(guard, 'load', side_effect=FileNotFoundError('fixture')):
            self.assertIn('unavailable', guard.advisory_context(self.event('spawn_agent', {})))

if __name__ == '__main__':
    unittest.main()
