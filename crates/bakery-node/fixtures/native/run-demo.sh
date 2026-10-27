#!/bin/sh
# Executable mock-backed native-runtime demo (BAKER-NATIVE-CHAT-CODE-02).
#
# Runs the EXACT released, integrity-frozen, Bakery-PATCHED Claude adapter+engine
# over ACP with permission prompts ON (no bypass) and proves, with synthetic
# fixtures, operation-correlated properties:
#   A. a proposed Edit reaches the client permission/Review point BEFORE any write,
#      and the file is UNCHANGED while the approval is held;
#   B. rejection produces no mutation;
#   C. acceptance yields exactly ONE logical native write, performed in the engine
#      process, with NO duplicate client-delegated write;
#   D. edited-accept (Bakery's bakery.dev/review patch) makes the native tool write
#      the human-edited bytes exactly once;
#   E. the Bash build step actually ran and produced its result.
#
# QUALIFICATIONS (do not overclaim): provider traffic is loopback-REDIRECTED via
# ANTHROPIC_BASE_URL, which is URL redirection, NOT enforced network isolation.
# The credential is SYNTHETIC and the config dir is fresh, so the engine's
# Keychain lookup targets a distinct non-existent service name and no write
# occurs under a far-future-expiry token; that is a synthetic-NAMING argument,
# NOT a measured absence of Keychain access. Filesystem confinement (cross-project
# denial) is a property of Bakery's Seatbelt spawn, NOT of the adapter/engine
# alone, so it is proven by the Bakery-path integration test, not here.
#
# Exit: 0 = PASS; 1 = a proof FAILED, or (in strict mode) the environment could
# not run it; 2 = environment unavailable (only when NOT strict). Strict mode is
# selected by BAKERY_NATIVE_STRICT=1 and makes missing deps / assembly / driver
# errors / timeouts FAIL rather than skip.
set -eu
MOCK=""
HERE="$(cd "$(dirname "$0")" && pwd)"
STRICT="${BAKERY_NATIVE_STRICT:-0}"
unavailable() { # $1 = message
  if [ "$STRICT" = "1" ]; then echo "FAIL(strict): $1"; exit 1; else echo "SKIP: $1"; exit 2; fi
}
command -v node >/dev/null || unavailable "node not found"
command -v npm  >/dev/null || unavailable "npm not found"
command -v git  >/dev/null || unavailable "git not found"
[ -f "$HERE/runtime/package.json" ] && [ -f "$HERE/runtime/package-lock.json" ] || unavailable "frozen lockfile missing"
PATCH="$(cd "$HERE/../../../.." && pwd)/bakery-client/patches/@agentclientprotocol__claude-agent-acp@0.76.0.patch"
[ -f "$PATCH" ] || unavailable "Bakery adapter patch not found at $PATCH"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/bakery-native-demo.XXXXXX")"
cleanup() { [ -n "${MOCK:-}" ] && kill -TERM "$MOCK" 2>/dev/null || true; }
trap cleanup EXIT

# --- Phase 1: ASSEMBLE (integrity-frozen install + Bakery patch). Separate from run. ---
cp "$HERE/runtime/package.json" "$HERE/runtime/package-lock.json" "$WORK/" || unavailable "cannot stage runtime manifest"
( cd "$WORK" && npm ci --no-audit --no-fund --silent ) || unavailable "npm ci failed (offline or integrity mismatch)"
ADAPTER="$WORK/node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js"
ACP_SDK="$WORK/node_modules/@agentclientprotocol/sdk/dist/acp.js"
[ -f "$ADAPTER" ] && [ -f "$ACP_SDK" ] || unavailable "assembled runtime incomplete"
# Apply Bakery's real adapter patch (bakery.dev/review). Fail hard if it will not apply.
( cd "$WORK/node_modules/@agentclientprotocol/claude-agent-acp" && git apply --unsafe-paths -p1 "$PATCH" ) \
  || { echo "FAIL: Bakery adapter patch did not apply cleanly"; exit 1; }
grep -q "bakery.dev/review" "$WORK/node_modules/@agentclientprotocol/claude-agent-acp/dist/acp-agent.js" \
  || { echo "FAIL: patch marker absent after apply"; exit 1; }
echo "runtime assembled + patched:"
echo "  adapter $(node -p "require('$WORK/node_modules/@agentclientprotocol/claude-agent-acp/package.json').version")  (bakery.dev/review patch applied)"
echo "  acp-sdk $(node -p "require('$WORK/node_modules/@agentclientprotocol/sdk/package.json').version")"
echo "  agent-sdk $(node -p "require('$WORK/node_modules/@anthropic-ai/claude-agent-sdk/package.json').version")"

FUTURE=$(( ($(date +%s) + 315360000) * 1000 ))
synth_home() { d="$(mktemp -d "$WORK/home.XXXX")"; printf '{"claudeAiOauth":{"accessToken":"synthetic-access-not-a-real-token","refreshToken":"synthetic-refresh","expiresAt":%s,"scopes":["user:inference"],"subscriptionType":"max"}}' "$FUTURE" > "$d/.credentials.json"; chmod 600 "$d/.credentials.json"; echo "$d"; }

# $1=home $2=project $3=perm $4=hold_ms $5=edit_mode ; prints OBS json
run_case() {
  : > "$WORK/port.txt"
  MOCK_FILE="$2/target.txt" node "$HERE/mock-tools.mjs" > "$WORK/port.txt" 2>"$WORK/mock.err" & MOCK=$!
  i=0; while [ $i -lt 25 ]; do grep -q PORT= "$WORK/port.txt" 2>/dev/null && break; sleep 0.2; i=$((i+1)); done
  PORT="$(sed -n 's/PORT=//p' "$WORK/port.txt")"
  [ -n "$PORT" ] || { echo "FAIL: mock did not start"; exit 1; }
  OUT="$(env HOME="$1" CLAUDE_CONFIG_DIR="$1" ANTHROPIC_BASE_URL="http://127.0.0.1:$PORT" \
    ADAPTER="$ADAPTER" ACP_SDK="$ACP_SDK" PROJECT="$2" DRIVER_FS=1 DRIVER_PERM="$3" \
    DRIVER_HOLD_MS="${4:-0}" DRIVER_EDIT="${5:-plain}" \
    node "$HERE/acp-driver.mjs" 2>"$WORK/drv.err")" || { echo "FAIL: driver crashed"; cat "$WORK/drv.err" >&2; exit 1; }
  kill -TERM "$MOCK" 2>/dev/null || true; MOCK=
  printf '%s\n' "$OUT" | sed -n 's/^OBS=//p'
}
get() { node -e "let o=JSON.parse(process.argv[1]); let v=o; for(const k of process.argv[2].split('.')) v=v?.[k]; console.log(typeof v==='object'?JSON.stringify(v):String(v))" "$1" "$2"; }
FAIL=0
check() { if [ "$2" = "$3" ]; then echo "  PASS: $1"; else echo "  FAIL: $1 (got '$2' want '$3')"; FAIL=1; fi; }

echo "== Case A: acceptance, approval HELD 800ms, edited-accept =="
HA="$(synth_home)"; PA="$(mktemp -d "$WORK/projA.XXXX")"; printf 'ORIGINAL content here\n' > "$PA/target.txt"
OA="$(run_case "$HA" "$PA" approve 800 edited)"
[ -n "$OA" ] || { echo "FAIL: no OBS from case A"; exit 1; }
check "edit reached Review point"       "$(printf '%s' "$OA" | grep -c 'Edit target.txt')" "1"
check "review _meta present (patched)"   "$(get "$OA" 'permission_requests.0.review_meta.version')" "1"
check "file unchanged while held"        "$(get "$OA" 'content_while_held')" "ORIGINAL content here"
check "file unchanged after 800ms hold"  "$(get "$OA" 'content_after_hold')" "ORIGINAL content here"
check "no duplicate client write"        "$(get "$OA" 'write_calls')" "[]"
check "one native in-process write"      "$(get "$OA" 'file_changed_by')" "engine(in-process)"
check "edited-accept bytes written"      "$(get "$OA" 'final_content')" "EDITED_BY_HUMAN_REVIEW content here"
check "build step ran + produced result" "$(get "$OA" 'build_artifact')" "BUILD_RAN"
check "turn completed (end_turn)"        "$(get "$OA" 'stop_reason')" "end_turn"

echo "== Case B: rejection => no mutation =="
HB="$(synth_home)"; PB="$(mktemp -d "$WORK/projB.XXXX")"; printf 'ORIGINAL content here\n' > "$PB/target.txt"
OB="$(run_case "$HB" "$PB" reject 0 plain)"
check "rejection wrote nothing"          "$(get "$OB" 'final_content')" "ORIGINAL content here"
check "rejection: no client write"       "$(get "$OB" 'write_calls')" "[]"

echo "== Case C: case A project still isolated from case B run =="
check "second project unaffected"        "$(cat "$PA/target.txt")" "EDITED_BY_HUMAN_REVIEW content here"

[ "$FAIL" = 0 ] && { echo "NATIVE DEMO: PASS"; exit 0; } || { echo "NATIVE DEMO: FAIL"; exit 1; }
