//! gensynth: a random ops.bin in the harness format, for differential testing of fasteval against eval_circuit.
//!
//! It covers every op kind, conditions on measured bits, nested Push/PopCondition (including pops at depth 0) and
//! R/Hmr inside conditions. The circuits are not correct point additions, so every shot fails; the test compares
//! failure counts and the executed-gate averages.
//!
//! Drop-in: copy into <challenge-repo>/src/bin/ and `cargo build --release --bin gensynth`.
//! Usage:   gensynth <out.bin> <n_ops> <seed>

use std::io::Write;

const NOQ: u64 = u64::MAX;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let out = &a[1];
    let n: usize = a[2].parse().unwrap();
    let mut s: u64 = a[3].parse::<u64>().unwrap().wrapping_mul(2).wrapping_add(1); // xorshift needs a nonzero state
    let mut rnd = move |m: u64| -> u64 {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s % m
    };
    const QN: u64 = 700;
    const BN: u64 = 700;
    let mut recs: Vec<[u64; 7]> = vec![];
    // kind, q_control2, q_control1, q_target, c_target, c_condition, r_target
    for r in 0..4u64 {
        recs.push([1, NOQ, NOQ, NOQ, NOQ, NOQ, r]);
    }
    for r in 0..4u64 {
        for i in 0..256u64 {
            if r < 2 {
                recs.push([2, NOQ, NOQ, r * 256 + i, NOQ, NOQ, r]);
            } else {
                recs.push([2, NOQ, NOQ, NOQ, (r - 2) * 256 + i, NOQ, r]);
            }
        }
    }
    let mut depth = 0u32;
    while recs.len() < n {
        let cond = if rnd(10) < 3 { rnd(BN) } else { NOQ };
        let q3 = |rnd: &mut dyn FnMut(u64) -> u64| -> (u64, u64, u64) {
            loop {
                let (x, y, z) = (rnd(QN), rnd(QN), rnd(QN));
                if x != y && y != z && x != z {
                    return (x, y, z);
                }
            }
        };
        let (x, y, z) = q3(&mut rnd);
        let rec = match rnd(100) {
            0..=29 => [13, x, y, z, NOQ, cond, NOQ],
            30..=49 => [8, NOQ, x, y, NOQ, cond, NOQ],
            50..=53 => [14, x, y, z, NOQ, cond, NOQ],
            54..=56 => [9, NOQ, x, y, NOQ, cond, NOQ],
            57..=61 => [10, NOQ, x, y, NOQ, cond, NOQ],
            62..=66 => [6, NOQ, NOQ, x, NOQ, cond, NOQ],
            67..=68 => [7, NOQ, NOQ, x, NOQ, cond, NOQ],
            69 => [0, NOQ, NOQ, NOQ, NOQ, cond, NOQ],
            70..=74 => [12, NOQ, NOQ, x, 512 + rnd(BN - 512), cond, NOQ],
            75..=77 => [11, NOQ, NOQ, x, NOQ, cond, NOQ],
            78..=80 => [3, NOQ, NOQ, NOQ, 512 + rnd(BN - 512), cond, NOQ],
            81..=82 => [4, NOQ, NOQ, NOQ, 512 + rnd(BN - 512), cond, NOQ],
            83..=84 => [5, NOQ, NOQ, NOQ, 512 + rnd(BN - 512), cond, NOQ],
            85..=89 => {
                if depth < 4 {
                    depth += 1;
                    [15, NOQ, NOQ, NOQ, NOQ, rnd(BN), NOQ]
                } else {
                    continue;
                }
            }
            90..=95 => {
                // pops at depth 0 are no-ops in the simulator
                depth = depth.saturating_sub(1);
                [16, NOQ, NOQ, NOQ, NOQ, NOQ, NOQ]
            }
            96 => [17, NOQ, NOQ, x, NOQ, cond, NOQ],
            _ => [8, NOQ, y, z, NOQ, NOQ, NOQ],
        };
        recs.push(rec);
    }
    let f = std::fs::File::create(out).unwrap();
    let mut w = std::io::BufWriter::new(f);
    w.write_all(b"QECCOPSZ").unwrap();
    w.write_all(&(recs.len() as u64).to_le_bytes()).unwrap();
    let mut enc = zstd::stream::write::Encoder::new(w, 3).unwrap();
    for r in &recs {
        enc.write_all(&(r[0] as u32).to_le_bytes()).unwrap();
        enc.write_all(&0u32.to_le_bytes()).unwrap();
        for v in &r[1..] {
            enc.write_all(&v.to_le_bytes()).unwrap();
        }
    }
    enc.finish().unwrap().flush().unwrap();
    eprintln!("wrote {} ops", recs.len());
}
