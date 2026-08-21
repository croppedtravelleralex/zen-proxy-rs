#!/usr/bin/env python3
"""Configure NewAPI channel 69: models, abilities, free pricing for all Zenproxy public models."""
from __future__ import annotations

import subprocess
import sys
from datetime import datetime

SSH = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=20", "panda"]
CHANNEL_ID = 69
GROUPS = ("defualt", "oc")
# Public aliases from zen-proxy /v1/models (not upstream *-free ids)
MODELS = [
    "deepseek-v4-flash",
    "big-pickle",
    "mimo-v2.5",
    "hy3",
    "x-preview-f",
    "muse-spark-1.2-contributor",
    "nemotron-3-ultra",
    "nemotron-3.5-lightning",
    "laguna-s-2.1",
]
ENDPOINTS = '["anthropic","openai"]'

def psql_c(sql: str) -> str:
  esc = sql.replace("\\", "\\\\").replace('"', '\\"')
  return f'"${{PSQL[@]}}" -c "{esc}"'


def run_remote(script: str) -> str:
    proc = subprocess.run(
        SSH + ["bash", "-s"],
        input=script.encode(),
        capture_output=True,
        check=False,
    )
    out = proc.stdout.decode(errors="replace")
    err = proc.stderr.decode(errors="replace")
    if out:
        print(out, end="" if out.endswith("\n") else "\n")
    if err:
        print(err, file=sys.stderr, end="" if err.endswith("\n") else "\n")
    if proc.returncode != 0:
        raise SystemExit(proc.returncode)
    return out


def main() -> int:
    stamp = datetime.now().strftime("%Y%m%d_%H%M")
    models_csv = ",".join(MODELS)
    models_sql = "\n".join(
        psql_c(
            f"INSERT INTO models (model_name, status, endpoints) VALUES ('{m}', 1, '{ENDPOINTS}') "
            f"ON CONFLICT (model_name, deleted_at) DO UPDATE SET status=1, endpoints=EXCLUDED.endpoints;"
        )
        for m in MODELS
    )
    price_sql = ""
    for m in MODELS:
        price_sql += psql_c(
            f"UPDATE options SET value = ((value::jsonb || jsonb_build_object('{m}', 0))::text) WHERE key='ModelPrice';"
        ) + "\n"
        price_sql += psql_c(
            f"UPDATE options SET value = ((value::jsonb || jsonb_build_object('{m}', 0))::text) WHERE key='ModelRatio';"
        ) + "\n"
    abilities_sql = ""
    for g in GROUPS:
        for m in MODELS:
            abilities_sql += psql_c(
                f'INSERT INTO abilities ("group", model, channel_id, enabled, priority, weight) '
                f"VALUES ('{g}', '{m}', {CHANNEL_ID}, true, 100, 100) "
                f'ON CONFLICT ("group", model, channel_id) DO UPDATE SET enabled=true, priority=100, weight=100;'
            ) + "\n"

    script = f"""
set -euo pipefail
PSQL=(docker exec new-api-postgres psql -U newapi -d new-api -v ON_ERROR_STOP=1)
STAMP={stamp}
CHANNEL_ID={CHANNEL_ID}

echo "=== preflight channel ${{CHANNEL_ID}} ==="
"${{PSQL[@]}}" -At -c "SELECT id, name, type, status, \\"group\\", models FROM channels WHERE id=${{CHANNEL_ID}};"

echo "=== backup stamp=${{STAMP}} ==="
"${{PSQL[@]}}" -c "CREATE TABLE IF NOT EXISTS closeapi_channel69_backup_${{STAMP}}_free_models AS SELECT * FROM channels WHERE id=${{CHANNEL_ID}};"
"${{PSQL[@]}}" -c "CREATE TABLE IF NOT EXISTS closeapi_abilities_ch69_backup_${{STAMP}}_free_models AS SELECT * FROM abilities WHERE channel_id=${{CHANNEL_ID}};"
"${{PSQL[@]}}" -c "CREATE TABLE IF NOT EXISTS closeapi_options_pricing_backup_${{STAMP}}_free_models AS SELECT * FROM options WHERE key IN ('ModelPrice','ModelRatio');"

echo "=== update channel models list ==="
"${{PSQL[@]}}" -c "UPDATE channels SET models='{models_csv}' WHERE id=${{CHANNEL_ID}};"

echo "=== ensure models table active ==="
{models_sql}

echo "=== ensure abilities for groups {GROUPS} ==="
{abilities_sql}

echo "=== set ModelPrice/ModelRatio=0 ==="
{price_sql}

echo "=== verify pricing keys for our models ==="
"${{PSQL[@]}}" -At -c "SELECT key, value::jsonb -> '{MODELS[0]}', value::jsonb -> '{MODELS[-1]}' FROM options WHERE key IN ('ModelPrice','ModelRatio');"

echo "=== channel + abilities ==="
"${{PSQL[@]}}" -At -c "SELECT models FROM channels WHERE id=${{CHANNEL_ID}};"
"${{PSQL[@]}}" -At -c "SELECT \\"group\\", model, enabled FROM abilities WHERE channel_id=${{CHANNEL_ID}} ORDER BY model, \\"group\\";"

echo "=== restart new-api ==="
docker restart new-api
sleep 5
docker ps --filter name=new-api --format '{{{{.Names}}}} {{{{.Status}}}}'

echo DONE stamp=${{STAMP}}
"""
    print("=== CONFIGURE NEWAPI FREE MODELS ===")
    run_remote(script)
    print("=== DONE ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
