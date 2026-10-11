# ecdsafail-tools

A small collection of standalone tools for working on the
[ecdsa.fail](https://ecdsa.fail) reversible-circuit challenge (minimize
`peak_qubits × avg-executed-Toffoli` for a secp256k1 point-add circuit).

These are **drop-in source files** for a checkout of the challenge repo (they
build against its `quantum_ecc` crate as extra `src/bin/` binaries) plus the docs
and runners to use them. Nothing here ships in a submission — they're analysis /
search tooling.

## Tools

### [`dead-ccx-finder/`](dead-ccx-finder/) — find charged-but-inert CCX/CCZ
Finds Toffoli gates the grader counts (`toffoli_gates += executed_shots`) but that
**never actually fire** (`cond & c1 & c2 == 0` on every input). Removing them is
value-neutral and lowers avg-executed-Toffoli for free. A self-contained,
transposed, bit-sliced screener — builds against an unmodified simulator, runs a
9M-input screen in ~13 min on a 16-core box. See its
[README](dead-ccx-finder/README.md).

### [`fasteval/`](fasteval/) — a fast, exact replica of `eval_circuit`
Scores an `ops.bin` exactly like the trusted evaluator (same Fiat–Shamir shots,
totals and score) but runs the shot batches in parallel on a compact op list: a
1.24B-op circuit in ~3 min and 33 GB, where the official evaluator needs ~69 GB.
Matched to the grader's logs to the last digit. It comes with a random-circuit
differential test (`gensynth` + `validate.sh`), and `FE_SALT` re-runs a circuit on
fresh shots to estimate its true failure rate. See its
[README](fasteval/README.md).

## Using a tool

Each tool is a `.rs` you copy into `<challenge-repo>/src/bin/` and build with
`cargo build --release --bin <tool>`, plus a runner script and a README. See each
tool's README for env knobs and the exact workflow.
