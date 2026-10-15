#!/usr/bin/env bash
# Live end-to-end for Phase 5 retrieval: lexical + hybrid search via HTTP.
# Ingest two docs (stub provider) → exact-term lexical query → vector query
# → hybrid fusion → metadata filter → update/delete propagation →
# restart rebuild. Fails fast on the first failed check.
set -euo pipefail

PORT=18101
DATA_DIR="$(mktemp -d)/polyvec-e2e-retrieval"
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
query() { # query <json> → response body
    curl -sf -X POST "$BASE/v1/collections/kb/query" \
        -H 'content-type: application/json' -d "$1"
}
hits() { # hits <json> → number of hits
    query "$1" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["hits"]))'
}
first_source() { # first_source <json> → source of rank-1 hit
    query "$1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["hits"][0]["source"])'
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
start_server() {
    POLYVEC_BIND="127.0.0.1:$PORT" \
    POLYVEC_DATA_DIR="$DATA_DIR" \
    POLYVEC_EMBEDDING_PROVIDER=stub \
    POLYVEC_STUB_DIMENSION=64 \
    "$BIN" >/tmp/polyvec-e2e-retrieval.log 2>&1 &
    SERVER_PID=$!
    for i in $(seq 1 50); do
        curl -sf "$BASE/healthz" >/dev/null 2>&1 && break
        sleep 0.2
    done
}

echo "building..."
cargo build -q -p polyvec-server --bin polyvec-server

echo "starting server (stub provider, dim 64)..."
start_server
trap 'kill $SERVER_PID 2>/dev/null || true' EXIT
check "server healthy" curl -sf "$BASE/healthz"

echo "creating collection (dim 64, cosine)..."
curl -sf -X POST "$BASE/v1/collections" \
  -H 'content-type: application/json' \
  -d '{"id":"kb","dimension":64,"metric":"cosine"}' >/dev/null
check "create collection" true

ingest_text() { # <source> <text> <metadata-json>
    curl -sf -X POST "$BASE/v1/collections/kb/ingest" \
      -H 'content-type: application/json' \
      -d "{\"source\":\"$1\",\"filename\":\"doc.txt\",\"mime\":\"text/plain\",\"content\":{\"text\":\"$2\"},\"metadata\":$3}" \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["job_id"])'
}

echo "ingesting two docs..."
JOB_A="$(ingest_text "src://a" "the xylophone orchestra plays nightly" '{"author":"roger"}')"
expect_eq "ingest A done" "$(wait_job "$JOB_A")" "done"
JOB_B="$(ingest_text "src://b" "quantum foam dynamics in plain words" '{"author":"ana"}')"
expect_eq "ingest B done" "$(wait_job "$JOB_B")" "done"

echo "lexical: exact-term query..."
expect_eq "lexical 'xylophone' → 1 hit" "$(hits '{"text":"xylophone","k":5,"mode":"lexical"}')" "1"
expect_eq "lexical hit is doc A" "$(first_source '{"text":"xylophone","k":5,"mode":"lexical"}')" "src://a"
expect_eq "lexical absent term → 0 hits" "$(hits '{"text":"no-such-term-zzz","k":5,"mode":"lexical"}')" "0"

echo "lexical needs no provider-side work: schema check..."
MODE="$(query '{"text":"xylophone","k":5,"mode":"lexical"}' | python3 -c 'import json,sys; j=json.load(sys.stdin); print(j["mode"], j["hits"][0]["score_kind"], j["hits"][0]["path"])')"
expect_eq "lexical schema" "$MODE" "lexical bm25 ['lexical']"

echo "vector: default mode is phase-4 behavior..."
expect_eq "default mode → 2 hits" "$(hits '{"text":"xylophone","k":5}')" "2"
MODE="$(query '{"text":"quantum foam","k":5}' | python3 -c 'import json,sys; j=json.load(sys.stdin); print(j["mode"], j["hits"][0]["score_kind"])')"
expect_eq "vector schema" "$MODE" "vector distance"

echo "hybrid: RRF combines both rankings..."
expect_eq "hybrid → 2 hits" "$(hits '{"text":"xylophone","k":5,"mode":"hybrid"}')" "2"
expect_eq "hybrid rank 1 is doc A (in both lists)" "$(first_source '{"text":"xylophone","k":5,"mode":"hybrid"}')" "src://a"
KIND="$(query '{"text":"xylophone","k":5,"mode":"hybrid"}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["hits"][0]["score_kind"])')"
expect_eq "hybrid score_kind" "$KIND" "rrf"

echo "filters: consistent across modes..."
for mode in vector lexical hybrid; do
    expect_eq "filter author=roger ($mode)" "$(hits "{\"text\":\"xylophone\",\"k\":5,\"mode\":\"$mode\",\"filter\":{\"author\":\"roger\"}}")" "1"
done
# 'xylophone' is only in doc A (author roger): filtering to ana → 0 hits lexically.
expect_eq "filter author=ana excludes (lexical)" "$(hits '{"text":"xylophone","k":5,"mode":"lexical","filter":{"author":"ana"}}')" "0"

echo "update: old term vanishes from lexical results..."
TEXT2="the updated text mentions quux instead"
JOB_A2="$(ingest_text "src://a" "$TEXT2" '{"author":"roger"}')"
expect_eq "re-ingest A done" "$(wait_job "$JOB_A2")" "done"
expect_eq "stale term gone" "$(hits '{"text":"xylophone","k":5,"mode":"lexical"}')" "0"
expect_eq "new term present" "$(hits '{"text":"quux","k":5,"mode":"lexical"}')" "1"

echo "delete: content absent from lexical results..."
DOC_ID="$(curl -sf "$BASE/v1/jobs/$JOB_B" | python3 -c 'import json,sys; print(json.load(sys.stdin)["affected"]["document_id"])')"
ENC_ID="${DOC_ID//#/%23}"
curl -sf -X DELETE "$BASE/v1/collections/kb/documents/$ENC_ID" >/dev/null
check "delete doc B" true
expect_eq "deleted term gone (lexical)" "$(hits '{"text":"quantum","k":5,"mode":"lexical"}')" "0"

echo "restart: lexical index rebuilds from the catalog..."
kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
sleep 0.5
start_server
check "server healthy after restart" curl -sf "$BASE/healthz"
expect_eq "lexical works after restart" "$(hits '{"text":"quux","k":5,"mode":"lexical"}')" "1"
expect_eq "deleted stays deleted after restart" "$(hits '{"text":"quantum","k":5,"mode":"lexical"}')" "0"

echo "---"
echo "passed: $pass, failed: $fail"
[ "$fail" -eq 0 ]
