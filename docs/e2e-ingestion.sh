#!/usr/bin/env bash
# Live end-to-end: ingest a real HTML file via HTTP (stub provider) →
# text query → verify hits → re-ingest unchanged (no dupes) → update →
# delete. Fails fast on the first failed check.
set -euo pipefail

PORT=18099
DATA_DIR="$(mktemp -d)/polyvec-e2e"
BIN="./target/debug/polyvec-server"
BASE="http://127.0.0.1:$PORT"

pass=0
fail=0
check() { # check <name> <command...>
    local name="$1"; shift
    if "$@" >/dev/null 2>&1; then
        pass=$((pass+1)); echo "ok   - $name"
    else
        fail=$((fail+1)); echo "FAIL - $name"
    fi
}
expect_eq() { # expect_eq <name> <got> <want>
    if [ "$2" = "$3" ]; then
        pass=$((pass+1)); echo "ok   - $1"
    else
        fail=$((fail+1)); echo "FAIL - $1 (got '$2', want '$3')"
    fi
}

echo "building..."
cargo build -q -p polyvec-server --bin polyvec-server

echo "starting server (stub provider, dim 64)..."
POLYVEC_BIND="127.0.0.1:$PORT" \
POLYVEC_DATA_DIR="$DATA_DIR" \
POLYVEC_EMBEDDING_PROVIDER=stub \
POLYVEC_STUB_DIMENSION=64 \
"$BIN" >/tmp/polyvec-e2e.log 2>&1 &
SERVER_PID=$!
trap 'kill $SERVER_PID 2>/dev/null || true' EXIT

for i in $(seq 1 50); do
    curl -sf "$BASE/healthz" >/dev/null 2>&1 && break
    sleep 0.2
done
check "server healthy" curl -sf "$BASE/healthz"

echo "creating collection (dim 64, cosine)..."
curl -sf -X POST "$BASE/v1/collections" \
  -H 'content-type: application/json' \
  -d '{"id":"kb","dimension":64,"metric":"cosine"}' >/dev/null
check "create collection" true

HTML='<html><head><title>Rust guide</title></head><body><h1>Ownership</h1><p>Ownership, borrowing, and lifetimes are the heart of Rust.</p><h2>Async</h2><p>Tokio powers async Rust.</p></body></html>'
B64="$(printf '%s' "$HTML" | base64 -w0)"

submit_ingest() { # <source> <base64>
    curl -sf -X POST "$BASE/v1/collections/kb/ingest" \
      -H 'content-type: application/json' \
      -d "{\"source\":\"$1\",\"filename\":\"guide.html\",\"mime\":\"text/html\",\"content\":{\"base64\":\"$2\"}}"
}

wait_job() { # <job_id> → prints state
    for _ in $(seq 1 100); do
        local state
        state="$(curl -sf "$BASE/v1/jobs/$1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"])')"
        if [ "$state" = "done" ] || [ "$state" = "failed" ]; then
            echo "$state"; return 0
        fi
        sleep 0.2
    done
    echo "timeout"; return 1
}

echo "ingesting HTML..."
JOB1="$(submit_ingest "https://example.com/rust" "$B64" | python3 -c 'import json,sys; print(json.load(sys.stdin)["job_id"])')"
expect_eq "ingest → 202 accepted" "$(wait_job "$JOB1")" "done"

echo "text query..."
HITS="$(curl -sf -X POST "$BASE/v1/collections/kb/query" \
  -H 'content-type: application/json' \
  -d '{"text":"rust ownership borrowing","k":5}' \
  | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["hits"]))')"
[ "$HITS" -gt 0 ] && check "query returns hits ($HITS)" true || check "query returns hits" false

echo "re-ingesting unchanged..."
JOB2="$(submit_ingest "https://example.com/rust" "$B64" | python3 -c 'import json,sys; print(json.load(sys.stdin)["job_id"])')"
wait_job "$JOB2" >/dev/null
INFO2="$(curl -sf "$BASE/v1/jobs/$JOB2" | python3 -c 'import json,sys; j=json.load(sys.stdin); print(j["affected"]["skipped"], j["affected"]["version"])')"
expect_eq "unchanged re-ingest skipped at version 1" "$INFO2" "True 1"

echo "updating with changed HTML..."
HTML2='<html><body><h1>Ownership</h1><p>Ownership, borrowing, lifetimes, and fearless concurrency.</p></body></html>'
B64_2="$(printf '%s' "$HTML2" | base64 -w0)"
JOB3="$(submit_ingest "https://example.com/rust" "$B64_2" | python3 -c 'import json,sys; print(json.load(sys.stdin)["job_id"])')"
wait_job "$JOB3" >/dev/null
INFO3="$(curl -sf "$BASE/v1/jobs/$JOB3" | python3 -c 'import json,sys; j=json.load(sys.stdin); print(j["affected"]["skipped"], j["affected"]["version"])')"
expect_eq "changed content bumps to version 2" "$INFO3" "False 2"

echo "deleting the document..."
DOC_ID="$(curl -sf "$BASE/v1/jobs/$JOB3" | python3 -c 'import json,sys; print(json.load(sys.stdin)["affected"]["document_id"])')"
ENC_ID="${DOC_ID//#/%23}"
curl -sf -X DELETE "$BASE/v1/collections/kb/documents/$ENC_ID" >/dev/null
check "delete document" true
HITS_AFTER="$(curl -sf -X POST "$BASE/v1/collections/kb/query" \
  -H 'content-type: application/json' \
  -d '{"text":"rust ownership borrowing","k":5}' \
  | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["hits"]))')"
expect_eq "no hits after delete" "$HITS_AFTER" "0"

echo "---"
echo "passed: $pass, failed: $fail"
[ "$fail" -eq 0 ]
