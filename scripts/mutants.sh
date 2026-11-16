#!/usr/bin/env bash
# The named mutants: each line of scripts/mutants.list breaks one protected line and names the test that must go
# red. Fails when a mutant does not apply (the code moved), runs no test (the name moved), or survives (the test
# no longer pins the line). Restores with cp, never mv: a moved file keeps its old mtime and cargo would not rebuild.
# Usage: scripts/mutants.sh            (every mutant)
#        scripts/mutants.sh <substring> (the mutants whose label, file or test contains it)
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
filter="${1:-}"; ran=0; bad=0
cargo test --locked --no-run >/dev/null 2>&1 || { echo "mutants: the crate does not build"; exit 1; }
while IFS=$'\t' read -r label file test sub; do
  [[ -z "$label" || "$label" == \#* ]] && continue
  [[ -n "$filter" && "$label$file$test" != *"$filter"* ]] && continue
  ran=$((ran + 1))
  cp "$file" "$file.orig" && perl -0pi -e "$sub" "$file"
  if cmp -s "$file" "$file.orig"; then
    echo "NOT APPLIED: $label"; bad=$((bad + 1)); cp "$file.orig" "$file"; rm "$file.orig"; continue
  fi
  out=$(cargo test --locked --lib "$test" 2>&1)
  cp "$file.orig" "$file"; rm "$file.orig"
  count=$(printf '%s\n' "$out" | grep -oE '^running [0-9]+ test' | grep -oE '[0-9]+' | head -1)
  if [[ "${count:-0}" -eq 0 ]]; then echo "NO TEST RAN: $label ($test)"; bad=$((bad + 1))
  elif printf '%s\n' "$out" | grep -qE '^test result: FAILED'; then echo "red: $label"
  else echo "SURVIVED: $label"; bad=$((bad + 1)); fi
done < scripts/mutants.list
cargo test --locked --no-run >/dev/null 2>&1 || { echo "mutants: the crate does not build after restore"; exit 1; }
echo "mutants: $ran run, $bad failed"
[[ "$bad" -eq 0 && "$ran" -gt 0 ]]
