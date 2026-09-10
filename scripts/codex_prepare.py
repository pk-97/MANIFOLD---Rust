#!/usr/bin/env python3
"""Prepare a compact, source-grounded brief for a worker."""
import argparse, json, os, shlex, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

def _load_manifest():
    return json.loads((HERE / "codex_subsystems.json").read_text())

def build(repo, paths, task, findings, acceptance):
    context = select_context(repo, paths)
    root, owned, matched, refs = Path(repo).resolve(), context["owned_paths"], context["subsystems"], context["references"]
    from codex_checks import build_plan
    plan = build_plan(root, owned)
    return {"task": task, "findings": findings, "owned_paths": owned,
            "subsystems": matched, "references": refs, "acceptance": acceptance,
            "checks": plan, "instructions": "Do not delegate or land; return edits and check results to the lead."}

def select_context(repo, paths):
    root = Path(repo).resolve()
    owned = []
    for raw in paths:
        p = (root / raw).resolve() if not os.path.isabs(raw) else Path(raw).resolve()
        if not p.is_relative_to(root):
            raise ValueError(f"path escapes repository: {raw}")
        owned.append(p.relative_to(root).as_posix())
    selected = [s for s in _load_manifest()["subsystems"] if any(any((x.endswith("/") and p.startswith(x)) or (not x.endswith("/") and p == x) for x in s["prefixes"]) for p in owned)]
    references = [ref for s in selected for field in ("entry_points", "docs") for ref in s[field]]
    for ref in references:
        if not (root / ref).is_file():
            raise ValueError(f"manifest reference missing: {ref}")
    return {"owned_paths": owned, "subsystems": selected, "references": references, "unmapped": not selected}

def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True); ap.add_argument("--path", action="append", required=True)
    ap.add_argument("--task", required=True); ap.add_argument("--findings", required=True)
    ap.add_argument("--acceptance", required=True); ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)
    try: result = build(args.repo, args.path, args.task, args.findings, args.acceptance)
    except (ValueError, RuntimeError, OSError) as e: ap.error(str(e))
    if args.json: print(json.dumps(result, indent=2)); return 0
    print(f"Task: {result['task']}\nFindings: {result['findings']}\nOwned paths: {', '.join(result['owned_paths'])}")
    if result["subsystems"]:
        for s in result["subsystems"]: print(f"Subsystem: {s['name']}\nEntry points/docs: {', '.join(s['entry_points'] + s['docs'])}\nInvariant: {s['invariants']}")
    else: print("Subsystem: unmapped (no manifest match; do not guess)")
    print(f"Acceptance: {result['acceptance']}\nInstructions: {result['instructions']}")
    for check in result["checks"]["checks"]:
        print(f"Check (cwd {check['cwd']}): {shlex.join(check['argv'])}")
    for warning in result["checks"]["warnings"]:
        print(f"Warning: {warning}")
    for evidence in result["checks"].get("regressions", []):
        print(f"Regression evidence: {evidence['name']} — {', '.join(evidence['tests'])} ({evidence['source']})")
    if not result["checks"]["checks"]:
        print("No mapped worker checks; lead must choose validation for this scope.")
    return 0
if __name__ == "__main__": sys.exit(main())
