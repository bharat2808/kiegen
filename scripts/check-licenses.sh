#!/usr/bin/env bash
# kiegen ships under Apache-2.0. This script mechanically checks dependency licences:
# it reads the licence of every crate in the resolved dependency graph and fails if
# anything copyleft reaches the *runtime* graph.
#
# Two distinct questions, deliberately answered differently:
#   * runtime graph  (normal edges)  → strict. Anything GPL/AGPL/SSPL fails the build.
#   * build/dev graph (build edges)  → informational. Build scripts are compiled into a
#     throwaway tool binary and are NOT distributed inside kiegen.app, so cssparser and
#     selectors arriving via tauri-build are noted, not blockers.
#
# Weak/file-level copyleft (MPL-2.0, LGPL, EPL, CDDL) warns: linking is fine, and these
# crates are unmodified, but a human should see them listed rather than discover them.
#
# Use scripts/check-licenses.sh (offline, no install) or cargo-deny (deny.toml, CI).
set -euo pipefail

cd "$(dirname "$0")/../src-tauri"

REGISTRY="${CARGO_HOME:-$HOME/.cargo}/registry/src"
# Hard fail: copyleft that would relicense kiegen or block permissive distribution.
DENY_ATOMS='GPL-1.0|GPL-2.0|GPL-3.0|AGPL-1.0|AGPL-3.0|SSPL-1.0|CPL-1.0|OSL-3.0|EUPL-1.2|CC-BY-SA-4.0|CECILL-2.1'
# Warn only: weak or file-level copyleft, compatible with a permissive project.
WARN_ATOMS='MPL-2.0|LGPL-2.1|LGPL-3.0|EPL-2.0|CDDL-1.0'
# Every atom of an SPDX expression must appear here to count as clean.
ALLOW_ATOMS='MIT|MIT-0|Apache-2.0|BSD-2-Clause|BSD-3-Clause|ISC|Unicode-3.0|Unicode-DFS-2016|Unlicense|Zlib|CC0-1.0|0BSD|BSL-1.0|LLVM-exception|OpenSSL|NCSA|CC-BY-4.0|CDLA-Permissive-2.0'

runtime_fail=0
runtime_warn=0
build_notes=0

# Cargo.toml license fields use `X OR Y` / `X AND Y`; split into atoms and judge each.
judge() { # $1 = crate, $2 = licence expression, $3 = mode(strict|note)
  local crate="$1" expr="$2" mode="$3"
  local atoms atom verdict="allow"
  atoms=$(echo "$expr" | sed -E 's/[()]//g; s/ OR /\n/g; s/ AND /\n/g; s/ WITH /\n/g; s|/|\n|g' | sed 's/^ *//; s/ *$//' | grep -v '^$' || true)
  [ -z "$atoms" ] && atoms="$expr"

  while read -r atom; do
    [ -z "$atom" ] && continue
    if echo "$atom" | grep -qE "^($DENY_ATOMS)$"; then
      verdict="deny"; break
    elif echo "$atom" | grep -qE "^($WARN_ATOMS)$"; then
      [ "$verdict" = "allow" ] && verdict="warn"
    elif ! echo "$atom" | grep -qE "^($ALLOW_ATOMS)$"; then
      [ "$verdict" = "allow" ] && verdict="unknown"
    fi
  done <<< "$atoms"

  case "$verdict" in
    deny)
      if [ "$mode" = "strict" ]; then
        echo "  ✗  $crate — $expr (copyleft in the RUNTIME graph)"
        runtime_fail=$((runtime_fail + 1))
      else
        echo "  !  $crate — $expr (copyleft, but build-only: not distributed)"
        build_notes=$((build_notes + 1))
      fi
      ;;
    warn)
      echo "  ⚠  $crate — $expr (weak/file-level copyleft — linking is fine, review it)"
      [ "$mode" = "strict" ] && runtime_warn=$((runtime_warn + 1))
      ;;
    unknown)
      echo "  ?  $crate — $expr (not in the allow list; review it)"
      [ "$mode" = "strict" ] && runtime_warn=$((runtime_warn + 1))
      ;;
  esac
}

scan() { # $1 = label, $2 = extra cargo-tree args, $3 = mode
  echo "$1"
  local count=0 crate manifest licence
  while read -r crate; do
    [ -z "$crate" ] && continue
    count=$((count + 1))
    manifest=$(ls -d "$REGISTRY"/*/"$crate"-*/Cargo.toml 2>/dev/null | head -1 || true)
    [ -z "$manifest" ] && continue                      # workspace/path crate
    licence=$(grep -m1 -E '^license(-file)? *=' "$manifest" | sed -E 's/^license(-file)? *= *//; s/"//g' || true)
    [ -z "$licence" ] && continue
    judge "$crate" "$licence" "$3"
  done < <(cargo tree --prefix none $2 2>/dev/null | sed 's/ v.*//; s/ (\*)$//' | sort -u)
  echo "  ($count crates)"
}

echo "resolving the dependency graph…"
scan "runtime graph (shipped inside the .app):" "--edges normal" "strict"
echo
scan "build/dev graph (not shipped):" "--edges build,dev" "note"

echo
if [ "$runtime_fail" -gt 0 ]; then
  echo "FAIL: $runtime_fail copyleft crate(s) in the runtime graph."
  exit 1
fi
if [ "$runtime_warn" -gt 0 ]; then
  echo "WARN: no copyleft blockers, but $runtime_warn crate(s) in the runtime graph need a look."
else
  echo "OK: runtime graph is clean — no copyleft, nothing unreviewed."
fi
[ "$build_notes" -gt 0 ] && echo "NOTE: $build_notes build-only copyleft crate(s); not distributed, see above."
exit 0
