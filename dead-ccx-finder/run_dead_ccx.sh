#!/bin/bash
# Generic dead-CCX finder runner. Route-agnostic: it calls point_add::build(), which
# reads the circuit route from whatever is baked into build() OR exported in the env
# before you run this. Nothing here is hardcoded to a machine or a specific route.
#
# Usage:
#   REPO=/path/to/repo ./run_dead_ccx.sh [n_seeds] [out.idx]
#
# Env (all optional):
#   REPO          repo checkout to build/run in            (default: cwd)
#   DEAD_REAL_RNG 1=exact (default) | 0=fast/conservative
#   DEAD_THREADS  worker threads                           (default: all cores)
#   PREDICT_SHOTS shots per seed                           (default: 9024)
#   SEED_BASE / SEED_STRIDE   nonce spread start/stride
#   plus any TLM_* route knobs your circuit needs (if not baked into build()).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="${REPO:-$PWD}"
N_SEEDS="${1:-1000}"           # 1000 x 9024 ~= 9M inputs (field convergence point)
OUT="${2:-/tmp/dead_ccx.idx}"
SEED_BASE="${SEED_BASE:-700000000001}"
SEED_STRIDE="${SEED_STRIDE:-999999937}"

cp -f "$HERE/find_dead_ccx.rs" "$REPO/src/bin/find_dead_ccx.rs"
cd "$REPO"
echo ">> building find_dead_ccx ..."
cargo build --release --bin find_dead_ccx >/tmp/find_dead_build.log 2>&1 \
  || { echo "BUILD FAILED:"; tail -30 /tmp/find_dead_build.log; exit 1; }

NONCES=$(python3 -c "print(' '.join(str($SEED_BASE + i*$SEED_STRIDE) for i in range($N_SEEDS)))")
echo ">> screening $N_SEEDS seeds (~$((N_SEEDS * 9024 / 1000000))M inputs), real_rng=${DEAD_REAL_RNG:-1} ..."
DEAD_SCREEN_NONCES="$NONCES" DEAD_IDX_OUT="$OUT" \
  ./target/release/find_dead_ccx
echo ">> wrote $OUT"
