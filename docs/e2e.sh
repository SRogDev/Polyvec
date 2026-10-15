#!/usr/bin/env bash
# End-to-end smoke test for the polyvec-server HTTP API (Phase 3).
#
# Builds the server, starts it on 127.0.0.1:18080 with a temp data dir,
# drives every endpoint with curl (success AND error cases), then SIGTERMs
# the server, restarts it, and verifies the data survived (durability proof).
#
# Usage: ./docs/e2e.sh
# Requires: cargo, curl, python3. Exits non-zero on the first failed check.

set -euo pipefail

PORT=18080
BASE="http://127.0.0.1:$PORT"
DATA_DIR="$(mktemp -d /tmp/polyvec-e2e-data.XXXXXX)"
BIN=""

pass=0
fail=0

check() { # check <description> <expected-status> <curl-args...>
    local desc="$1"; local expected="$2"; shift 2
    local code
    code="$(curl -s -o /tmp/polyvec-e2e-body -w '%{http_code}' "$@")"
    if [[ "$code" == "$expected" ]]; then
        pass=$((pass + 1)); echo "ok   [$code] $desc"
    else
        fail=$((fail + 1)); echo "FAIL [$code != $expected] $desc"
        echo "     body: $(cat /tmp/polyvec-e2e-body)"
    fi
}

jget() { # jget <jq-like-python-expr>  — read a field from the last body
    python3 -c "import json,sys; print(json.load(open('/tmp/polyvec-e2e-body'))$1)"
}

cleanup() {
    [[ -n "${SERVER_PID:-}" ]] && kill "$SERVER_PID" 2>/dev/null || true
    [[ -n "$BIN" ]] || true
}
trap cleanup EXIT

echo "== building polyvec-server =="
BIN="$(cargo build -q --bin polyvec-server --message-format=json 2>/dev/null \
    | python3 -c "import json,sys; exes=[json.loads(l) for l in sys.stdin if l.strip()]; print([e['filenames'][0] for e in exes if e.get('reason')=='compiler-artifact' and 'polyvec-server' in e['target']['name'] and e['filenames']][0])")"
echo "   binary: $BIN"

start_server() {
    POLYVEC_BIND="127.0.0.1:$PORT" POLYVEC_DATA_DIR="$DATA_DIR" \
        RUST_LOG=warn "$BIN" & SERVER_PID=$!
    for _ in $(seq 1 50); do
        curl -sf "$BASE/healthz" >/dev/null 2>&1 && return 0
        sleep 0.2
    done
    echo "server did not become healthy"; exit 1
}

echo "== starting server (pid will be recorded) =="
start_server
echo "   pid: $SERVER_PID, data dir: $DATA_DIR"

echo "== health & readiness =="
check "GET /healthz" 200 "$BASE/healthz"
check "GET /readyz" 200 "$BASE/readyz"

echo "== collections =="
check "create collection (201)" 201 -X POST "$BASE/v1/collections" \
    -H 'content-type: application/json' \
    -d '{"id":"kb-e2e","dimension":8,"metric":"cosine","model":{"name":"e2e","version":"1"}}'
check "create duplicate (409)" 409 -X POST "$BASE/v1/collections" \
    -H 'content-type: application/json' -d '{"id":"kb-e2e","dimension":8,"metric":"cosine"}'
check "create bad id (400)" 400 -X POST "$BASE/v1/collections" \
    -H 'content-type: application/json' -d '{"id":"BAD","dimension":8,"metric":"cosine"}'
check "get collection (200)" 200 "$BASE/v1/collections/kb-e2e"
check "list collections (200)" 200 "$BASE/v1/collections"
check "get missing collection (404)" 404 "$BASE/v1/collections/nope"

echo "== vectors =="
check "insert batch (201)" 201 -X POST "$BASE/v1/collections/kb-e2e/vectors" \
    -H 'content-type: application/json' \
    -d '{"vectors":[{"id":"a","values":[1,0,0,0,0,0,0,0]},{"id":"b","values":[0,1,0,0,0,0,0,0]}]}'
check "insert single (201)" 201 -X POST "$BASE/v1/collections/kb-e2e/vectors" \
    -H 'content-type: application/json' -d '{"id":"c","values":[0,0,1,0,0,0,0,0]}'
check "insert dim mismatch (400)" 400 -X POST "$BASE/v1/collections/kb-e2e/vectors" \
    -H 'content-type: application/json' -d '{"vectors":[{"id":"x","values":[1,2]}]}'
check "insert duplicate (409)" 409 -X POST "$BASE/v1/collections/kb-e2e/vectors" \
    -H 'content-type: application/json' -d '{"vectors":[{"id":"a","values":[1,0,0,0,0,0,0,0]}]}'
check "search (200)" 200 -X POST "$BASE/v1/collections/kb-e2e/vectors/search" \
    -H 'content-type: application/json' -d '{"vector":[1,0,0,0,0,0,0,0],"k":2}'
TOP_HIT="$(jget "['hits'][0]['id']")"
[[ "$TOP_HIT" == "a" ]] && { pass=$((pass+1)); echo "ok   [hit=a] nearest-first ordering"; } \
    || { fail=$((fail+1)); echo "FAIL [hit=$TOP_HIT] nearest-first ordering"; }
check "search k=0 (400)" 400 -X POST "$BASE/v1/collections/kb-e2e/vectors/search" \
    -H 'content-type: application/json' -d '{"vector":[1,0,0,0,0,0,0,0],"k":0}'
check "get vector (200)" 200 "$BASE/v1/collections/kb-e2e/vectors/a"
check "update vector (200)" 200 -X PUT "$BASE/v1/collections/kb-e2e/vectors/c" \
    -H 'content-type: application/json' -d '{"values":[0,0,0,1,0,0,0,0]}'
check "delete vector (204)" 204 -X DELETE "$BASE/v1/collections/kb-e2e/vectors/c"
check "get deleted vector (404)" 404 "$BASE/v1/collections/kb-e2e/vectors/c"

echo "== documents =="
check "put document (200)" 200 -X PUT "$BASE/v1/collections/kb-e2e/documents" \
    -H 'content-type: application/json' \
    -d '{"source":"https://example.com/e2e","metadata":{"topic":"demo"},"chunks":[{"text":"hello e2e","char_start":0,"char_end":9}]}'
DOC_ID="$(jget "['id']")"
echo "   document id: $DOC_ID"
ENC_DOC_ID="${DOC_ID//#/%23}"
check "get document (200)" 200 "$BASE/v1/collections/kb-e2e/documents/$ENC_DOC_ID"
check "stats show 1 document (200)" 200 "$BASE/v1/collections/kb-e2e/stats"
check "flush (200)" 200 -X POST "$BASE/v1/collections/kb-e2e/flush"
check "error body shape" 404 "$BASE/v1/collections/kb-e2e/vectors/ghost"
python3 - << 'EOF'
import json
body = json.load(open('/tmp/polyvec-e2e-body'))
err = body.get("error", {})
assert set(err) == {"code", "message", "detail"}, f"bad shape: {err}"
assert err["code"] == "not_found", err
print("ok   [shape] error body has {code,message,detail}")
EOF
pass=$((pass + 1))

echo "== graceful shutdown & durability =="
kill -TERM "$SERVER_PID"
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
echo "   sent SIGTERM; server exited"
[[ -f "$DATA_DIR/kb-e2e/catalog.redb" ]] || { echo "FAIL catalog missing after shutdown"; fail=$((fail+1)); }

echo "== restart: data survives =="
start_server
check "collection still listed after restart (200)" 200 "$BASE/v1/collections"
check "vector still searchable after restart (200)" 200 -X POST "$BASE/v1/collections/kb-e2e/vectors/search" \
    -H 'content-type: application/json' -d '{"vector":[1,0,0,0,0,0,0,0],"k":1}'
TOP_HIT="$(jget "['hits'][0]['id']")"
[[ "$TOP_HIT" == "a" ]] && { pass=$((pass+1)); echo "ok   [hit=a] search works after restart"; } \
    || { fail=$((fail+1)); echo "FAIL [hit=$TOP_HIT] search after restart"; }
check "document still there after restart (200)" 200 "$BASE/v1/collections/kb-e2e/documents/$ENC_DOC_ID"

echo "== teardown =="
check "delete document (204)" 204 -X DELETE "$BASE/v1/collections/kb-e2e/documents/$ENC_DOC_ID"
check "delete collection (204)" 204 -X DELETE "$BASE/v1/collections/kb-e2e"
kill -TERM "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
rm -rf "$DATA_DIR"

echo
echo "==================================="
echo "passed: $pass, failed: $fail"
[[ "$fail" == 0 ]]
