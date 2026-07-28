#!/usr/bin/env python3
"""claude_usage_export — put the Anthropic path on the fleet-value dashboard.

Claude Code talks to Anthropic directly, never through the litellm proxy, so
`LiteLLM_SpendLogs` cannot see the largest subscription by list-rate value. This reads
the per-message `usage` blocks Claude Code writes into its transcripts, prices them at
Anthropic's own card rate, and lands them in Postgres beside the proxy rows.

Only `claude-*` models are exported. Proxy models (k3, glm-*, deepseek-*) appear in the
same transcripts but are already metered in SpendLogs — exporting them double-counts.

Idempotent: keyed on the message uuid, so re-running picks up appended turns and never
duplicates. Drives `psql` rather than a driver: no Python Postgres binding is installed
and the litellm venv carries local patches that must not be disturbed.

  claude_usage_export.py                 export every transcript, refresh the view
  claude_usage_export.py --since-days 2  only transcripts touched in the last 2 days
  claude_usage_export.py --dry-run       print totals, write nothing
"""
import argparse
import glob
import json
import os
import subprocess
import sys
import time

DSN = os.environ.get(
    "MANIFOLD_LITELLM_DSN", "postgresql://litellm:litellm-local@localhost:5432/litellm"
)
TRANSCRIPTS = os.path.expanduser("~/.claude/projects/*/*.jsonl")

# Anthropic list rates, $ per million tokens: (input, output). Cache read bills at 0.1x
# input, a 5-minute cache write at 1.25x. Card rates as published 2026-07-26 — the
# dashboard's Anthropic column is only as current as this table.
RATES = {
    "claude-fable-5": (10.0, 50.0),
    "claude-mythos-5": (10.0, 50.0),
    "claude-opus-5": (5.0, 25.0),
    "claude-opus-4-8": (5.0, 25.0),
    "claude-opus-4-7": (5.0, 25.0),
    "claude-opus-4-6": (5.0, 25.0),
    "claude-opus-4-5": (5.0, 25.0),
    "claude-sonnet-5": (3.0, 15.0),
    "claude-sonnet-4-6": (3.0, 15.0),
    "claude-sonnet-4-5": (3.0, 15.0),
    "claude-haiku-4-5": (1.0, 5.0),
}
CACHE_READ_MULT = 0.1
CACHE_WRITE_MULT = 1.25

SCHEMA = """
CREATE TABLE IF NOT EXISTS manifold_anthropic_usage (
    message_uuid  text PRIMARY KEY,
    session_id    text NOT NULL,
    model         text NOT NULL,
    ts            timestamptz NOT NULL,
    project       text,
    input_tokens  bigint NOT NULL DEFAULT 0,
    cache_read    bigint NOT NULL DEFAULT 0,
    cache_write   bigint NOT NULL DEFAULT 0,
    output_tokens bigint NOT NULL DEFAULT 0,
    cost          double precision NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS manifold_anthropic_usage_ts_idx ON manifold_anthropic_usage (ts);

CREATE TABLE IF NOT EXISTS manifold_anthropic_errors (
    message_uuid text PRIMARY KEY,
    session_id   text NOT NULL,
    model        text NOT NULL DEFAULT '',
    ts           timestamptz NOT NULL,
    project      text
);
CREATE INDEX IF NOT EXISTS manifold_anthropic_errors_ts_idx ON manifold_anthropic_errors (ts);
"""

# One row shape per API call across every provider, so a single query answers a panel
# regardless of which subscription served the call. Per-call granularity is what makes
# the work-class split (permission classifier vs real agent turn) derivable.
VIEW = """
CREATE OR REPLACE VIEW manifold_fleet_usage AS
SELECT
    'proxy'::text AS source,
    CASE
        WHEN model_group LIKE 'k3%' OR model_group LIKE 'kimi%' THEN 'Kimi'
        WHEN model_group LIKE 'glm%'                            THEN 'Z.AI'
        WHEN model_group LIKE 'deepseek%'                       THEN 'OpenCode'
        ELSE 'Other'
    END AS provider,
    model_group AS model,
    "startTime" AT TIME ZONE 'UTC' AS ts,
    session_id,
    prompt_tokens::bigint,
    COALESCE((metadata->'usage_object'->>'cache_read_input_tokens')::bigint, 0) AS cache_read,
    COALESCE((metadata->'usage_object'->>'cache_creation_input_tokens')::bigint, 0) AS cache_write,
    completion_tokens::bigint AS output_tokens,
    spend AS cost,
    COALESCE(NULLIF(status, ''), 'success') AS status,
    request_duration_ms
FROM "LiteLLM_SpendLogs"
WHERE model_group IS NOT NULL AND model_group <> ''
UNION ALL
SELECT
    'anthropic'::text,
    'Anthropic',
    model,
    ts,
    session_id,
    (input_tokens + cache_read + cache_write)::bigint,
    cache_read,
    cache_write,
    output_tokens,
    cost,
    'success',
    NULL::int
FROM manifold_anthropic_usage;
"""

# Token shape is the only work-class signal available: SpendLogs stores no request body
# and CC sends no tags. A 7-output-token reply against a 60k prompt is the auto-mode
# permission classifier's stage-1 verdict; a real turn writes hundreds of tokens.
WORK_CLASS_VIEW = """
CREATE OR REPLACE VIEW manifold_fleet_work AS
SELECT *,
    CASE
        WHEN output_tokens <= 16 AND prompt_tokens > 20000 THEN 'permission-classifier'
        WHEN output_tokens <= 16                           THEN 'tiny-util'
        WHEN output_tokens < 100                           THEN 'short-reply'
        ELSE 'agent-turn'
    END AS work_class
FROM manifold_fleet_usage;
"""


def normalize_model(model):
    """Strip a dated snapshot suffix so claude-haiku-4-5-20251001 prices as its alias."""
    head, _, tail = model.rpartition("-")
    if head and len(tail) == 8 and tail.isdigit():
        return head
    return model


def price(model, input_tokens, cache_read, cache_write, output_tokens):
    rate = RATES.get(model)
    if rate is None:
        return 0.0
    rate_in, rate_out = rate
    return (
        input_tokens * rate_in
        + cache_read * rate_in * CACHE_READ_MULT
        + cache_write * rate_in * CACHE_WRITE_MULT
        + output_tokens * rate_out
    ) / 1_000_000


def scan(paths):
    # Keyed by message uuid, not appended: resuming or forking a session copies the
    # earlier turns verbatim into a new transcript, so the same call is on disk many
    # times over. Summing the files would inflate every figure on the dashboard.
    rows, unpriced = {}, set()
    for path in paths:
        project = os.path.basename(os.path.dirname(path))
        fallback_session = os.path.splitext(os.path.basename(path))[0]
        with open(path, errors="ignore") as handle:
            for line in handle:
                if '"usage"' not in line:
                    continue
                try:
                    record = json.loads(line)
                except json.JSONDecodeError:
                    continue
                message = record.get("message")
                if not isinstance(message, dict):
                    continue
                usage = message.get("usage")
                if not isinstance(usage, dict):
                    continue
                model = normalize_model(message.get("model") or "")
                if not model.startswith("claude-"):
                    continue
                if model not in RATES:
                    unpriced.add(model)
                uuid = record.get("uuid") or record.get("messageId")
                timestamp = record.get("timestamp")
                if not uuid or not timestamp:
                    continue
                counts = [
                    usage.get("input_tokens") or 0,
                    usage.get("cache_read_input_tokens") or 0,
                    usage.get("cache_creation_input_tokens") or 0,
                    usage.get("output_tokens") or 0,
                ]
                rows[uuid] = [
                    uuid,
                    record.get("sessionId") or fallback_session,
                    model,
                    timestamp,
                    project,
                    *counts,
                    f"{price(model, *counts):.10f}",
                ]
    return list(rows.values()), unpriced


def scan_errors(paths):
    # Same dedup rationale as scan(): keyed by message uuid, because a resumed or
    # forked session copies earlier turns — including their error records — verbatim
    # into a new transcript file.
    rows = {}
    for path in paths:
        project = os.path.basename(os.path.dirname(path))
        fallback_session = os.path.splitext(os.path.basename(path))[0]
        with open(path, errors="ignore") as handle:
            for line in handle:
                if '"isApiErrorMessage"' not in line:
                    continue
                try:
                    record = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if not record.get("isApiErrorMessage"):
                    continue
                uuid = record.get("uuid")
                timestamp = record.get("timestamp")
                if not uuid or not timestamp:
                    continue
                message = record.get("message")
                model = message.get("model") if isinstance(message, dict) else None
                if model is None:
                    # No model on the error record (e.g. it never reached the API) —
                    # keep it, but with no model to attribute cost/blame to.
                    model = ""
                elif model == "<synthetic>":
                    # Claude Code injects a synthetic assistant message when the call
                    # itself failed (rate limit, auth, timeout) — the very failures
                    # this table exists to surface. In practice every error record
                    # carries this, never a real model id. May include proxy-seat
                    # failures too; those are also visible live in Prometheus.
                    model = "synthetic"
                else:
                    model = normalize_model(model)
                    if not model.startswith("claude-"):
                        continue
                rows[uuid] = [
                    uuid,
                    record.get("sessionId") or fallback_session,
                    model,
                    timestamp,
                    project,
                ]
    return list(rows.values())


def _copy_payload(rows):
    return "".join(
        "\t".join(str(field).replace("\t", " ").replace("\n", " ") for field in row) + "\n"
        for row in rows
    )


def load(rows, error_rows):
    """COPY into temp tables, then upsert — one round trip, no per-row statements."""
    payload = _copy_payload(rows)
    errors_payload = _copy_payload(error_rows)
    # One explicit transaction: psql autocommits per statement, which would drop the
    # ON COMMIT DROP staging tables before the COPY could reach them.
    sql = f"""
BEGIN;
{SCHEMA}
CREATE TEMP TABLE staging (LIKE manifold_anthropic_usage) ON COMMIT DROP;
COPY staging (message_uuid, session_id, model, ts, project,
              input_tokens, cache_read, cache_write, output_tokens, cost)
     FROM STDIN;
{payload}\\.
INSERT INTO manifold_anthropic_usage
SELECT * FROM staging
ON CONFLICT (message_uuid) DO UPDATE SET
    input_tokens  = EXCLUDED.input_tokens,
    cache_read    = EXCLUDED.cache_read,
    cache_write   = EXCLUDED.cache_write,
    output_tokens = EXCLUDED.output_tokens,
    cost          = EXCLUDED.cost;

CREATE TEMP TABLE staging_errors (LIKE manifold_anthropic_errors) ON COMMIT DROP;
COPY staging_errors (message_uuid, session_id, model, ts, project)
     FROM STDIN;
{errors_payload}\\.
INSERT INTO manifold_anthropic_errors
SELECT * FROM staging_errors
ON CONFLICT (message_uuid) DO UPDATE SET
    session_id = EXCLUDED.session_id,
    model      = EXCLUDED.model,
    ts         = EXCLUDED.ts,
    project    = EXCLUDED.project;

{VIEW}
{WORK_CLASS_VIEW}
COMMIT;
"""
    result = subprocess.run(
        ["psql", DSN, "--quiet", "--no-psqlrc", "-v", "ON_ERROR_STOP=1", "-f", "-"],
        input=sql,
        text=True,
        capture_output=True,
    )
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        raise SystemExit(f"psql failed ({result.returncode})")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--since-days", type=float, help="only transcripts modified in the last N days")
    parser.add_argument("--dry-run", action="store_true", help="print totals, write nothing")
    args = parser.parse_args()

    paths = glob.glob(TRANSCRIPTS)
    if args.since_days is not None:
        cutoff = time.time() - args.since_days * 86400
        paths = [p for p in paths if os.path.getmtime(p) >= cutoff]

    rows, unpriced = scan(paths)
    error_rows = scan_errors(paths)
    total = sum(float(row[-1]) for row in rows)
    print(
        f"transcripts={len(paths)} calls={len(rows)} list_rate_cost=${total:,.2f} "
        f"api_errors={len(error_rows)}"
    )
    # Unpriced models still export at $0 — the launchd log needs a nonzero exit so a
    # rate gap doesn't rot silently until someone notices the dashboard undercounting.
    exit_code = 0
    if unpriced:
        print(f"WARNING: no rate for {sorted(unpriced)} — priced at $0, add to RATES", file=sys.stderr)
        exit_code = 1
    if args.dry_run:
        return exit_code
    if not rows and not error_rows:
        return exit_code

    load(rows, error_rows)
    print("loaded; views manifold_fleet_usage + manifold_fleet_work refreshed")
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
