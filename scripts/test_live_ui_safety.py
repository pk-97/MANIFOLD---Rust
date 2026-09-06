import json
import unittest
from unittest import mock

import live_ui_safety_demo as safety


class SafetyDemoTests(unittest.TestCase):
    def test_refuses_unprepared_project_without_mutation(self):
        with mock.patch.object(safety, "request", return_value={"data": {"state": {"layers": []}, "nodes": []}}), mock.patch.object(safety, "disconnect_drag") as drag:
            with self.assertRaisesRegex(RuntimeError, "refusing"):
                safety.demonstrate("test.sock")
            drag.assert_not_called()

    def test_disconnect_drag_sends_json_and_closes_without_reading(self):
        connection = mock.MagicMock()
        connection.__enter__.return_value = connection
        factory = mock.patch.object(safety.socket, "socket", return_value=connection)
        with factory, mock.patch.object(safety.time, "sleep") as sleep:
            safety.disconnect_drag("test.sock", {"x": 1, "y": 2}, {"x": 3, "y": 4}, 60, 0.15)
        connection.connect.assert_called_once_with("test.sock")
        payload = json.loads(connection.sendall.call_args.args[0])
        self.assertEqual(payload["action"]["Pointer"]["gesture"]["Drag"]["steps"], 60)
        sleep.assert_called_once_with(0.15)
        connection.__exit__.assert_called_once_with(None, None, None)
        connection.recv.assert_not_called()


if __name__ == "__main__":
    unittest.main()
