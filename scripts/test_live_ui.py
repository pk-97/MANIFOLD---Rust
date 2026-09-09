import unittest
from unittest.mock import patch

import live_ui


class FlowTests(unittest.TestCase):
    def test_edit_is_dispatched_once_and_observation_is_polled(self):
        with patch.object(live_ui, "request", side_effect=[{"ok": True}, {"data": {"value": 0}}, {"data": {"value": 1}}]) as request:
            with patch.object(live_ui.time, "sleep"):
                results = live_ui.run("socket", [{"request": {"op": "act"}}, {"expect": {"data.value": 1}}])
        self.assertEqual(len(results), 2)
        self.assertEqual([call.args[1]["op"] for call in request.call_args_list], ["act", "observe", "observe"])

    def test_mutating_expectation_is_rejected_before_dispatch(self):
        with patch.object(live_ui, "request") as request:
            with self.assertRaisesRegex(ValueError, "read-only"):
                live_ui.run("socket", [{"request": {"op": "act"}, "expect": {"ok": True}}])
        request.assert_not_called()

    def test_missing_postcondition_fails_with_observation(self):
        with patch.object(live_ui, "request", return_value={"data": {}}):
            with self.assertRaisesRegex(RuntimeError, "step 0 expectation failed"):
                live_ui.run("socket", [{"timeout": 0, "expect": {"data.layers.0.name": "Gen 2"}}])

    def test_numeric_array_paths(self):
        self.assertEqual(live_ui.lookup({"layers": [{"clips": [128]}]}, "layers.0.clips.0"), 128)


if __name__ == "__main__":
    unittest.main()
