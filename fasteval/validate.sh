#!/usr/bin/env bash
# Differential test: the official eval_circuit vs fasteval on random synthetic circuits.
#
# Run from the root of a challenge checkout that has fasteval.rs and gensynth.rs in src/bin/:
#   bash validate.sh [n_ops] [seed ...]        (defaults: 400000 ops, seeds 11 22 33)
#
# Random circuits fail correctness on every shot, so eval_circuit prints its four failure counts and exits; its
# averages only appear in the FAIL row it appends to results.tsv. The script compares the counts and that row with
# fasteval's output, then restores results.tsv.
set -euo pipefail

REPO=$(pwd)
N=${1:-400000}
if [ $# -gt 0 ]; then shift; fi
if [ $# -eq 0 ]; then set -- 11 22 33; fi

[ -f "$REPO/src/bin/fasteval.rs" ] && [ -f "$REPO/src/bin/gensynth.rs" ] || {
  echo "run from a challenge checkout with fasteval.rs and gensynth.rs in src/bin/" >&2; exit 2; }

cargo build --release --bin eval_circuit --bin fasteval --bin gensynth >/dev/null 2>&1 || {
  echo "cargo build failed" >&2; exit 2; }
BIN=$REPO/target/release

TMP=$(mktemp -d)
TSV=$REPO/results.tsv
if [ -f "$TSV" ]; then cp "$TSV" "$TMP/results.tsv.bak"; fi
restore() {
  if [ -f "$TMP/results.tsv.bak" ]; then cp "$TMP/results.tsv.bak" "$TSV"; else rm -f "$TSV"; fi
  rm -rf "$TMP"
}
trap restore EXIT

counts='tested shots|classical mismatches|phase-garbage batches|ancilla-garbage batches'
field() { grep -E "$1" "$2" | head -1 | awk '{print $NF}'; }
fail=0
for seed in "$@"; do
  d=$TMP/s$seed
  mkdir -p "$d"
  "$BIN/gensynth" "$d/ops.bin" "$N" "$seed" 2>/dev/null
  (cd "$d" && "$BIN/eval_circuit" > eval.log 2>&1 || true)
  (cd "$d" && "$BIN/fasteval" ops.bin > fe.log 2>/dev/null || true)

  a=$(grep -E "$counts" "$d/eval.log" | sed 's/ *$//')
  b=$(grep -E "$counts" "$d/fe.log" | sed 's/ *$//')
  # eval_circuit's row: ts, commit, avg Toffoli, avg Clifford, qubits, ops, OK/FAIL, note
  row=$(tail -1 "$TSV" | cut -f3-6)
  ours=$(printf '%s\t%s\t%s\t%s' "$(field 'avg executed Toffoli' "$d/fe.log")" \
    "$(field 'avg executed Clifford' "$d/fe.log")" "$(field '^  qubits +:' "$d/fe.log")" \
    "$(field 'emitted ops' "$d/fe.log")")
  if [ "$a" = "$b" ] && [ -n "$a" ] && [ "$row" = "$ours" ]; then
    echo "seed $seed: MATCH  (avg Toffoli, avg Clifford, qubits, ops = $(echo "$ours" | tr '\t' ' '))"
  else
    fail=1
    echo "seed $seed: MISMATCH"
    diff <(echo "$a") <(echo "$b") || true
    echo "  eval_circuit row: $row"
    echo "  fasteval        : $ours"
  fi
done
[ $fail -eq 0 ] && echo "all seeds match" || { echo "MISMATCH found" >&2; exit 1; }
