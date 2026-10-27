#!/usr/bin/env bash
# The final gate, as the operator runs it: the node's tests, the sandbox's
# tests and the client's checks, ending in a short brief. The tests run
# under scripts/test.sh, so each has a time limit: one still running at its
# limit fails as TIMEOUT, by name, and the rest still run. Each step's whole
# output goes to .bakery-dev-logs/gate-<step>.log; only what needs eyes shows
# live: a test stalled past 60 s or past its limit, a failure, a build error.
#
# The Mac stays awake while it runs (caffeinate). Closing the lid only pauses
# it, and it carries on when opened; a test that then times out is the clock
# jumping, so run the gate again before believing it.
#
#   ./scripts/gate.sh
set -uo pipefail
if [[ -z "${GATE_AWAKE:-}" ]]; then
  GATE_AWAKE=1 exec caffeinate -i "$0" "$@"
fi
cd "$(dirname "$0")/.."
logs=.bakery-dev-logs
mkdir -p "$logs"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
started=$SECONDS
brief=()
failed=0

# Runs one step: its whole output to its log, its stalls and failures on
# screen, then one line of the brief.
step() {
  local name="$1"
  shift
  local log="$logs/gate-$name.log" t=$SECONDS line
  echo "── $name"
  "$@" 2>&1 | tee "$log" | rg --line-buffered "running for over|FAILED|panicked|^error|^FAIL|✗|^TIMEOUT|^FLAKY"
  local code=${PIPESTATUS[0]} took=$((SECONDS - t))
  if [[ "$name" == client ]]; then
    line="$(rg -c "^pass:" "$log" || echo 0) passed, $(rg -c "^(fail|FAIL)" "$log" || echo 0) failed"
  else
    line=$(rg "^test result:" "$log" | awk '{p+=$4; f+=$6; i+=$8} END {printf "%d passed, %d failed, %d ignored", p, f, i}')
  fi
  if [[ $code -eq 0 ]]; then
    brief+=("ok    $name   $line   (${took}s)")
  else
    failed=1
    brief+=("FAIL  $name   $line   (${took}s)   log: $log")
    while read -r t; do brief+=("        $t"); done < <(rg "^(TIMEOUT|FLAKY) " "$log" | head -10)
    # A test that failed before a timeout's stop runs again: named once.
    while read -r t; do brief+=("        $t"); done < <(rg -o "^test (\S+) \.\.\. FAILED" -r '$1' "$log" | awk '!seen[$0]++' | head -10)
  fi
}

step node scripts/test.sh -p bakery-node
step exec scripts/test.sh -p bakery-exec
step client pnpm -C bakery-client check:all

echo
echo "Gate on $(git log --oneline -1 | cut -c1-60)   $((SECONDS - started))s"
printf '%s\n' "${brief[@]}"
if [[ $failed -eq 0 ]]; then echo "PASS: safe to deploy"; else echo "FAIL: don't deploy"; fi
exit $failed
