#!/usr/bin/env bash
# The tests a change touches, run as scripts/test.sh runs them (each test
# with its time limit). For a changed Rust file: its own module's tests
# (they live in the file; for a crate's lib.rs, the whole crate's), and a
# type-check of the crates built on its crate. For a changed client file:
# the checks that load it, and the type-checks over it. The files come from
# git: what isn't committed, and with --base <rev> what the branch has
# committed since it left <rev>. Where git can't run (a room today: the
# worktree's repository is out of its reach), name them as git_status or
# git_diff lists them:
#
#   scripts/test-changed.sh
#   scripts/test-changed.sh --base workspace/qa-slice-0ccea0
#   scripts/test-changed.sh crates/bakery-node/src/git.rs bakery-client/src/lib/transcript.ts
#
# --plan prints what it would run and runs nothing. The whole suite runs in
# the final gate: scripts/gate.sh.
set -uo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
mkdir -p "$root/.bakery-dev-logs"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"

# Parallel tests open many files at once, and a macOS Terminal often starts
# at 256: raise this run's soft limit to 10240, or the hard limit if lower
# ("unlimited" counts as higher). This process only; a refusal is no error.
soft=$(ulimit -Sn 2>/dev/null) hard=$(ulimit -Hn 2>/dev/null) want=10240
[[ $hard =~ ^[0-9]+$ ]] && ((hard < want)) && want=$hard
[[ $soft =~ ^[0-9]+$ ]] && ((soft < want)) && { ulimit -Sn "$want" 2>/dev/null || true; }

base="" plan=0 files=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --base) base="${2:?--base needs a revision}" && shift 2 ;;
    --plan) plan=1 && shift ;;
    -h | --help) sed -n '2,17p' "$0" && exit 0 ;;
    *) files+=("${1#"$root/"}") && shift ;;
  esac
done

if [[ ${#files[@]} -eq 0 ]]; then
  if ! listed=$({ git diff --name-only HEAD && git ls-files --others --exclude-standard &&
    if [[ -n "$base" ]]; then git diff --name-only "$base...HEAD"; fi; } 2>/dev/null); then
    echo "test-changed.sh: git can't run here; name the changed files (git_status or git_diff lists them)" >&2
    exit 2
  fi
  while IFS= read -r f; do [[ -n "$f" ]] && files+=("$f"); done < <(printf '%s\n' "$listed" | sort -u)
fi
if [[ ${#files[@]} -eq 0 ]]; then
  echo "Nothing has changed."
  exit 0
fi

# Per package, as bash 3.2 has no maps: F_<package> its test filters ("*"
# for all of its tests), T_<package> its integration tests.
tested=" " checked=" " client=() notes=() selfchecks=()
var() { printf '%s_%s' "$1" "$(printf '%s' "$2" | tr -c 'a-zA-Z0-9' '_')"; }
has() { [[ " ${!1-} " == *" $2 "* ]]; }
add_test() { # <package> <filter or "*">
  local name
  name=$(var F "$1")
  has "$name" "$2" || printf -v "$name" '%s %s' "${!name-}" "$2"
  [[ "$tested" == *" $1 "* ]] || tested+="$1 "
}
add_target() { # <package> <integration test>
  local name
  name=$(var T "$1")
  has "$name" "$2" || printf -v "$name" '%s %s' "${!name-}" "$2"
  [[ "$tested" == *" $1 "* ]] || tested+="$1 "
}
package_of() { sed -n 's/^name = "\(.*\)"$/\1/p' "crates/$1/Cargo.toml" | head -1; }
module_of() { # a path under src/ → its module's test filter
  local m="${1%.rs}"
  m="${m%/mod}"
  printf '%s::' "${m//\//::}"
}
# A file's module, as the test filter of its crate.
add_module_of() { # <crate folder> <path from the repository's root>
  local package rel
  package=$(package_of "$1")
  rel="${2#crates/$1/src/}"
  case "$rel" in
    lib.rs) add_test "$package" "*" ;;
    *) add_test "$package" "$(module_of "$rel")" ;;
  esac
}

for f in "${files[@]}"; do
  case "$f" in
    crates/*/*)
      dir="${f#crates/}"
      dir="${dir%%/*}"
      [[ -f "crates/$dir/Cargo.toml" ]] || continue
      [[ "$f" == crates/*/src/private/* ]] && continue
      package=$(package_of "$dir")
      [[ "$checked" == *" $package "* ]] || checked+="$package "
      rel="${f#crates/$dir/}"
      case "$rel" in
        src/main.rs)
          # The node's main.rs is its routes: their catalog and client.
          if [[ "$package" == bakery-node ]]; then
            add_test "$package" "api_catalog::"
            add_test "$package" "api_client::"
          else
            add_test "$package" "*"
          fi
          ;;
        src/bin/fake_acp_agent.rs)
          # Run by the tests that start it, in the node and the sandbox.
          while IFS= read -r user; do
            udir="${user#crates/}"
            add_module_of "${udir%%/*}" "$user"
          done < <(rg -l 'fake_acp_agent_binary\(\)' crates --glob '*.rs')
          ;;
        src/bin/*) notes+=("$f: a binary of its own; the type-check covers it, and the tests that start it, the full suite") ;;
        src/*.rs) add_module_of "$dir" "$f" ;;
        tests/*.rs) add_target "$package" "$(basename "$rel" .rs)" ;;
        Cargo.toml | build.rs) add_test "$package" "*" ;;
        *)
          # A fixture or other file: the modules that name it.
          found=0
          while IFS= read -r user; do
            add_module_of "$dir" "$user"
            found=1
          done < <(rg -l --fixed-strings "$(basename "$rel")" "crates/$dir/src" --glob '*.rs')
          [[ $found -eq 1 ]] || notes+=("$f: no test in $package names it")
          ;;
      esac
      # The routes' client in bakery-client is made from the node's types.
      [[ "$package" == bakery-node ]] && add_test "$package" "api_client::"
      ;;
    Cargo.toml | Cargo.lock)
      for d in crates/*/; do
        d="${d#crates/}"
        checked+="$(package_of "${d%/}") "
      done
      notes+=("$f: the workspace's dependencies; the whole suite runs in the final gate (scripts/gate.sh)")
      ;;
    bakery-client/*) client+=("$f") ;;
    scripts/test-watchdog.rs) selfchecks+=("watchdog") ;;
    *.sh) selfchecks+=("sh:$f") ;;
    *.mjs) selfchecks+=("mjs:$f") ;;
    *) notes+=("$f: nothing to test") ;;
  esac
done

# The crates built on a changed crate, type-checked with their tests.
dependents=" "
grow=1
while [[ $grow -eq 1 ]]; do
  grow=0
  for package in $checked $dependents; do
    while IFS= read -r manifest; do
      dependent=$(package_of "$(basename "$(dirname "$manifest")")")
      if [[ "$checked$dependents" != *" $dependent "* ]]; then
        dependents+="$dependent "
        grow=1
      fi
    done < <(rg -l --fixed-strings "path = \"../$package\"" crates/*/Cargo.toml)
  done
done
typecheck=()
for package in $checked $dependents; do
  [[ "$tested" == *" $package "* ]] || typecheck+=(-p "$package")
done

steps=() failed=()
step() { # <label> <command...>
  local label="$1"
  shift
  steps+=("$label")
  printf '── %s: %s\n' "$label" "$*"
  [[ $plan -eq 1 ]] && return 0
  "$@" || failed+=("$label")
}

if [[ ${#typecheck[@]} -gt 0 ]]; then
  step "type-check" cargo check --tests "${typecheck[@]}"
fi
for package in $tested; do
  name=$(var F "$package")
  filters="${!name-}"
  if [[ -n "$filters" ]]; then
    if [[ " $filters " == *" * "* ]]; then
      step "$package" scripts/test.sh -p "$package"
    else
      # shellcheck disable=SC2086 # the filters are words
      step "$package" scripts/test.sh -p "$package" -- $filters
    fi
  fi
  name=$(var T "$package")
  for target in ${!name-}; do
    step "$package $target" scripts/test.sh -p "$package" --test "$target"
  done
done
if [[ ${#client[@]} -gt 0 ]]; then
  if ! checks=$(node scripts/client-checks.mjs "${client[@]}" 2>"$root/.bakery-dev-logs/client-checks.err"); then
    cat "$root/.bakery-dev-logs/client-checks.err" >&2
    failed+=("which client checks")
  fi
  for check in $checks; do
    step "client $check" pnpm -C bakery-client run "$check"
  done
fi
for check in ${selfchecks[@]+"${selfchecks[@]}"}; do
  case "$check" in
    watchdog)
      bin="$root/.bakery-dev-logs/bin"
      mkdir -p "$bin"
      step "the test watchdog's own tests" sh -c \
        "rustc --edition 2021 --test -o '$bin/test-watchdog-tests' scripts/test-watchdog.rs && '$bin/test-watchdog-tests'"
      ;;
    sh:*) step "${check#sh:} reads as bash" bash -n "${check#sh:}" ;;
    mjs:*) step "${check#mjs:} reads as JavaScript" node --check "${check#mjs:}" ;;
  esac
done

echo
for note in ${notes[@]+"${notes[@]}"}; do echo "note: $note"; done
[[ $plan -eq 1 ]] && exit 0
if [[ ${#steps[@]} -eq 0 ]]; then
  echo "No tests for these files."
elif [[ ${#failed[@]} -eq 0 ]]; then
  echo "PASS: ${#steps[@]} steps. The whole suite runs in the final gate: scripts/gate.sh"
else
  echo "FAIL: ${#failed[@]} of ${#steps[@]} steps: ${failed[*]}"
fi
[[ ${#failed[@]} -eq 0 ]]
