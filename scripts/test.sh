#!/usr/bin/env bash
# cargo test, with a time limit on every test: a test still running at its
# limit fails as "TIMEOUT <test> after <n> s", and the rest of its binary
# still runs and reports (scripts/test-watchdog.rs; the limits are in
# scripts/test-limits). Takes cargo test's own arguments:
#
#   scripts/test.sh -p bakery-node routes::git::   one module's tests
#   scripts/test.sh -p bakery-exec                 one crate's
#   scripts/test.sh --workspace                    every crate's
#
# Before the tests it builds what they'd otherwise build inside their time:
# the watchdog (running its own tests when its source changed), and the fake
# ACP agent and node binaries the node's and the sandbox's tests start,
# handed to them as copies, since other worktrees may share the build
# folder. In a room whose temp folder refuses git, tests get a temp folder
# in .bakery-dev-logs and a stand-in git identity. It ends with the slowest
# tests and their times (all of them in .bakery-dev-logs/test-times.tsv).
set -uo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
logs="$root/.bakery-dev-logs"
bin="$logs/bin"
mkdir -p "$bin"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
rustc="${RUSTC:-rustc}"

# Parallel tests open many files at once, and a macOS Terminal often starts
# at 256: raise this run's soft limit to 10240, or the hard limit if lower
# ("unlimited" counts as higher). This process only; a refusal is no error.
soft=$(ulimit -Sn 2>/dev/null) hard=$(ulimit -Hn 2>/dev/null) want=10240
[[ $hard =~ ^[0-9]+$ ]] && ((hard < want)) && want=$hard
[[ $soft =~ ^[0-9]+$ ]] && ((soft < want)) && { ulimit -Sn "$want" 2>/dev/null || true; }

# The watchdog, built when its source is newer, and only once its own
# tests pass.
watchdog="$bin/test-watchdog"
source="$root/scripts/test-watchdog.rs"
if [[ ! -x "$watchdog" || "$source" -nt "$watchdog" ]]; then
  echo "── the test watchdog: building it and running its own tests"
  if ! { "$rustc" --edition 2021 --test -o "$bin/test-watchdog-tests" "$source" &&
    "$bin/test-watchdog-tests" -q &&
    "$rustc" --edition 2021 -O -o "$watchdog.new" "$source" &&
    mv "$watchdog.new" "$watchdog"; }; then
    echo "test.sh: the test watchdog didn't build, or its own tests failed" >&2
    exit 1
  fi
fi

# Which packages: the ones named, else every one.
packages=" "
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  case "${args[i]}" in
    --) break ;;
    -p | --package) packages+="${args[i + 1]:-} " ;;
    -p*) packages+="${args[i]#-p} " ;;
    --package=*) packages+="${args[i]#--package=} " ;;
  esac
done
[[ "$packages" == " " ]] && packages=" bakery-node bakery-exec "

# The fake ACP agent and the node, for the node's and the sandbox's tests.
if [[ "$packages" == *" bakery-node "* || "$packages" == *" bakery-exec "* ]]; then
  echo "── the binaries tests start: building them"
  built=$(cargo build -p bakery-node --bin bakery-fake-acp-agent --bin bakery-node \
    --message-format=json-render-diagnostics) || exit 1
  for name in bakery-fake-acp-agent bakery-node; do
    from=$(printf '%s\n' "$built" | sed -n "s|.*\"executable\":\"\\([^\"]*/$name\\)\".*|\\1|p" | tail -1)
    if [[ -z "$from" ]]; then
      echo "test.sh: cargo didn't say where it built $name" >&2
      exit 1
    fi
    { cp -c "$from" "$bin/$name.new" 2>/dev/null || cp "$from" "$bin/$name.new"; } &&
      mv "$bin/$name.new" "$bin/$name" || exit 1
  done
  export BAKERY_FAKE_ACP_AGENT_BIN_FOR_TEST="$bin/bakery-fake-acp-agent"
  export BAKERY_NODE_BIN_FOR_TEST="$bin/bakery-node"
fi

# A room's own temp folder refuses a repository's settings today, and a
# room has no git identity: then tests get a temp folder here, where git
# works, and a stand-in identity. Git stops looking for a repository at that
# folder, so a test's folder never finds this worktree's.
probe=$(mktemp -d "${TMPDIR:-/tmp}/test-sh.XXXXXX" 2>/dev/null) && git init -q "$probe" >/dev/null 2>&1
git_works=$?
[[ -n "${probe:-}" ]] && rm -rf "$probe" 2>/dev/null
if [[ $git_works -ne 0 ]]; then
  export TMPDIR="$logs/tmp/run-$$"
  mkdir -p "$TMPDIR"
  trap 'rm -rf "$TMPDIR"' EXIT
  export GIT_CEILING_DIRECTORIES="$logs/tmp"
  echo "── this room's temp folder refuses git: tests use $TMPDIR"
fi
if [[ -z "$(cd / && git config --get user.email 2>/dev/null)" ]]; then
  printf '[user]\n\tname = Bakery Test\n\temail = test@bakery.invalid\n' >"$logs/gitconfig"
  export GIT_CONFIG_GLOBAL="$logs/gitconfig"
  echo "── no git identity here: tests use a stand-in one"
fi

times="$logs/test-times.tsv"
: >"$times"
host=$("$rustc" -vV | sed -n 's/^host: //p')
for path in "$watchdog" "$root/scripts/test-limits" "$times"; do
  if [[ "$path" == *"'"* ]]; then
    echo "test.sh: a path with a quote in it can't be handed to cargo: $path" >&2
    exit 1
  fi
done
runner="['$watchdog', '--limits', '$root/scripts/test-limits', '--times', '$times']"
cargo test --config "target.$host.runner=$runner" "$@"
code=$?

if [[ -n "$(awk -F'\t' '$1 >= 1' "$times")" ]]; then
  echo
  echo "Slowest tests:"
  sort -t$'\t' -k1,1 -rn "$times" | awk -F'\t' '$1 >= 1' | head -10 |
    awk -F'\t' '{ printf "  %7.1f s  %s %s%s\n", $1, $2, $3, ($4 != "" ? "  (" $4 ")" : "") }'
fi
exit $code
