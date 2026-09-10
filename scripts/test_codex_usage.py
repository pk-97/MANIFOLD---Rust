import json, tempfile, unittest
from pathlib import Path
import codex_usage

class UsageTests(unittest.TestCase):
    def test_dedup_context_repeat_filter_and_malformed(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/"a.jsonl"; lines=[
              {"type":"session_meta","session_id":"s","payload":{"id":"s","cwd":"/repo","source":"cli"}},
              {"type":"turn_context","session_id":"s","payload":{"model":"m1","effort":"high","parent_thread_id":"p","source":"cli"}},
              {"type":"response_item","session_id":"s","payload":{"type":"function_call","name":"exec_command","arguments":{"cmd":"cargo test","cwd":"/repo"}}},
              {"type":"response_item","session_id":"s","payload":{"type":"function_call","name":"exec_command","arguments":{"cmd":"cargo test","cwd":"/repo"}}},
              {"type":"token_usage_record","session_id":"s","response_id":"r","timestamp":"2026-09-01T00:00:00Z","payload":{"thread_id":"t1","usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":8,"reasoning_output_tokens":3}}},
              {"type":"token_usage_record","session_id":"s","response_id":"r","payload":{"usage":{"input_tokens":99}}}, "{bad" ]
            p.write_text("\n".join(x if isinstance(x,str) else json.dumps(x) for x in lines))
            out=codex_usage.scan(d); self.assertEqual(out["totals"],{"input":10,"cached":2,"output":8,"reasoning":3}); self.assertEqual(out["by_model"]["m1"]["responses"],1); self.assertEqual(out["repeated_commands"][0]["count"],2); self.assertEqual(out["warnings"],1); self.assertEqual(out["sessions"]["s"]["source"],"cli")
    def test_missing(self):
        with self.assertRaises(FileNotFoundError): codex_usage.scan("/definitely/missing")

    def test_actual_schema_filters_and_command_identity(self):
        with tempfile.TemporaryDirectory() as d:
            def event(kind, payload, stamp="2026-09-10T00:00:00Z"):
                return {"type": kind, "payload": payload, "timestamp": stamp}
            records = [event("session_meta", {"id": "session", "cwd": d, "source": "cli"}),
                       event("turn_context", {"model": "gpt-6-astra", "effort": "medium"})]
            for call, cmd, stamp in [("old", "echo one", "2026-09-01T00:00:00Z"), ("a", "echo one", "2026-09-10T00:00:00Z"), ("b", "echo two", "2026-09-10T00:00:00Z"), ("c", "echo one", "2026-09-10T00:00:00Z")]:
                records.append(event("response_item", {"type": "function_call", "name": "exec_command", "call_id": call, "arguments": json.dumps({"cmd": cmd})}, stamp))
            for rid in ("r1", "r2", "r2"):
                records.append(event("token_usage_record", {"response_id": rid, "thread_id": "thread", "usage": {"input_tokens": 10, "output_tokens": 4, "reasoning_output_tokens": 2}}))
            (Path(d)/"actual.jsonl").write_text("\n".join(map(json.dumps, records)))
            out = codex_usage.scan(d, codex_usage._time("2026-09-09"), repo=d)
            self.assertEqual(out["responses"], 2)
            self.assertEqual(out["totals"]["reasoning"], 4)
            self.assertEqual(out["by_task"]["thread"]["responses"], 2)
            self.assertEqual(out["by_effort"]["gpt-6-astra/medium"]["responses"], 2)
            self.assertEqual(len(out["repeated_commands"]), 1)
            self.assertEqual(out["repeated_commands"][0]["count"], 2)
            self.assertNotIn("echo one", json.dumps(out))
            self.assertEqual(codex_usage.scan(d, repo="/unrelated")["responses"], 0)

    def test_missing_response_ids_are_not_thread_deduplicated(self):
        with tempfile.TemporaryDirectory() as d:
            record = {"type": "token_usage_record", "payload": {"thread_id": "same", "usage": {"output_tokens": 2}}}
            (Path(d)/"missing.jsonl").write_text(json.dumps(record) + "\n" + json.dumps(record))
            self.assertEqual(codex_usage.scan(d)["responses"], 2)
            self.assertEqual(codex_usage.scan(d, repo=d)["responses"], 0)
            self.assertEqual(codex_usage.main(["--sessions-dir", d, "--since", "invalid"]), 2)

    def test_completed_nested_commands_are_authoritative_and_bounded(self):
        self.assertEqual(codex_usage._command_text(["python3", "-c", "print(1)"]), "python3 -c 'print(1)'")
        with tempfile.TemporaryDirectory(prefix="codex usage ") as d:
            cwd = Path(d).resolve()
            def completed(identifier, command, timestamp):
                return {"type": "event_msg", "timestamp": timestamp, "payload": {
                    "type": "item_completed", "thread_id": "s", "item": {
                        "type": "CommandExecution", "id": identifier, "cwd": cwd.as_uri(),
                        "command": ["/bin/zsh", "-lc", command], "status": "completed", "exit_code": 0}}}
            records = [{"type": "session_meta", "payload": {"id": "s", "cwd": str(cwd)}},
                       {"type": "response_item", "timestamp": "2026-09-10T02:00:00Z", "payload": {
                           "type": "function_call", "name": "exec_command", "call_id": "different-id", "arguments": {"cmd": "echo secret"}}},
                       completed("old", "echo secret", "2026-09-09T02:00:00Z"),
                       completed("one", "echo secret", "2026-09-10T02:00:00Z"),
                       completed("one", "echo secret", "2026-09-10T02:00:00Z"),
                       completed("two", "echo secret", "2026-09-10T03:00:00Z"),
                       completed("other", "echo different", "2026-09-10T03:00:00Z"),
                       completed("boundary", "echo secret", "2026-09-11T00:00:00Z")]
            (cwd / "actual.jsonl").write_text("\n".join(map(json.dumps, records)))
            result = codex_usage.scan(d, codex_usage._time("2026-09-10"), d, until=codex_usage._time("2026-09-11"))
            self.assertEqual(result["command_coverage"]["completed_records"], 3)
            self.assertEqual(result["command_coverage"]["fallback_direct_records"], 0)
            self.assertEqual(len(result["repeated_commands"]), 1)
            self.assertEqual(result["repeated_commands"][0]["count"], 2)
            self.assertEqual(result["repeated_commands"][0]["cwd"], str(cwd))
            self.assertNotIn("echo secret", json.dumps(result))
            self.assertEqual(codex_usage.scan(d, repo="/elsewhere")["command_coverage"]["completed_records"], 0)
            self.assertEqual(codex_usage.main(["--sessions-dir", d, "--since", "2026-09-11", "--until", "2026-09-10"]), 2)
if __name__ == "__main__": unittest.main()
