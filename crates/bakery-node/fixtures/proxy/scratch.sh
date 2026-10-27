#!/bin/sh
# A scratch Bakery node with its own model proxy, whose Claude calls go to
# the stand-in Anthropic API (stand-in.mjs), for testing the proxy beside the
# operator's own node. Everything lives in the folder given: its home, its
# random operator token, logs. The operator's node, ~/.bakery, Keychain and
# sign-ins are never touched. Ports: node 47911, remote 47912, proxy 47913,
# egress 47914, stand-in 47915.
#
#   scratch.sh up <dir>      build, start the stand-in and the node, install
#                            and start the node's proxy, send Claude to the
#                            stand-in, and check one call gets through;
#                            <dir>/env then holds ANTHROPIC_BASE_URL and a
#                            client token for runs straight at the proxy
#   scratch.sh proxy <dir>   the proxy's pid, to stop it mid-call
#   scratch.sh down <dir>
#
# STAND_IN_CHUNKS and STAND_IN_DELAY_MS shape the stand-in's replies.
set -eu
cmd="${1:?up, proxy or down}"
dir="${2:?a scratch folder}"
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../../.." && pwd)"
mkdir -p "$dir/home"
dir="$(cd "$dir" && pwd)"
node_at=127.0.0.1:47911
proxy=47913
stand_in=47915
facts="$dir/home/.bakery"
# cargo and rustup keep their own folders under the real home.
rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
# The release model_proxy.rs pins, for when the node can't fetch it itself.
asset=CLIProxyAPI_8.0.22_darwin_aarch64.tar.gz
pinned=2ccb94b03ce4feefa2b839f7a0eaae90951d4131572c075099f56c94d15a7f50

token() {
  [ -s "$dir/operator-token" ] || (umask 077 && openssl rand -hex 32 > "$dir/operator-token")
  cat "$dir/operator-token"
}

# method path [json body]
call() {
  curl -sS --fail-with-body --max-time 600 -X "$1" -H "Authorization: Bearer $(token)" \
    -H 'content-type: application/json' ${3:+-d "$3"} "http://$node_at$2"
}

# url seconds: the first answer within that long, whatever its status
answers() {
  i=0
  while [ "$i" -lt "$2" ]; do
    curl -s -o /dev/null --max-time 5 "$1" && return 0
    sleep 1
    i=$((i + 1))
  done
  return 1
}

stop() {
  [ -s "$1" ] && kill "$(cat "$1")" 2>/dev/null || true
  rm -f "$1"
}

case "$cmd" in
up)
  cargo build -q -p bakery-node --manifest-path "$repo/Cargo.toml"
  STAND_IN_PORT=$stand_in STAND_IN_LOG="$dir/stand-in.log" nohup node "$here/stand-in.mjs" >"$dir/stand-in.out" 2>&1 &
  echo $! >"$dir/stand-in.pid"
  # Each exec, and cargo run's own, keeps one pid: the node's.
  (cd "$dir" && exec env RUSTUP_HOME="$rustup_home" CARGO_HOME="$cargo_home" HOME="$dir/home" \
    BAKERY_NODE_ADDR=$node_at BAKERY_REMOTE_PORT=47912 BAKERY_MODEL_PROXY_PORT=$proxy \
    BAKERY_MODEL_PROXY_EGRESS_PORT=47914 BAKERY_MODEL_PROXY_STAND_IN=$stand_in BAKERY_OPERATOR_TOKEN="$(token)" \
    cargo run -q -p bakery-node --manifest-path "$repo/Cargo.toml") >"$dir/node.out" 2>&1 &
  echo $! >"$dir/node.pid"
  answers "http://$node_at/health" 120 || { echo "the scratch node didn't start: $dir/node.out"; exit 1; }
  if ! call POST /model-proxy/install >/dev/null 2>&1; then
    echo "the scratch node couldn't fetch the proxy release; fetching it with curl"
    # A new node build may not reach GitHub yet (a network filter asks
    # about each new binary): the same pinned release, by curl.
    mkdir -p "$dir/dl" "$facts/bin/cliproxyapi-8.0.22"
    curl -sSL --max-time 300 -o "$dir/dl/$asset" \
      "https://github.com/router-for-me/CLIProxyAPI/releases/download/v8.0.22/$asset"
    [ "$(shasum -a 256 "$dir/dl/$asset" | cut -d' ' -f1)" = "$pinned" ] || { echo "$asset isn't the pinned release"; exit 1; }
    tar -xzf "$dir/dl/$asset" -C "$facts/bin/cliproxyapi-8.0.22" cli-proxy-api
    call POST /model-proxy/start >/dev/null
  fi
  answers "http://127.0.0.1:$proxy/healthz" 120 || { echo "the scratch proxy didn't start"; exit 1; }
  sleep 2
  call POST /model-proxy/keys '{"provider":"claude","key":"synthetic-stand-in-key","label":"stand-in"}' >/dev/null
  # Claude's one key goes to the stand-in, directly: the proxy's room may
  # reach that port (BAKERY_MODEL_PROXY_STAND_IN), and nothing else of it.
  management="$(jq -r '.model_proxy.management_key' "$facts/node.json")"
  route='[{"api-key":"synthetic-stand-in-key","base-url":"http://127.0.0.1:'$stand_in'","proxy-url":"direct"}]'
  curl -sS --fail-with-body -X PUT -H "Authorization: Bearer $management" -H 'content-type: application/json' \
    -d "$route" "http://127.0.0.1:$proxy/v0/management/claude-api-key" >/dev/null
  client="$(jq -r '.model_proxy.client_token' "$facts/node.json")"
  (umask 077 && printf 'ANTHROPIC_BASE_URL=http://127.0.0.1:%s\nANTHROPIC_AUTH_TOKEN=%s\n' "$proxy" "$client" >"$dir/env")
  said="$(curl -sS --max-time 60 -H "Authorization: Bearer $client" -H 'content-type: application/json' \
    -H 'anthropic-version: 2023-06-01' -d '{"model":"claude-sonnet-5-5","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}' \
    "http://127.0.0.1:$proxy/v1/messages")"
  case "$said" in
  *"stand-in reply"*) echo "scratch up: node $node_at, proxy 127.0.0.1:$proxy, stand-in 127.0.0.1:$stand_in; one call got through" ;;
  *) echo "the call through the proxy didn't reach the stand-in: $said"; exit 1 ;;
  esac
  ;;
proxy)
  echo "$(cat "$facts/proxy/proxy.pid")"
  ;;
down)
  stop "$dir/node.pid"
  stop "$facts/proxy/proxy.pid"
  stop "$dir/stand-in.pid"
  echo "scratch down"
  ;;
*)
  echo "up, proxy or down" >&2
  exit 2
  ;;
esac
