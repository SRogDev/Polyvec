#!/usr/bin/env bash
# Live end-to-end for Phase 6 relations: typed relations over HTTP,
# relationship-aware retrieval (expand), bounds, and delete cascade.
# Uses PUT /documents + lexical mode: no embedding provider needed.
# Fails fast on the first failed check.
set -euo pipefail

PORT=18102
DATA_DIR="$(mktemp -d)/polyvec-e2e-relations"
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
urlencode() { # urlencode <s> → percent-encoded (for '#' in ids)
    python3 -c 'import sys,urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' "$1"
}
put_doc() { # put_doc <source> <text> → "doc_id chunk_id"
    curl -sf -X PUT "$BASE/v1/collections/kb/documents" \
        -H 'content-type: application/json' \
        -d "{\"source\":\"$1\",\"chunks\":[{\"text\":\"$2\",\"char_start\":0,\"char_end\":${#2}}]}" \
        | python3 -c 'import json,sys; j=json.load(sys.stdin); print(j["id"], j["chunk_ids"][0])'
}
create_rel() { # create_rel <json> → "status body"
    local code body
    code=$(curl -s -o /tmp/rel_body.json -w '%{http_code}' -X POST "$BASE/v1/collections/kb/relations" \
        -H 'content-type: application/json' -d "$1")
    body=$(cat /tmp/rel_body.json)
    echo "$code $body"
}

echo "building..."
cargo build -q -p polyvec-server --bin polyvec-server

echo "starting server..."
POLYVEC_BIND="127.0.0.1:$PORT" \
POLYVEC_DATA_DIR="$DATA_DIR" \
"$BIN" >/tmp/polyvec-e2e-relations.log 2>&1 &
SERVER_PID=$!
for i in $(seq 1 50); do
    curl -sf "$BASE/healthz" >/dev/null 2>&1 && break
    sleep 0.2
done
trap 'kill $SERVER_PID 2>/dev/null || true' EXIT
check "server healthy" curl -sf "$BASE/healthz"

echo "creating collection..."
curl -sf -X POST "$BASE/v1/collections" \
    -H 'content-type: application/json' \
    -d '{"id":"kb","dimension":64,"metric":"cosine"}' >/dev/null
check "create collection" true

echo "putting three docs (only A mentions the query term)..."
read -r DOC_A CHUNK_A <<<"$(put_doc "src://a" "the xylophone manual")"
read -r DOC_B CHUNK_B <<<"$(put_doc "src://b" "advanced techniques")"
read -r DOC_C CHUNK_C <<<"$(put_doc "src://c" "reference tables")"
check "docs put" true

echo "creating relations A→B, B→C..."
expect_eq "create A→B is 201" "$(create_rel "{\"type\":\"references\",\"source\":\"$DOC_A\",\"target\":\"$DOC_B\"}" | cut -d' ' -f1)" "201"
expect_eq "create B→C is 201" "$(create_rel "{\"type\":\"references\",\"source\":\"$DOC_B\",\"target\":\"$DOC_C\"}" | cut -d' ' -f1)" "201"

echo "relation validation..."
expect_eq "unknown type → 400" "$(create_rel "{\"type\":\"cites\",\"source\":\"$DOC_A\",\"target\":\"$DOC_B\"}" | cut -d' ' -f1)" "400"
expect_eq "self-loop → 400" "$(create_rel "{\"type\":\"references\",\"source\":\"$DOC_A\",\"target\":\"$DOC_A\"}" | cut -d' ' -f1)" "400"
expect_eq "unknown endpoint → 404" "$(create_rel "{\"type\":\"references\",\"source\":\"$DOC_A\",\"target\":\"nope\"}" | cut -d' ' -f1)" "404"
expect_eq "duplicate id → 409" "$(create_rel "{\"id\":\"dup\",\"type\":\"references\",\"source\":\"$DOC_A\",\"target\":\"$DOC_B\"}" | cut -d' ' -f1)" "201"
expect_eq "duplicate id again → 409" "$(create_rel "{\"id\":\"dup\",\"type\":\"references\",\"source\":\"$DOC_A\",\"target\":\"$DOC_B\"}" | cut -d' ' -f1)" "409"

echo "listing..."
COUNT=$(curl -sf "$BASE/v1/collections/kb/relations" | python3 -c 'import json,sys; print(json.load(sys.stdin)["count"])')
expect_eq "list count" "$COUNT" "3"
COUNT=$(curl -sf "$BASE/v1/collections/kb/relations?type=references&limit=1&offset=1" | python3 -c 'import json,sys; j=json.load(sys.stdin); print(j["count"], j["limit"], j["offset"])')
expect_eq "filter+pagination" "$COUNT" "1 1 1"

echo "relationship-aware retrieval..."
expect_eq "lexical alone → 1 hit (A only)" "$(hits '{"text":"xylophone","k":5,"mode":"lexical"}')" "1"
expect_eq "expand depth 2 → 3 hits" "$(hits '{"text":"xylophone","k":5,"mode":"lexical","expand":{"depth":2}}')" "3"
expect_eq "expand depth 1 → 2 hits" "$(hits '{"text":"xylophone","k":5,"mode":"lexical","expand":{"depth":1}}')" "2"
expect_eq "type filter excludes chain → 1 hit" "$(hits '{"text":"xylophone","k":5,"mode":"lexical","expand":{"relation_types":["supersedes"]}}')" "1"
EVIDENCE=$(query '{"text":"xylophone","k":5,"mode":"lexical","expand":{"depth":2}}' \
    | python3 -c 'import json,sys; h=json.load(sys.stdin)["hits"]; print(h[1]["evidence"]["hops"], h[1]["evidence"]["path"][0]["relation_type"], h[1]["evidence"]["path"][0]["direction"], h[2]["evidence"]["hops"], len(h[2]["evidence"]["path"]))')
expect_eq "evidence paths" "$EVIDENCE" "1 references outgoing 2 2"
KIND=$(query '{"text":"xylophone","k":5,"mode":"lexical","expand":{"depth":2}}' \
    | python3 -c 'import json,sys; h=json.load(sys.stdin)["hits"]; print(h[1]["score_kind"], h[1]["path"], h[0].get("evidence"))')
expect_eq "expanded hit schema" "$KIND" "relationship ['relationship'] None"
expect_eq "bad expand depth → 400" "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/v1/collections/kb/query" -H 'content-type: application/json' -d '{"text":"xylophone","k":5,"mode":"lexical","expand":{"depth":9}}')" "400"

echo "cycle: C→A, expansion still terminates..."
expect_eq "create C→A is 201" "$(create_rel "{\"type\":\"references\",\"source\":\"$DOC_C\",\"target\":\"$DOC_A\"}" | cut -d' ' -f1)" "201"
expect_eq "expand with cycle → 3 hits (each once)" "$(hits '{"text":"xylophone","k":5,"mode":"lexical","expand":{"depth":5}}')" "3"

echo "delete cascade: DELETE doc A..."
curl -sf -X DELETE "$BASE/v1/collections/kb/documents/$(urlencode "$DOC_A")" >/dev/null
check "delete doc A" true
COUNT=$(curl -sf "$BASE/v1/collections/kb/relations" | python3 -c 'import json,sys; print(json.load(sys.stdin)["count"])')
expect_eq "relations touching A gone (B→C survives)" "$COUNT" "1"
expect_eq "expand from B → B + C only" "$(hits '{"text":"advanced","k":5,"mode":"lexical","expand":{"depth":2}}')" "2"

echo
echo "pass=$pass fail=$fail"
[ "$fail" -eq 0 ]
