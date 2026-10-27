#!/usr/bin/env bash
# Bakery control: run, inspect, deploy, roll back and stop your own node and app.
#
# The app you use runs from a DEPLOYED BUILD — release binaries of bakery-node
# and of the desktop app (Bakery.app, Electron) in ~/.bakery/deploy/current, and
# the UI it shows in ~/.bakery/deploy/ui/<its hash>, the node's own home, where
# no agent can write — not from the source the Baker edits. Agent edits change
# the source only; the app changes when you deploy. Until the first deploy,
# start and restart run from source as before (dev mode).
#
# Usage:
#   ./scripts/bakery.sh                  check; if something is running ask reuse/kill/cancel; start what's missing
#   ./scripts/bakery.sh start            the same
#   ./scripts/bakery.sh status           what's running, the deployed build, and source changes not deployed yet
#   ./scripts/bakery.sh deploy           build node + app from the source; ONLY if both builds pass, switch to them and restart
#   ./scripts/bakery.sh deploy --checked the same, but `cargo test -p bakery-node` and the client's check:all must pass too
#   ./scripts/bakery.sh deploy --ui      build only the UI and swap it in: the open app checks it and reloads, nothing restarts
#   ./scripts/bakery.sh rollback         switch back to the previous deployed build; restart node + app
#   ./scripts/bakery.sh rollback --ui    put back the UI shown before the last `deploy --ui` or install from the app;
#                                        nothing restarts, and running it again undoes it
#   ./scripts/bakery.sh upgrade-adapter <version>
#                                        upgrade the Claude native adapter: pin <version>, move the Bakery patch to it,
#                                        install, check its handshake, then deploy --checked. Any failure puts it all
#                                        back; exit 3 means the Bakery patch needs porting to <version> first
#   ./scripts/bakery.sh restart          restart the node on the deployed build (no rebuild — that is deploy)
#   ./scripts/bakery.sh restart --fg     the same, but the node runs in this terminal (logs here, Ctrl-C stops it)
#   ./scripts/bakery.sh restart client   restart only the app
#   ./scripts/bakery.sh restart all      node, then app
#   ./scripts/bakery.sh dev              your own UI work: rebuild the debug node, swap it in ONLY if green, and run
#                                        the desktop app against Vite (hot reload); run it again to pick up Rust edits
#   ./scripts/bakery.sh kill             stop node + app, start nothing
#   ./scripts/bakery.sh service install  run the deployed node as a per-user LaunchAgent: macOS starts it at login and
#                                        again after a crash, never after a clean stop; the app opens when you open it
#   ./scripts/bakery.sh service remove   the node no longer starts at login
#   ./scripts/bakery.sh --yes            non-interactive: kill whatever runs, then start fresh
#   ./scripts/bakery.sh --reuse          non-interactive: keep whatever runs, start only what's missing
#   ./scripts/bakery.sh fresh-start      a new, clean Heart carrying only the Baker thread "local models", under a new
#                                        permanent Baker, with the Always-allow grants cleared. Shows what it keeps and
#                                        leaves, asks first, refuses while a turn runs, then restarts the node into it.
#                                        The old Heart is kept aside, closed, for you to delete; worktrees stay on disk
#   ./scripts/bakery.sh fresh-start --keep <stream_id>   the same, keeping that Baker thread
#   ./scripts/bakery.sh uninstall        remove what Bakery made; asks whether to keep your work
#   ./scripts/bakery.sh uninstall --keep        keep projects + worktrees + chats + conversation history (moved to ~/Bakery-kept-<date>)
#   ./scripts/bakery.sh uninstall --everything  delete all of ~/.bakery
#   ./scripts/bakery.sh help             this text
#
# Deploy builds into ~/.bakery/deploy/next and switches only when every step
# passed, so a failed build leaves the running app untouched. The build it
# replaces is kept as ~/.bakery/deploy/previous for rollback, and a new node
# that never answers is rolled back by itself. One deploy, rollback or adapter
# upgrade runs at a time (~/.bakery/deploy/.lock; a lock whose owner is gone is
# taken over). Settings → Versions has the same Deploy, Restart, Roll back and
# adapter Upgrade as buttons: the node runs the deployed copy of this script
# (~/.bakery/deploy/current/bakery.sh — the working tree's copy is
# agent-editable) detached, logging to .bakery-dev-logs/control-<action>.log.
#
# A restart, deploy or rollback first lets the node finish its running turns
# (SIGUSR1; for as long as the node allows, while Settings offers Restart
# now); queued messages wait for the node that comes back, and a turn it
# still cuts off carries on by itself. Your open app reconnects on its own
# (the operator token persists in node.json); deploy and rollback replace the
# app window too. Run them from the buttons or an outside terminal: Bakery's
# own terminal pane closes with the node it belongs to. The node logs to
# ~/.bakery/logs/node.log, keeping the four files before it.
#
# Starting never kills anything without asking (or --yes). Playwright runs its
# own isolated node/vite on random ports with a scratch HOME, so it never
# collides with anything this script starts.
#
# Env:
#   BAKERY_NODE_ADDR   node bind address (default 127.0.0.1:7717, same as bakery-node's own default)
#   BAKERY_DRY_RUN=1   print each command that would build, move, start or stop something; run none of them
#   HOME               like bakery-node, everything lives in $HOME/.bakery (a scratch HOME isolates a test run)
#   BAKERY_ROOT        set by the node for a Settings button: the checkout, since the deployed copy lives outside it

set -euo pipefail

ROOT="${BAKERY_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
unset BAKERY_ROOT
ADDR="${BAKERY_NODE_ADDR:-127.0.0.1:7717}"
PORT="${ADDR##*:}"
LOG_DIR="$ROOT/.bakery-dev-logs"
DEV_NODE_BIN="$ROOT/target/debug/bakery-node"
# Resolved exactly as bakery-node resolves its own home.
BAKERY_HOME="${HOME:?}/.bakery"
DEPLOY="$BAKERY_HOME/deploy"
LOCK="$DEPLOY/.lock"
# `electron:pack` leaves Bakery.app under here (electron/builder.json).
ELECTRON_OUT="$ROOT/bakery-client/electron/out"
# What a deploy builds, as git pathspecs. bakery-node's control.rs keeps the
# same list to count the source changes Settings shows as not deployed.
DEPLOY_SOURCES=(crates Cargo.toml Cargo.lock bakery-client scripts/bakery.sh ':(exclude)bakery-client/e2e')
# The node's own log (it keeps the four before it beside it).
NODE_LOG="$BAKERY_HOME/logs/node.log"
# The node as a per-user LaunchAgent (`service install`).
SERVICE_LABEL="dev.bakery.node"
SERVICE_PLIST="$HOME/Library/LaunchAgents/$SERVICE_LABEL.plist"
SERVICE_DOMAIN="gui/$(id -u)"
# How long a stop waits for running turns: the node's own limit, and a little.
RESTART_WAIT=630

# Every command that builds, moves, starts or stops something goes through
# here, so BAKERY_DRY_RUN=1 shows the whole plan without touching anything.
run() {
  if [[ -n "${BAKERY_DRY_RUN:-}" ]]; then
    echo "+ $*"
  else
    "$@"
  fi
}

# On every exit: put back an adapter upgrade that did not finish, release the
# deploy lock if this run holds it, and hand the exit code to the node that
# started the run for a Settings button — that node is usually stopped by the
# run, so the one that comes back reads this file. bash 3.2 runs the EXIT trap
# with $? = 0 when killed, hence the signal trap.
CONTROL_EXIT="${BAKERY_CONTROL_EXIT:-}"
unset BAKERY_CONTROL_EXIT
LOCK_HELD=""
UPGRADE_BACKUP=""
# A fresh start armed by this run: still there at exit, no node took it up,
# so it goes, and nothing changed.
FRESH_ARMED=""
on_exit() {
  local code=$?
  if [[ -n "$UPGRADE_BACKUP" ]]; then restore_adapter || true; fi
  if [[ -n "$FRESH_ARMED" && -f "$FRESH_ARMED" ]]; then
    rm -f "$FRESH_ARMED"
    echo "The fresh start was put away: no node took it up, so nothing changed." >&2
  fi
  if [[ "$LOCK_HELD" == 1 ]]; then rm -rf "$LOCK"; fi
  if [[ -n "$CONTROL_EXIT" ]]; then echo "$code" >"$CONTROL_EXIT"; fi
}
trap on_exit EXIT
trap 'exit 143' HUP INT TERM

# ---- discovery ---------------------------------------------------------

# The node binds $PORT; that's the one reliable signal (no pidfile exists).
node_pid() {
  lsof -nP -iTCP:"$PORT" -sTCP:LISTEN -t 2>/dev/null | head -1 || true
}

# This repo's dev app — Vite and Electron from its own node_modules — matched
# by path, so another project's dev session is never touched.
dev_client_pids() {
  pgrep -f "$ROOT/bakery-client/node_modules/.*vite" 2>/dev/null || true
  pgrep -f "$ROOT/bakery-client/node_modules/.*[Ee]lectron" 2>/dev/null || true
}

# This repo's deployed app, from whichever slot it was started in (a deploy
# renames current → previous under the running app).
app_pids() {
  pgrep -f "$DEPLOY/[a-z]*/bakery-client" 2>/dev/null || true
  pgrep -f "$DEPLOY/[a-z]*/Bakery.app/" 2>/dev/null || true
}

client_pids() {
  dev_client_pids
  app_pids
}

# Node and app present: a deploy only ever switches in a complete build.
deployed() {
  [[ -x "$DEPLOY/current/bakery-node" && (-d "$DEPLOY/current/Bakery.app" || -x "$DEPLOY/current/bakery-client") ]]
}

node_is_deployed() {
  [[ "$(ps -o command= -p "$1" 2>/dev/null)" == "$DEPLOY/"* ]]
}

describe_pid() {
  ps -o pid=,etime=,command= -p "$1" 2>/dev/null |
    awk '{cmd=""; for(i=3;i<=NF;i++) cmd=cmd" "$i; printf "  pid %-8s up %-10s %s\n", $1, $2, cmd}' | cut -c1-140
}

# Seconds since a process started (`ps` etime is [[dd-]hh:]mm:ss).
proc_age() {
  ps -o etime= -p "$1" 2>/dev/null | awk '{
    n = split($1, a, /[-:]/); s = 0
    if (n == 4) s = a[1]*86400 + a[2]*3600 + a[3]*60 + a[4]
    else if (n == 3) s = a[1]*3600 + a[2]*60 + a[3]
    else if (n == 2) s = a[1]*60 + a[2]
    print s }'
}

# First Rust/Cargo source changed since a node run from source started —
# `dev` picks it up. Compared to process start, not the binary on disk: a test
# build can refresh the binary while the old node keeps running.
stale_source() {
  local pid="$1" ref start stamp
  ref="$(mktemp "${TMPDIR:-/tmp}/bakery-ref.XXXXXX")"
  start=$(( $(date +%s) - $(proc_age "$pid") ))
  stamp="$(date -r "$start" +%Y%m%d%H%M.%S 2>/dev/null || date -d "@$start" +%Y%m%d%H%M.%S)"
  touch -t "$stamp" "$ref"
  find "$ROOT/crates" \( -name '*.rs' -o -name 'Cargo.toml' \) -newer "$ref" -print 2>/dev/null | head -1
  [[ "$ROOT/Cargo.lock" -nt "$ref" ]] && echo "$ROOT/Cargo.lock"
  rm -f "$ref"
  return 0
}

# One field of a build.json this script wrote (one flat line, so no jq).
build_field() {
  sed -nE "s/.*\"$2\":\"?([^\",}]*).*/\1/p" "$1/build.json" 2>/dev/null || true
}

describe_build() {
  local dirty
  dirty="$(build_field "$1" dirty_files)"
  printf '%s' "$(build_field "$1" commit)"
  if [[ -n "$dirty" && "$dirty" != 0 ]]; then printf ' + %s uncommitted files' "$dirty"; fi
  echo ", built $(build_field "$1" built_at)"
}

# Source files changed after the deployed build began. build.json is written
# first, so edits made while it built count as not deployed.
undeployed() {
  local ref="$DEPLOY/current/build.json"
  [[ -f "$ref" ]] || return 0
  git -C "$ROOT" ls-files -z --cached --others --exclude-standard -- "${DEPLOY_SOURCES[@]}" 2>/dev/null |
    while IFS= read -r -d '' f; do
      if [[ "$ROOT/$f" -nt "$ref" ]]; then echo "$f"; fi
    done || true
}

report() {
  local node client stale changes
  node="$(node_pid)"
  client="$(client_pids)"
  echo "Bakery status"
  echo "============="
  if [[ -z "$node" ]]; then
    echo "node    not running (port $PORT free)"
  elif node_is_deployed "$node"; then
    echo "node    LISTENING on $ADDR — deployed build"
    describe_pid "$node"
  else
    echo "node    LISTENING on $ADDR — from source (dev)"
    describe_pid "$node"
    stale="$(stale_source "$node" | head -1)"
    if [[ -n "$stale" ]]; then
      echo "        older than its source: ${stale#"$ROOT/"} changed since it started — ./scripts/bakery.sh dev"
    fi
  fi
  if service_installed; then
    echo "service $SERVICE_LABEL — macOS starts the node at login and again after a crash"
    if [[ -n "$node" ]] && service_ours && [[ "$node" != "$(service_field pid)" ]]; then
      echo "        the node on $PORT is not the service's own process, so the service keeps failing to start — ./scripts/bakery.sh restart puts it right"
    fi
  else
    echo "service not installed — ./scripts/bakery.sh service install starts the node at login"
  fi
  if [[ -n "$client" ]]; then
    echo "app     running"
    while read -r pid; do [[ -n "$pid" ]] && describe_pid "$pid"; done <<<"$client"
  else
    echo "app     not running"
  fi
  if ! deployed; then
    echo "deploy  nothing deployed yet — the app runs from source; ./scripts/bakery.sh deploy builds one"
    return 0
  fi
  echo "deploy  $(describe_build "$DEPLOY/current")"
  if [[ -d "$DEPLOY/previous" ]]; then
    echo "        previous: $(describe_build "$DEPLOY/previous") — ./scripts/bakery.sh rollback"
  fi
  changes="$(undeployed)"
  if [[ -n "$changes" ]]; then
    echo "        $(grep -c . <<<"$changes") source changes not deployed yet, e.g. $(head -1 <<<"$changes") — ./scripts/bakery.sh deploy"
  else
    echo "        no source changes since this build"
  fi
}

# ---- stop --------------------------------------------------------------

service_installed() { [[ -f "$SERVICE_PLIST" ]]; }
service_loaded() { launchctl print "$SERVICE_DOMAIN/$SERVICE_LABEL" >/dev/null 2>&1; }
# One field of the loaded service, as launchd has it: program, pid, path.
service_field() { launchctl print "$SERVICE_DOMAIN/$SERVICE_LABEL" 2>/dev/null | awk -F' = ' -v key="$1" '$1 == "\t" key { print $2; exit }'; }
# The service is this HOME's own: its plist is here, and, once loaded, it
# runs this HOME's deployed node. Its label and domain are the same for
# every HOME, so another HOME (a scratch test, a preview) never starts,
# stops or unloads it.
service_ours() {
  service_installed || return 1
  ! service_loaded || [[ "$(service_field program)" == "$DEPLOY/current/bakery-node" ]]
}

# The node on $PORT must be the service's own process. One started any
# other way (by hand, or by the app before it went through the service)
# holds the port the service needs, and launchd starts the service again
# and again, failing each time: it goes, and the service starts.
service_owns_port() {
  local listener job
  if [[ -n "${BAKERY_DRY_RUN:-}" ]]; then
    echo "+ check that the service's own process holds port $PORT"
    return 0
  fi
  for _ in 1 2 3; do
    wait_health || return 1
    listener="$(node_pid)"
    job="$(service_field pid)"
    if [[ -n "$listener" && "$listener" == "$job" ]]; then return 0; fi
    echo "  port $PORT is held by pid $listener, not the service's own process: stopping it so the service can start"
    kill "$listener" 2>/dev/null || true
    for _ in $(seq 1 50); do
      [[ -z "$(node_pid)" ]] && break
      sleep 0.2
    done
    run launchctl kickstart "$SERVICE_DOMAIN/$SERVICE_LABEL"
  done
  echo "  the service's own node never took port $PORT" >&2
  return 1
}

# Stop the node. First it finishes its running turns (SIGUSR1), for as long
# as it allows; with "now", it stops at once (TERM: a clean exit, so the
# service doesn't bring it back) and the turns carry on when it's back.
# Whatever ignores both is killed; a service is unloaded first, so it stays
# down.
stop_node() {
  local pid how="${1:-wait}"
  pid="$(node_pid)"
  [[ -z "$pid" ]] && return 0
  if [[ -n "${BAKERY_DRY_RUN:-}" ]]; then
    if [[ "$how" == wait ]]; then echo "+ kill -USR1 $pid, then wait up to ${RESTART_WAIT}s"; fi
    echo "+ kill $pid"
    return 0
  fi
  if [[ "$how" == wait ]]; then
    echo "Stopping bakery-node (pid $pid) once its running turns finish — Settings → Restart now goes ahead at once…"
    kill -USR1 "$pid" 2>/dev/null || true
    for _ in $(seq 1 $((RESTART_WAIT * 5))); do
      [[ -z "$(node_pid)" ]] && return 0
      sleep 0.2
    done
  else
    echo "Stopping bakery-node (pid $pid)…"
  fi
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 50); do
    [[ -z "$(node_pid)" ]] && return 0
    sleep 0.2
  done
  echo "  still listening after 10s — forcing"
  if service_ours && service_loaded; then launchctl bootout "$SERVICE_DOMAIN/$SERVICE_LABEL" 2>/dev/null || true; fi
  kill -9 "$pid" 2>/dev/null || true
  for _ in $(seq 1 25); do
    [[ -z "$(node_pid)" ]] && return 0
    sleep 0.2
  done
  echo "  port $PORT is still busy — not starting a second node" >&2
  return 1
}

stop_client() {
  local pids
  pids="$(client_pids)"
  [[ -z "$pids" ]] && return 0
  if [[ -n "${BAKERY_DRY_RUN:-}" ]]; then
    echo "+ kill" $pids
    return 0
  fi
  while read -r pid; do
    [[ -z "$pid" ]] && continue
    echo "Stopping client process (pid $pid)…"
    kill "$pid" 2>/dev/null || true
  done <<<"$pids"
  sleep 1
  while read -r pid; do
    [[ -z "$pid" ]] && continue
    kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true
  done <<<"$pids"
}

# ---- start -------------------------------------------------------------

wait_health() {
  if [[ -n "${BAKERY_DRY_RUN:-}" ]]; then
    echo "+ wait for http://$ADDR/health"
    return 0
  fi
  for _ in $(seq 1 120); do
    if curl -fsS --max-time 1 "http://$ADDR/health" >/dev/null 2>&1; then
      echo "  healthy on $ADDR"
      return 0
    fi
    sleep 0.5
  done
  echo "  no /health answer after 60s — see $NODE_LOG" >&2
  return 1
}

# In the background, from the repo root (exactly like `cargo run`), output to
# the log named first.
launch() {
  local log="$1"
  shift
  if [[ -n "${BAKERY_DRY_RUN:-}" ]]; then
    echo "+ (cd $ROOT && $*) >$log 2>&1 &"
    return 0
  fi
  mkdir -p "$LOG_DIR"
  ( cd "$ROOT" && exec "$@" ) >"$log" 2>&1 &
  echo "  pid $!"
}

# The deployed node goes through the service when it's installed, so macOS
# keeps it running; anything else starts here. Either way it logs to
# $NODE_LOG ($LOG_DIR/node.log keeps only what it says before that opens).
start_node_bg() {
  if service_ours && [[ "$1" == "$DEPLOY/current/bakery-node" ]]; then
    echo "Starting bakery-node through its service ($SERVICE_LABEL) — log: $NODE_LOG"
    if ! service_loaded; then run launchctl bootstrap "$SERVICE_DOMAIN" "$SERVICE_PLIST"; fi
    run launchctl kickstart "$SERVICE_DOMAIN/$SERVICE_LABEL"
    service_owns_port
    return
  fi
  if [[ -f "$LOG_DIR/node.log" ]]; then run mv -f "$LOG_DIR/node.log" "$LOG_DIR/node.prev.log"; fi
  echo "Starting bakery-node ($1) — log: $NODE_LOG"
  launch "$LOG_DIR/node.log" "$1"
}

# The LaunchAgent for the deployed node: started at login (RunAtLoad), and
# again after a crash but never after a clean stop (KeepAlive on a failed
# exit), in the logged-in session only. The PATH it runs with is this
# shell's, as when the node started from here.
xml() { sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' <<<"$1"; }
service_plist() {
  cat <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$SERVICE_LABEL</string>
  <key>ProgramArguments</key><array><string>$(xml "$DEPLOY/current/bakery-node")</string></array>
  <key>WorkingDirectory</key><string>$(xml "$ROOT")</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>$(xml "$PATH")</string>
    <key>LANG</key><string>$(xml "${LANG:-en_US.UTF-8}")</string>
    <key>BAKERY_NODE_ADDR</key><string>$(xml "$ADDR")</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>5</integer>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
  <key>StandardOutPath</key><string>$(xml "$BAKERY_HOME/logs/launchd.log")</string>
  <key>StandardErrorPath</key><string>$(xml "$BAKERY_HOME/logs/launchd.log")</string>
</dict>
</plist>
EOF
}

service_install() {
  if ! deployed; then
    echo "Nothing is deployed yet: the service runs the deployed build. ./scripts/bakery.sh deploy first." >&2
    exit 1
  fi
  local loaded=false
  if service_loaded; then loaded=true; fi
  if $loaded && [[ "$(service_field path)" != "$SERVICE_PLIST" ]]; then
    if [[ -z "${BAKERY_DRY_RUN:-}" ]]; then
      echo "The node service is already loaded from $(service_field path), another HOME's: remove it there first." >&2
      exit 1
    fi
    # A dry run touches nothing: it says where a real run stops, then shows
    # this HOME's own plan, never one that reaches the other HOME's job.
    echo "+ (a real run stops here: the service is loaded from $(service_field path), another HOME's)"
    loaded=false
  fi
  run mkdir -p "$(dirname "$SERVICE_PLIST")" "$BAKERY_HOME/logs"
  if [[ -n "${BAKERY_DRY_RUN:-}" ]]; then
    echo "+ write $SERVICE_PLIST:"
    service_plist
  else
    service_plist >"$SERVICE_PLIST"
  fi
  # One node on $PORT: the service's from now on.
  if ! $loaded; then stop_node; fi
  if $loaded; then run launchctl bootout "$SERVICE_DOMAIN/$SERVICE_LABEL" || true; fi
  run launchctl bootstrap "$SERVICE_DOMAIN" "$SERVICE_PLIST"
  wait_health
  echo "The node now starts at login and again after a crash: ./scripts/bakery.sh service remove undoes it."
}

service_remove() {
  if service_ours && service_loaded; then
    stop_node
    run launchctl bootout "$SERVICE_DOMAIN/$SERVICE_LABEL" || true
  fi
  run rm -f "$SERVICE_PLIST"
  echo "The node no longer starts at login: ./scripts/bakery.sh starts it by hand."
}

# $1: deployed (Bakery.app on the deployed UI) or dev (the desktop app against
# Vite, hot reload). A build from before the Electron app has only its Tauri
# binary, which still starts when a rollback brings one back. An inherited
# ELECTRON_RUN_AS_NODE (VS Code sets it for its children) would start Electron
# as plain node, so it is dropped.
start_client_bg() {
  if [[ "$1" == deployed && -d "$DEPLOY/current/Bakery.app" ]]; then
    echo "Starting the deployed app — log: $LOG_DIR/client.log"
    launch "$LOG_DIR/client.log" env -u ELECTRON_RUN_AS_NODE "$DEPLOY/current/Bakery.app/Contents/MacOS/Bakery"
  elif [[ "$1" == deployed ]]; then
    echo "Starting the deployed app (a build from before Electron) — log: $LOG_DIR/client.log"
    launch "$LOG_DIR/client.log" "$DEPLOY/current/bakery-client"
  else
    echo "Starting Vite and the desktop app on it — logs: $LOG_DIR/{vite,client}.log"
    launch "$LOG_DIR/vite.log" pnpm --dir bakery-client dev
    launch "$LOG_DIR/client.log" env -u ELECTRON_RUN_AS_NODE BAKERY_UI_DEV_URL=http://localhost:1420 \
      pnpm --dir bakery-client electron:dev
  fi
}

# ---- restart -----------------------------------------------------------

# Swap the running node for the binary $1 — in this terminal when $2 is 1.
start_node() {
  stop_node || return 1
  if [[ "$2" == 1 ]]; then
    echo "Starting bakery-node in this terminal (Ctrl-C stops it)…"
    cd "$ROOT"
    run exec "$1"
    return 0
  fi
  start_node_bg "$1"
  wait_health
}

# Build FIRST, swap second: a compile error (say, mid-way through the Baker's
# edits) leaves the running node untouched instead of leaving you with none.
restart_dev_node() {
  echo "Building bakery-node from source…"
  if ! (cd "$ROOT" && run cargo build -p bakery-node); then
    echo "Build failed — the running node was left untouched." >&2
    exit 1
  fi
  start_node "$DEV_NODE_BIN" "$1"
}

# Never a rebuild: what runs changes only on deploy.
restart_node() {
  if deployed; then
    start_node "$DEPLOY/current/bakery-node" "$1"
  else
    echo "Nothing is deployed yet, so this restarts from source (./scripts/bakery.sh deploy makes a build to run)."
    restart_dev_node "$1"
  fi
}

restart_client() {
  stop_client
  if deployed; then start_client_bg deployed; else start_client_bg dev; fi
}

# ---- deploy / rollback -------------------------------------------------

# One deploy, rollback or adapter upgrade at a time, from a terminal or a
# Settings button: an atomic mkdir, released on exit. A lock whose recorded
# owner is gone (killed outright) is taken over; one without an owner yet is
# left alone. Taking it again in the same run (an upgrade's deploy) is a no-op.
take_lock() {
  local owner
  if [[ -n "$LOCK_HELD" ]]; then return 0; fi
  if [[ -d "$LOCK" ]]; then
    owner="$(cat "$LOCK/pid" 2>/dev/null || true)"
    if [[ -z "$owner" ]] || kill -0 "$owner" 2>/dev/null; then
      echo "A deploy, rollback or upgrade is already running${owner:+ (pid $owner)}. Wait for it to finish; if none is running, remove $LOCK." >&2
      exit 1
    fi
    echo "Taking over the deploy lock: its owner, pid $owner, is gone."
    run rm -rf "$LOCK"
  fi
  if [[ -n "${BAKERY_DRY_RUN:-}" ]]; then
    echo "+ mkdir $LOCK"
    LOCK_HELD=dry
    return 0
  fi
  mkdir -p "$DEPLOY"
  if ! mkdir "$LOCK" 2>/dev/null; then
    echo "A deploy or rollback started just now; wait for it to finish." >&2
    exit 1
  fi
  LOCK_HELD=1
  echo $$ >"$LOCK/pid"
}

# Every step must pass before anything running is touched; the first failure
# removes the half-made build and names the step that stopped it.
deploy_step() {
  local what="$1"
  shift
  echo "== $what"
  if ! (cd "$ROOT" && run "$@"); then
    echo "Deploy stopped: $what failed. The running app was left untouched." >&2
    run rm -rf "$DEPLOY/next"
    exit 1
  fi
}

write_build_json() {
  local commit dirty
  commit="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || true)"
  dirty="$(git -C "$ROOT" status --porcelain 2>/dev/null | grep -c . || true)"
  if [[ -n "${BAKERY_DRY_RUN:-}" ]]; then
    echo "+ write $1 (commit $commit, $dirty uncommitted files, built_at now)"
    return 0
  fi
  printf '{"commit":"%s","built_at":"%s","dirty_files":%s}\n' \
    "$commit" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${dirty:-0}" >"$1"
}

# current ↔ previous. The temporary name is unique, so an interrupted swap
# can never move one build inside another.
swap_builds() {
  run mv "$DEPLOY/current" "$DEPLOY/swap.$$"
  run mv "$DEPLOY/previous" "$DEPLOY/current"
  run mv "$DEPLOY/swap.$$" "$DEPLOY/previous"
}

# Node, then app, on the current deployed build. A node that never answers is
# rolled back by itself, so a bad build can't leave you without an app.
restart_deployed() {
  if ! start_node "$DEPLOY/current/bakery-node" 0; then
    if [[ -d "$DEPLOY/previous" ]]; then
      echo "The new node did not come up — rolling back to the previous build." >&2
      swap_builds
      start_node "$DEPLOY/current/bakery-node" 0 || true
    else
      echo "The new node did not come up and there is no previous build: ./scripts/bakery.sh dev runs from source." >&2
    fi
    exit 1
  fi
  restart_client
}

deploy() {
  local next="$DEPLOY/next"
  take_lock
  echo "Deploying $ROOT at $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo '(no commit)')…"
  run rm -rf "$next"
  run mkdir -p "$next"
  # First: its time marks the source this build saw (`status`, Settings).
  write_build_json "$next/build.json"
  client_dependencies
  if [[ "$1" == 1 ]]; then
    deploy_step "node tests" cargo test -p bakery-node
    deploy_step "client checks" pnpm --dir bakery-client check:all
  fi
  deploy_step "release node build" cargo build --release -p bakery-node
  deploy_step "UI build" pnpm --dir bakery-client build
  deploy_step "Electron runtime" ensure_electron
  deploy_step "desktop app build" pnpm --dir bakery-client electron:pack
  deploy_step "installing the UI" install_ui "$next"
  deploy_step "staging the build" cp "$ROOT/target/release/bakery-node" "$ROOT/scripts/bakery.sh" "$next/"
  deploy_step "staging the desktop app" stage_electron_app "$next"
  # The switch: current becomes previous (rollback), next becomes current.
  run rm -rf "$DEPLOY/previous"
  if [[ -d "$DEPLOY/current" ]]; then run mv "$DEPLOY/current" "$DEPLOY/previous"; fi
  run mv "$next" "$DEPLOY/current"
  prune_ui
  echo "Built. Restarting on the new build…"
  restart_deployed
}

# bakery-client's dependencies as its lockfile has them, the Claude adapter
# with the Bakery's patch among them: the node runs that adapter from here, so
# a patch changed since the last install would stay the old one. pnpm checks
# the installed tree against the lockfile itself (patch hashes too) and does
# nothing when they already match.
client_dependencies() {
  deploy_step "client dependencies: pnpm --dir bakery-client install --frozen-lockfile" \
    pnpm --dir bakery-client install --frozen-lockfile
}

# Only the UI: build it, install it, point the current build at it. The open
# app checks the new bundle and reloads on it; nothing restarts.
deploy_ui() {
  take_lock
  if [[ ! -d "$DEPLOY/current/Bakery.app" ]]; then
    echo "No desktop app is deployed to swap the UI of: ./scripts/bakery.sh deploy first." >&2
    exit 1
  fi
  client_dependencies
  if [[ "$1" == 1 ]]; then deploy_step "client checks" pnpm --dir bakery-client check:all; fi
  deploy_step "UI build" pnpm --dir bakery-client build
  deploy_step "installing the UI" install_ui "$DEPLOY/current"
  prune_ui
  echo "UI deployed: the open app reloads on it."
}

# The client build into the hash-named UI store (electron/install-ui.ts); its
# hash goes into build folder $1, where the app reads it and watches for a
# new one. The hash it replaces is kept for rollback --ui; the node's UI
# install (routes/ui_bundle.rs) keeps it the same way.
install_ui() {
  local hash old
  hash="$(node "$ROOT/bakery-client/electron/install-ui.ts" "$ROOT/bakery-client/build" "$DEPLOY/ui")" || return 1
  old="$(cat "$1/ui-hash" 2>/dev/null || true)"
  if [[ -n "$old" && "$old" != "$hash" ]]; then write_hash "$1/ui-hash.previous" "$old"; fi
  write_hash "$1/ui-hash" "$hash"
}

# A UI hash file, written whole or not at all.
write_hash() {
  printf '%s\n' "$2" >"$1.tmp" && mv "$1.tmp" "$1"
}

# Electron's npm package has no install script: its binary comes from its own
# installer, or on first use, and electron-builder packages that binary. So
# fetch it when it's missing, and stop with the fix when it can't be had.
ensure_electron() {
  local electron="$ROOT/bakery-client/node_modules/electron"
  if [[ ! -d "$electron/dist" ]]; then
    echo "Fetching Electron's binary (its own installer; pnpm install doesn't)…"
    node "$electron/install.js" || true
  fi
  if [[ ! -d "$electron/dist" ]]; then
    echo "Electron's binary is still missing from $electron/dist. Run \`pnpm --dir bakery-client install\`, then \`node bakery-client/node_modules/electron/install.js\` (it downloads, so it needs the network), and deploy again." >&2
    return 1
  fi
}

stage_electron_app() {
  local app
  for app in "$ELECTRON_OUT"/mac*/Bakery.app; do
    if [[ -d "$app" ]]; then
      ditto "$app" "$1/Bakery.app"
      return
    fi
  done
  echo "electron:pack left no Bakery.app under $ELECTRON_OUT" >&2
  return 1
}

# UI bundles that neither the current nor the previous build shows, nor
# rollback --ui would.
prune_ui() {
  local current previous undo bundle
  current="$(cat "$DEPLOY/current/ui-hash" 2>/dev/null || true)"
  previous="$(cat "$DEPLOY/previous/ui-hash" 2>/dev/null || true)"
  undo="$(cat "$DEPLOY/current/ui-hash.previous" 2>/dev/null || true)"
  [[ -n "$current" ]] || return 0
  for bundle in "$DEPLOY/ui"/*; do
    [[ -e "$bundle" ]] || continue
    case "${bundle##*/}" in
      "$current" | "$previous" | "$undo") ;;
      *) run rm -rf "$bundle" ;;
    esac
  done
}

rollback() {
  take_lock
  if [[ ! -d "$DEPLOY/previous" ]]; then
    echo "Nothing to roll back to: there is no previous deployed build." >&2
    exit 1
  fi
  echo "Rolling back to $(describe_build "$DEPLOY/previous")…"
  swap_builds
  restart_deployed
}

# The current build's UI before its last UI install, back in place; the one
# it replaces becomes the way forward, so running it again undoes it. The
# open app checks the bundle and reloads on it; nothing restarts.
rollback_ui() {
  local current undo
  take_lock
  current="$(cat "$DEPLOY/current/ui-hash" 2>/dev/null || true)"
  undo="$(cat "$DEPLOY/current/ui-hash.previous" 2>/dev/null || true)"
  if [[ -z "$undo" ]]; then
    echo "Nothing to roll the UI back to: the current build has shown only this UI. ./scripts/bakery.sh rollback switches the whole build back." >&2
    exit 1
  fi
  if [[ ! -d "$DEPLOY/ui/$undo" ]]; then
    echo "The previous UI (${undo:0:12}) is no longer installed. ./scripts/bakery.sh rollback switches the whole build back." >&2
    exit 1
  fi
  run write_hash "$DEPLOY/current/ui-hash.previous" "$current"
  run write_hash "$DEPLOY/current/ui-hash" "$undo"
  echo "UI rolled back to ${undo:0:12}: the open app reloads on it. Run it again to undo."
}

# ---- adapter upgrade (V1) ----------------------------------------------

# The Claude native adapter's pin lives only in bakery-client's package.json
# and pnpm-lock.yaml (the node compiles it in), and pnpm keys the Bakery patch
# by version. So an upgrade is: pin, move the patch, install, check the
# handshake, deploy --checked. Any failure puts all of it back and reinstalls
# what was there, so Claude native is never left on an adapter its node
# refuses. Exit 3: the patch does not apply to the new release and needs
# porting first, which the Baker can do.
ADAPTER="@agentclientprotocol/claude-agent-acp"

# Exactly one literal replacement, or fail: the pin is edited where it is
# written, never by pattern.
replace_once() {
  if [[ -n "${BAKERY_DRY_RUN:-}" ]]; then
    echo "+ in ${1#"$ROOT/"}: $2 → $3"
    return 0
  fi
  node -e '
    const fs = require("fs");
    const [file, from, to] = process.argv.slice(1);
    const text = fs.readFileSync(file, "utf8");
    if (text.split(from).length !== 2) {
      console.error(`${file}: expected ${from} exactly once`);
      process.exit(1);
    }
    fs.writeFileSync(file, text.replace(from, () => to));
  ' "$1" "$2" "$3"
}

# From the exit handler, so every way out of an unfinished upgrade comes here.
restore_adapter() {
  local client="$ROOT/bakery-client"
  echo "Putting the adapter pin, lockfile and Bakery patch back as they were…" >&2
  # Not -p: the node's build.rs compiles the pin from package.json and only
  # reruns when its mtime moves, so an old mtime would keep the failed pin.
  run cp "$UPGRADE_BACKUP/package.json" "$UPGRADE_BACKUP/pnpm-lock.yaml" "$UPGRADE_BACKUP/pnpm-workspace.yaml" "$client/"
  run rm -rf "$client/patches"
  run cp -Rp "$UPGRADE_BACKUP/patches" "$client/patches"
  if (cd "$client" && run pnpm install --frozen-lockfile); then
    run rm -rf "$UPGRADE_BACKUP"
    echo "Restored: the previous adapter is installed again." >&2
  else
    echo "The files are back, but reinstalling the previous adapter failed: run pnpm install in bakery-client. A copy stays in $UPGRADE_BACKUP." >&2
  fi
  UPGRADE_BACKUP=""
}

upgrade_adapter() {
  local to="$1" client="$ROOT/bakery-client" backup="$DEPLOY/adapter-backup"
  local exact='^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?$' patch from install_log
  patch="$client/patches/${ADAPTER//\//__}"
  if [[ ! "$to" =~ $exact ]]; then
    echo "usage: ./scripts/bakery.sh upgrade-adapter <exact version, e.g. 0.85.0>" >&2
    exit 2
  fi
  take_lock
  from="$(node -p 'require(process.argv[1]).dependencies[process.argv[2]]' "$client/package.json" "$ADAPTER")"
  if [[ "$from" == "$to" ]]; then
    echo "$ADAPTER is already pinned to $to."
    return 0
  fi
  if [[ ! -f "$patch@$from.patch" ]]; then
    echo "No Bakery patch for $ADAPTER@$from at ${patch#"$ROOT/"}@$from.patch." >&2
    exit 1
  fi
  # A backup left by an upgrade that never got to restore it may be the only
  # copy of the previous pin: never overwrite it.
  if [[ -e "$backup" ]]; then
    echo "An earlier adapter upgrade did not finish: the files it replaced are in $backup. Put them back (or remove it), then try again." >&2
    exit 1
  fi
  echo "Upgrading $ADAPTER $from → $to"
  run mkdir -p "$backup"
  run cp -p "$client/package.json" "$client/pnpm-lock.yaml" "$client/pnpm-workspace.yaml" "$backup/"
  run cp -Rp "$client/patches" "$backup/patches"
  UPGRADE_BACKUP="$backup"
  replace_once "$client/package.json" "\"$ADAPTER\": \"$from\"" "\"$ADAPTER\": \"$to\""
  replace_once "$client/pnpm-workspace.yaml" \
    "$ADAPTER@$from': patches/${ADAPTER//\//__}@$from.patch" "$ADAPTER@$to': patches/${ADAPTER//\//__}@$to.patch"
  # A patch already ported to the new release is the one to use; the old
  # patch only moves across when nobody has ported it yet.
  if [[ -f "$patch@$to.patch" ]]; then
    echo "== using the Bakery patch already ported to $to"
    run rm "$patch@$from.patch"
  else
    run mv "$patch@$from.patch" "$patch@$to.patch"
  fi
  echo "== installing $ADAPTER@$to with the Bakery patch"
  install_log="$(mktemp "${TMPDIR:-/tmp}/bakery-upgrade.XXXXXX")"
  if ! (cd "$client" && run pnpm install) 2>&1 | tee "$install_log"; then
    if grep -qE "ERR_PNPM_PATCH_FAILED|Could not apply patch" "$install_log"; then
      rm -f "$install_log"
      echo "The Bakery patch does not apply to $ADAPTER@$to: it needs porting first. Port ${patch#"$ROOT/"}@$from.patch to ${patch#"$ROOT/"}@$to.patch (the Baker can), then run this again." >&2
      exit 3
    fi
    rm -f "$install_log"
    echo "pnpm install failed." >&2
    exit 1
  fi
  rm -f "$install_log"
  echo "== handshake check: initialize must report $to and bakery.dev/review"
  if ! run node "$client/scripts/check-claude-native.mjs"; then
    echo "The new adapter failed its handshake check." >&2
    exit 1
  fi
  deploy 1
  UPGRADE_BACKUP=""
  run rm -rf "$backup"
  echo "Upgraded $ADAPTER to $to."
}

# ---- commands ----------------------------------------------------------

# All in one function, called on the script's last line: bash then holds the
# whole script before running any of it, so an edit to this file during a
# minutes-long deploy can't change what the running copy does next.
# ---- fresh start -------------------------------------------------------

# A new, clean Heart carrying one Baker thread (the one called "local models",
# or the one --keep names) under a permanent Baker minted fresh, with the
# Always-allow grants cleared. The running node makes the plan and refuses
# while a turn runs; the node it restarts into sets the old Heart aside,
# closed, before it opens one, and carries the thread over. Nothing else is
# deleted: the other conversations' worktrees stay on disk, listed.
fresh_start() {
  local keep="" token plan refused stream answer done failed
  case "${1:-}" in
    "") ;;
    --keep) keep="${2:?--keep needs the stream_id of the Baker thread to keep}" ;;
    *) echo "unknown fresh-start option: $1 (--keep <stream_id>)" >&2; exit 2 ;;
  esac
  if [[ -z "$(node_pid)" ]]; then
    echo "The node isn't running. Start it first (./scripts/bakery.sh): the fresh start asks it what runs." >&2
    exit 1
  fi
  token="${BAKERY_OPERATOR_TOKEN:-$(jq -r '.operator_token // empty' "$BAKERY_HOME/node.json" 2>/dev/null || true)}"
  # The credential goes to curl on stdin, never in its arguments, which any
  # process of this user can read.
  if ! plan="$(printf 'header = "Authorization: Bearer %s"\n' "$token" | curl -K - -fsS --max-time 60 "http://$ADDR/fresh-start/plan${keep:+?keep=$keep}")"; then
    echo "The running node has no fresh start yet: deploy a build that has it first." >&2
    exit 1
  fi
  jq -r '
    "A fresh start: a new, clean Heart.",
    (if .keep then "  Keeps:   the Baker thread \"\(.keep.title)\" (\(.keep.stream_id)) with its whole history, under a new permanent Baker" else "  Keeps:   no thread yet" end),
    "  Leaves:  \(.left_behind) conversations, with their history, in the old Heart, kept aside and closed",
    "  Clears:  \(.always_allow) Always-allow grants",
    (if (.worktrees | length) > 0 then "  Left on disk, for you to remove when you want:", (.worktrees[] | "    \(.)") else empty end),
    (if .keep == null and (.threads | length) > 0 then "  The Baker threads there are:", (.threads[] | "    \(.stream_id)  \(.title)") else empty end)
  ' <<<"$plan"
  refused="$(jq -r '.refused // empty' <<<"$plan")"
  if [[ -n "$refused" ]]; then
    echo "Not now: $refused" >&2
    exit 1
  fi
  stream="$(jq -r '.keep.stream_id' <<<"$plan")"
  read -r -p "Start fresh, keeping only that thread? [y/N] " answer
  case "$answer" in
    y | Y | yes) ;;
    *) echo "Cancelled: nothing changed."; return 0 ;;
  esac
  take_lock
  # The node to start again: the deployed build, or, before the first deploy,
  # one built from source first, so a failed build changes nothing.
  local bin="$DEPLOY/current/bakery-node"
  if ! deployed; then
    echo "Nothing is deployed yet, so it starts again from source: building bakery-node first…"
    if ! (cd "$ROOT" && run cargo build -p bakery-node); then
      echo "Build failed — nothing changed." >&2
      exit 1
    fi
    bin="$DEV_NODE_BIN"
  fi
  done="$BAKERY_HOME/fresh-start.done.json"
  failed="$BAKERY_HOME/fresh-start.failed.json"
  # Armed only once the running node is down: it never sees the marker, and
  # a node that won't stop leaves nothing armed.
  if ! stop_node; then
    echo "The running node didn't stop, so nothing changed." >&2
    exit 1
  fi
  if [[ -n "${BAKERY_DRY_RUN:-}" ]]; then
    echo "+ write $BAKERY_HOME/fresh-start.json: keep $stream"
  else
    rm -f "$done" "$failed"
    jq -n --arg keep "$stream" '{keep: $keep}' >"$BAKERY_HOME/fresh-start.json"
    FRESH_ARMED="$BAKERY_HOME/fresh-start.json"
  fi
  # The node it starts again sets the old Heart aside before it opens one.
  start_node_bg "$bin"
  # A fresh start that failed puts everything back and stops that node:
  # with no service to start it again, nothing answers, and why is said
  # below all the same.
  local healthy=yes
  wait_health || healthy=
  [[ -n "${BAKERY_DRY_RUN:-}" ]] && return 0
  if [[ -f "$done" ]]; then
    echo "Done: the new Heart carries $(jq -r .kept "$done"), under the new Baker $(jq -r .home "$done")."
    echo "The old Heart, closed, for you to delete when you want: $(jq -r .closed "$done")"
  elif [[ -f "$failed" ]]; then
    echo "The fresh start did not run, and the Heart is as it was: $(jq -r .why "$failed")" >&2
    if [[ -z "$(node_pid)" ]]; then echo "The node is stopped: ./scripts/bakery.sh starts it again on that Heart." >&2; fi
    exit 1
  elif [[ -n "$healthy" ]]; then
    echo "The node is back, but said nothing of the fresh start: see $NODE_LOG" >&2
    exit 1
  elif [[ -z "$(node_pid)" ]]; then
    echo "The node stopped before it said anything of the fresh start: see $NODE_LOG" >&2
    exit 1
  else
    echo "The node is running but hasn't answered, and hasn't said yet how the fresh start went: see $NODE_LOG" >&2
    exit 1
  fi
}

main() {
  local cmd="${1:-start}" mode="" target fg checked ui answer
  case "$cmd" in
    help | -h | --help)
      awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
      return 0
      ;;
    uninstall)
      # Everything Bakery makes lives in ~/.bakery — its state, the agents'
      # homes and caches, worktrees, projects created inside Bakery, and the
      # deployed build — so one folder decides it, like a Homebrew prefix.
      # Folders you attached from elsewhere are yours and never touched.
      local choice="${2:-}" links wt common kept d link main_repo
      if [[ ! -d "$BAKERY_HOME" ]]; then
        echo "Nothing to remove: $BAKERY_HOME does not exist."
        return 0
      fi
      if [[ -z "$choice" ]]; then
        echo "Bakery keeps everything it made in $BAKERY_HOME."
        echo "Your work there: projects/, worktrees/, chats/ and the conversation history. Folders you attached from elsewhere are never touched."
        read -r -p "[k]eep my work / [d]elete everything / [c]ancel? " answer
        case "$answer" in
          k | K) choice="--keep" ;;
          d | D) choice="--everything" ;;
          *) echo "Cancelled."; return 0 ;;
        esac
      fi
      case "$choice" in
        --keep | --everything) ;;
        *) echo "unknown uninstall option: $choice (--keep | --everything)" >&2; exit 2 ;;
      esac
      # Its service first, so nothing starts it again.
      if service_ours && service_loaded; then run launchctl bootout "$SERVICE_DOMAIN/$SERVICE_LABEL" || true; fi
      run rm -f "$SERVICE_PLIST"
      stop_client
      stop_node now
      # Linked worktrees are registered in their main repositories: record
      # which repo owns each one, then tell it where the worktree went (kept)
      # or that it is gone (deleted).
      links=()
      for wt in "$BAKERY_HOME"/worktrees/*/; do
        [[ -e "$wt.git" ]] || continue
        common="$(git -C "$wt" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || continue
        links+=("${wt%/}"$'\t'"$(dirname "$common")")
      done
      if [[ "$choice" == "--keep" ]]; then
        kept="$HOME/Bakery-kept-$(date +%Y%m%d-%H%M%S)"
        mkdir -p "$kept"
        for d in projects worktrees chats; do
          if [[ -d "$BAKERY_HOME/$d" ]]; then mv "$BAKERY_HOME/$d" "$kept/$d"; fi
        done
        # The conversation history: the Heart, whole (the node is stopped),
        # and every Heart a fresh start set aside (heart-<stamp>.db*).
        for f in "$BAKERY_HOME"/heart.db "$BAKERY_HOME"/heart.db-wal "$BAKERY_HOME"/heart.db-shm "$BAKERY_HOME"/heart-*.db*; do
          if [[ -e "$f" ]]; then mv "$f" "$kept/$(basename "$f")"; fi
        done
        for link in "${links[@]+"${links[@]}"}"; do
          wt="${link%%$'\t'*}"
          main_repo="${link#*$'\t'}"
          for d in projects chats; do
            [[ "$main_repo" == "$BAKERY_HOME/$d"/* ]] && main_repo="$kept/$d/${main_repo#"$BAKERY_HOME/$d"/}"
          done
          git -C "$main_repo" worktree repair "$kept/worktrees/$(basename "$wt")" >/dev/null 2>&1 || true
        done
        rm -rf "$BAKERY_HOME"
        echo "Kept your projects, worktrees, chats and conversation history in $kept; removed the rest of $BAKERY_HOME."
      else
        rm -rf "$BAKERY_HOME"
        for link in "${links[@]+"${links[@]}"}"; do
          main_repo="${link#*$'\t'}"
          [[ -d "$main_repo" ]] && git -C "$main_repo" worktree prune >/dev/null 2>&1 || true
        done
        echo "Removed $BAKERY_HOME."
      fi
      echo "To finish, delete the app itself (Bakery.app, or this checkout)."
      return 0
      ;;
    status)
      report
      return 0
      ;;
    kill)
      stop_client
      stop_node now
      echo "Done."
      return 0
      ;;
    service)
      case "${2:-}" in
        install) service_install ;;
        remove) service_remove ;;
        *) echo "unknown service option: ${2:-} (install | remove)" >&2; exit 2 ;;
      esac
      return 0
      ;;
    restart)
      shift
      target="node"
      fg=0
      for arg in "$@"; do
        case "$arg" in
          node | client | all) target="$arg" ;;
          --fg) fg=1 ;;
          *) echo "unknown restart option: $arg (node|client|all, --fg)" >&2; exit 2 ;;
        esac
      done
      case "$target" in
        node) restart_node "$fg" ;;
        client) restart_client ;;
        all)
          # --fg would take over this terminal before the client restarts.
          if [[ "$fg" == "1" ]]; then
            echo "--fg only applies to 'restart' / 'restart node'" >&2
            exit 2
          fi
          restart_node 0
          restart_client
          ;;
      esac
      return 0
      ;;
    deploy)
      shift
      checked=0
      ui=0
      for arg in "$@"; do
        case "$arg" in
          --checked) checked=1 ;;
          --ui) ui=1 ;;
          *) echo "unknown deploy option: $arg (--checked, --ui)" >&2; exit 2 ;;
        esac
      done
      if [[ "$ui" == 1 ]]; then deploy_ui "$checked"; else deploy "$checked"; fi
      return 0
      ;;
    rollback)
      case "${2:-}" in
        "") rollback ;;
        --ui) rollback_ui ;;
        *) echo "unknown rollback option: $2 (--ui)" >&2; exit 2 ;;
      esac
      return 0
      ;;
    upgrade-adapter)
      upgrade_adapter "${2:-}"
      return 0
      ;;
    fresh-start)
      fresh_start "${@:2}"
      return 0
      ;;
    dev)
      restart_dev_node 0
      # The dev app hot-reloads, so one already running is left alone; the
      # deployed app makes way for it.
      if [[ -n "$(app_pids)" || -z "$(dev_client_pids)" ]]; then
        stop_client
        start_client_bg dev
      else
        echo "The dev app already runs — it hot-reloads."
      fi
      return 0
      ;;
    start) mode="${2:-}" ;;
    --yes | --reuse) mode="$cmd" ;;
    *) echo "unknown command: $cmd — ./scripts/bakery.sh help" >&2; exit 2 ;;
  esac

  # ---- start: check, prompt (or --yes / --reuse), start what's missing ----

  if [[ -n "$(node_pid)" || -n "$(client_pids)" ]]; then
    report
    echo
    case "$mode" in
      --yes)
        stop_client
        stop_node now
        ;;
      --reuse)
        echo "Reusing what's already running (--reuse); starting only what's missing."
        ;;
      *)
        read -r -p "Bakery is already running. [r]euse / [k]ill and restart / [c]ancel? " answer
        case "$answer" in
          k | K)
            stop_client
            stop_node now
            ;;
          r | R) echo "Reusing what's already running; starting only what's missing." ;;
          *) echo "Cancelled."; return 0 ;;
        esac
        ;;
    esac
    echo
  fi

  if deployed; then
    echo "Running the deployed build: $(describe_build "$DEPLOY/current")"
  else
    echo "Nothing is deployed yet — running from source (./scripts/bakery.sh deploy makes a build to run)."
  fi

  if [[ -n "$(node_pid)" ]]; then
    echo "bakery-node already listening on $ADDR — leaving it alone."
  elif deployed; then
    start_node_bg "$DEPLOY/current/bakery-node"
  else
    if [[ -f "$LOG_DIR/node.log" ]]; then run mv -f "$LOG_DIR/node.log" "$LOG_DIR/node.prev.log"; fi
    echo "Starting bakery-node (cargo run -p bakery-node) — log: $NODE_LOG"
    launch "$LOG_DIR/node.log" cargo run -p bakery-node
  fi

  if [[ -n "$(client_pids)" ]]; then
    echo "Client already running — leaving it alone."
  elif deployed; then
    start_client_bg deployed
  else
    start_client_bg dev
  fi

  echo
  echo "Logs: $NODE_LOG and $LOG_DIR/client.log"
  echo "Status any time:   ./scripts/bakery.sh status"
  echo "Deploy your edits: ./scripts/bakery.sh deploy"
  echo "Stop everything:   ./scripts/bakery.sh kill"
}

main "$@"; exit $?
