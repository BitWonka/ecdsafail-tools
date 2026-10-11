# fasteval: a fast, exact replica of `eval_circuit`

`fasteval` scores an `ops.bin` exactly like the challenge's trusted evaluator (`src/bin/eval_circuit.rs`): same loader,
same Fiat–Shamir shots, same simulator semantics, same totals and score. It runs the 141 shot batches in parallel and
keeps ops in a compact form, so a billion-op circuit takes a few minutes and about half the memory.

We built it for the low-qubit line, whose circuits run to 1.2–1.9 billion ops. `eval_circuit` keeps every op in memory at
56 bytes each, so a 1.24B-op build needs about 69 GB and a 1.78B-op build about 100 GB.

| Circuit (graded submission) | Ops | fasteval wall time | Peak memory |
|---|---|---|---|
| 739 qubits, `ddd607c9` | 1,236,559,427 | ~3 min (6 worker threads) | 33 GB |

## How it stays exact while running in parallel

`eval_circuit` draws all its randomness from one SHAKE256 stream, seeded by the Fiat–Shamir hash of the op list:

- the first 64 × 9,024 bytes generate the shots (two scalars per shot);
- after that, every `R`/`Hmr` op reads 8 bytes per 64-shot batch, **whether or not its condition lets it execute**.

So the measurement bytes for batch `b` always sit at offset `64·9024 + 8·M·b` (M = number of `R`/`Hmr` ops), whatever
happened in earlier batches. fasteval has one thread squeeze the stream in order and hand each batch its 8·M bytes,
while worker threads simulate batches in parallel. Each shot is one bit of a 64-bit word, and each op is packed into 20
bytes instead of 56.

## Build

It's a drop-in binary for a checkout of the challenge repo, like the other tools here. It needs no changes to any
harness file and only the crates the challenge already depends on (`sha3`, `zstd`, `ruint`).

```sh
cp fasteval.rs gensynth.rs <challenge-repo>/src/bin/
cd <challenge-repo> && cargo build --release --bin fasteval
```

## Use

```sh
./target/release/fasteval path/to/ops.bin
```

| Env var | Default | Meaning |
|---|---|---|
| `FE_THREADS` | 6 | Worker threads. The single SHAKE producer is the bottleneck, so more workers mostly add memory (one 8·M-byte buffer each). |
| `FE_SALT` | unset | Any string. Mixes a salt into the seed to get a fresh, independent set of shots and measurement bytes for the same circuit. The score it prints is **not** the official one. |

Output mirrors `eval_circuit`'s lines (`avg executed Toffoli`, `total Toffoli (sum)`, the three failure counts,
`qubits`, …) and ends with the score, computed the way `score.json` is: `round(avg executed Toffoli) × qubits`.

Differences from `eval_circuit`:

- It always prints the totals, even when some shots fail (`eval_circuit` stops at the first failing batch).
- It also prints `failing shots (any)`: the number of shots with a wrong output, a phase error or a dirty ancilla.
  `eval_circuit` only counts failing *batches*.
- It doesn't write `score.json` or `results.tsv`.

### Estimating the true failure rate

The official 9,024 shots are one sample. To estimate a circuit's expected number of failing shots per 9,024, run it
under several salts and average `failing shots (any)`. The standard error is the standard deviation over √(runs).

```sh
for s in $(seq 1 20); do FE_SALT=$s ./target/release/fasteval ops.bin | grep 'failing shots'; done
```

## Validation

**Graded submissions.** The average executed Toffoli matched the official grader's log to the last digit:

| Submission | Qubits | Avg executed Toffoli (grader = fasteval) |
|---|---|---|
| `6dbb9a88` | 732 | 416,403,331.484 |
| `ddd607c9` | 739 | 261,923,458.635 |
| `ae371a08` | 739 | 220,601,274.622 |
| `51cf3ca4` | 732 | 328,605,706.666 |

`ddd607c9` was re-checked with exactly the code in this folder. The other three were checked with the first version, which
used OpenSSL's SHAKE256 behind a self-test asserting it matched the `sha3` crate's stream.

**Differential test against the real evaluator.** `gensynth` writes random circuits in the `ops.bin` format. They cover
every op kind, conditions on measured bits, nested `PushCondition`/`PopCondition` (including pops at depth 0), and
`R`/`Hmr` inside conditions. `validate.sh` runs each one through both evaluators and compares:

- the four correctness counts;
- the average executed Toffoli and Clifford, qubits and op count (from the row `eval_circuit` appends to
  `results.tsv`, which the script restores afterwards).

```sh
cp validate.sh <challenge-repo>/ && cd <challenge-repo> && bash validate.sh 400000 11 22 33
```

Random circuits fail on every shot. So this test checks gate semantics, executed-gate accounting and
measurement-stream alignment, while the graded circuits above check the passing path.

## Caveats

- Matched against the harness at `ecdsafail-challenge` `c672dda` (2026-10-11). Its `eval_circuit.rs`, `circuit.rs`,
  `sim.rs`, `weierstrass_elliptic_curve.rs` and `Cargo.toml` are unchanged since `641d38b`. If the harness changes (the Fiat–Shamir prefix, the op
  format or the shot construction), re-run `validate.sh` and re-check one graded log before trusting the numbers.
- It's a local tool, not the grader. Submissions are scored by the official runner.
- Memory is about 20 bytes per op, plus one 8·M-byte measurement buffer per worker and queued batch.

The simulator semantics it replicates come from the challenge's `src/circuit.rs` and `src/sim.rs`. Those files are
reused from `zkp_ecc` by Tanuj Khattar et al. (CC BY 4.0, DOI 10.5281/zenodo.19196956). fasteval builds against them
through the challenge crate and doesn't copy them.
