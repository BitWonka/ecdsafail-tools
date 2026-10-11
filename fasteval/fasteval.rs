//! fasteval: a parallel, exact replica of the challenge harness's `eval_circuit`.
//!
//! It uses the same ops.bin loader, Fiat-Shamir seed, shots and simulator semantics, and prints the same totals,
//! failure counts and score, in minutes instead of hours on billion-op circuits.
//!
//! Why batches can run in parallel and still match: `eval_circuit` draws everything from one SHAKE256 stream.
//! The first 64 * 9024 bytes are the shots. After that, every R/Hmr op reads 8 bytes per 64-shot batch, whether
//! or not it executes. So batch b's measurement bytes are always the 8*M bytes at offset 64*9024 + 8*M*b
//! (M = number of R/Hmr ops), whatever happened in earlier batches. One producer squeezes them in order;
//! worker threads simulate batches in parallel on a compact 20-byte op list.
//!
//! Drop-in: copy into <challenge-repo>/src/bin/ and `cargo build --release --bin fasteval`.
//! Usage:   fasteval [ops.bin]
//! Env:     FE_THREADS = worker threads (default 6)
//!          FE_SALT    = any string: a fresh, independent set of shots and measurement words for the same circuit,
//!                       for estimating the true failure rate. With it set the score is NOT the official one.

use quantum_ecc::circuit::{analyze_ops, BitId, Op, OperationType, QubitId, QubitOrBit, RegisterId, NO_BIT};
use quantum_ecc::weierstrass_elliptic_curve::WeierstrassEllipticCurve;
use ruint::aliases::U256;
use sha3::digest::{ExtendableOutput, Update, XofReader};
use sha3::Shake256;
use std::fs::File;
use std::io::{BufReader, Read};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const MAGIC: &[u8; 8] = b"QECCOPSZ";
const ZSTD_WINDOW_LOG_MAX: u32 = 27;
const OP_BYTES: usize = 56;
const MAX_OPS: u64 = 4_000_000_000;
const NUM_TESTS: usize = 9024;
const CH: usize = 1 << 22;

/// One SHAKE256 stream: absorb, then squeeze in pieces that continue the same stream.
enum Shake {
    Absorb(Shake256),
    Squeeze(sha3::Shake256Reader),
}

impl Shake {
    fn new() -> Self {
        Shake::Absorb(Shake256::default())
    }
    fn update(&mut self, d: &[u8]) {
        match self {
            Shake::Absorb(h) => h.update(d),
            Shake::Squeeze(_) => panic!("absorb after squeeze"),
        }
    }
    fn read(&mut self, out: &mut [u8]) {
        if let Shake::Absorb(h) = self {
            let h = std::mem::take(h);
            *self = Shake::Squeeze(h.finalize_xof());
        }
        if let Shake::Squeeze(r) = self {
            XofReader::read(r, out);
        }
    }
}

/// Compact op. Bit indices are shifted by one; bits[0] is all ones and stands for "no condition".
#[derive(Clone, Copy)]
#[repr(C)]
struct C {
    k: u32,
    c: u32,
    a: u32,
    b: u32,
    t: u32,
}

fn kind_from_u32(v: u32) -> Option<OperationType> {
    use OperationType::*;
    Some(match v {
        0 => Neg, 1 => Register, 2 => AppendToRegister, 3 => BitInvert, 4 => BitStore0, 5 => BitStore1,
        6 => X, 7 => Z, 8 => CX, 9 => CZ, 10 => Swap, 11 => R, 12 => Hmr, 13 => CCX, 14 => CCZ,
        15 => PushCondition, 16 => PopCondition, 17 => DebugPrint,
        _ => return None,
    })
}

fn rd(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

fn secp256k1() -> WeierstrassEllipticCurve {
    let h = |s: &str| U256::from_str_radix(s, 16).unwrap();
    WeierstrassEllipticCurve {
        modulus: h("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F"),
        a: U256::from(0),
        b: U256::from(7),
        gx: h("79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798"),
        gy: h("483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8"),
        order: h("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141"),
    }
}

struct Loaded {
    ops: Vec<C>,
    n_ops: usize,
    m_rng: usize,
    nq: u64,
    nbits: u64,
    regs: Vec<Vec<QubitOrBit>>,
    xof: Shake,
}

fn load(path: &str) -> Result<Loaded, String> {
    let mut file = File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let mut header = [0u8; 16];
    file.read_exact(&mut header).map_err(|e| format!("header: {e}"))?;
    if &header[..8] != MAGIC {
        return Err("bad magic".into());
    }
    let n = u64::from_le_bytes(header[8..].try_into().unwrap());
    if n > MAX_OPS {
        return Err(format!("op count {n} exceeds cap"));
    }
    let n = n as usize;
    let (tx_h, rx_h) = sync_channel::<Arc<Vec<u8>>>(3);
    let (tx_c, rx_c) = sync_channel::<Arc<Vec<u8>>>(3);
    std::thread::scope(|s| {
        let dec = s.spawn(move || -> Result<(), String> {
            let mut dec = zstd::stream::read::Decoder::new(BufReader::new(file)).map_err(|e| format!("zstd: {e}"))?;
            dec.window_log_max(ZSTD_WINDOW_LOG_MAX).map_err(|e| format!("zstd: {e}"))?;
            let mut left = n;
            let mut busy = 0f64;
            while left > 0 {
                let m = left.min(CH);
                let tt = Instant::now();
                let mut buf = vec![0u8; m * OP_BYTES];
                dec.read_exact(&mut buf).map_err(|e| format!("short read: {e}"))?;
                busy += tt.elapsed().as_secs_f64();
                let a = Arc::new(buf);
                if tx_h.send(a.clone()).is_err() || tx_c.send(a).is_err() {
                    return Err("consumer gone".into());
                }
                left -= m;
            }
            drop((tx_h, tx_c));
            eprintln!("  [load] zstd decode busy {busy:.1}s");
            let mut extra = [0u8; 1];
            match dec.read(&mut extra) {
                Ok(0) => Ok(()),
                Ok(_) => Err("trailing data".into()),
                Err(e) => Err(format!("trailing check: {e}")),
            }
        });
        let hasher = s.spawn(move || {
            let mut h = Shake::new();
            h.update(b"quantum_ecc-fiat-shamir-v2");
            // FE_SALT (failure-rate estimates only): a fresh set of shots and measurement words for the same circuit
            if let Ok(salt) = std::env::var("FE_SALT") {
                h.update(b"fe-salt:");
                h.update(salt.as_bytes());
            }
            h.update(&(n as u64).to_le_bytes());
            let mut ser = Vec::with_capacity(CH * 49);
            let mut busy = 0f64;
            for chunk in rx_h {
                let tt = Instant::now();
                ser.clear();
                for rec in chunk.chunks_exact(OP_BYTES) {
                    ser.push(rec[0]);
                    ser.extend_from_slice(&rec[8..56]);
                }
                h.update(&ser);
                busy += tt.elapsed().as_secs_f64();
            }
            eprintln!("  [load] FS hash busy {busy:.1}s");
            h
        });
        // convert in this thread
        let mut ops: Vec<C> = Vec::with_capacity(n);
        let (mut nq, mut nbits, mut m_rng) = (0u64, 0u64, 0usize);
        let mut regs: Vec<Vec<QubitOrBit>> = vec![];
        let mut seen = 0usize;
        let mut err: Option<String> = None;
        let mut chunk_ops: Vec<Op> = Vec::with_capacity(CH);
        let mut cbusy = 0f64;
        for chunk in rx_c {
            if err.is_some() {
                continue;
            }
            let tt = Instant::now();
            chunk_ops.clear();
            for rec in chunk.chunks_exact(OP_BYTES) {
                let kr = u32::from_le_bytes(rec[0..4].try_into().unwrap());
                let Some(kind) = kind_from_u32(kr) else {
                    err = Some(format!("op {seen}: unknown kind {kr}"));
                    break;
                };
                let op = Op {
                    kind,
                    q_control2: QubitId(rd(rec, 8)),
                    q_control1: QubitId(rd(rec, 16)),
                    q_target: QubitId(rd(rec, 24)),
                    c_target: BitId(rd(rec, 32)),
                    c_condition: BitId(rd(rec, 40)),
                    r_target: RegisterId(rd(rec, 48)),
                };
                if std::panic::catch_unwind(|| op.validate()).is_err() {
                    err = Some(format!("op {seen}: validate failed"));
                    break;
                }
                chunk_ops.push(op);
                seen += 1;
            }
            if err.is_some() {
                continue;
            }
            let (cq, cb, _, cr) = analyze_ops(chunk_ops.iter());
            nq = nq.max(cq);
            nbits = nbits.max(cb);
            if regs.len() < cr.len() {
                regs.resize(cr.len(), vec![]);
            }
            for (i, r) in cr.into_iter().enumerate() {
                regs[i].extend(r);
            }
            for op in &chunk_ops {
                let q = |x: QubitId| -> u32 {
                    assert!(x.0 < u32::MAX as u64);
                    x.0 as u32
                };
                let bt = |x: BitId| -> u32 {
                    assert!(x.0 < (u32::MAX - 1) as u64);
                    x.0 as u32 + 1
                };
                let c = if op.c_condition == NO_BIT { 0 } else { bt(op.c_condition) };
                use OperationType::*;
                let (a, b, t) = match op.kind {
                    Register | AppendToRegister | DebugPrint => continue,
                    CCX | CCZ => (q(op.q_control1), q(op.q_control2), q(op.q_target)),
                    CX | CZ | Swap => (q(op.q_control1), 0, q(op.q_target)),
                    X | Z | R => (0, 0, q(op.q_target)),
                    Hmr => (0, bt(op.c_target), q(op.q_target)),
                    BitInvert | BitStore0 | BitStore1 => (0, 0, bt(op.c_target)),
                    Neg | PushCondition | PopCondition => (0, 0, 0),
                };
                if matches!(op.kind, R | Hmr) {
                    m_rng += 1;
                }
                ops.push(C { k: op.kind as u32, c, a, b, t });
            }
            cbusy += tt.elapsed().as_secs_f64();
        }
        eprintln!("  [load] convert busy {cbusy:.1}s");
        let dres = dec.join().unwrap();
        let xof = hasher.join().unwrap();
        if let Some(e) = err {
            return Err(e);
        }
        dres?;
        Ok(Loaded { ops, n_ops: n, m_rng, nq, nbits, regs, xof })
    })
}

struct BatchRes {
    batch: usize,
    tof: u64,
    cliff: u64,
    classical: usize,
    first_fail: Option<String>,
    phase_bad: bool,
    fail_shots: u32,
    anc_bad: bool,
}

#[inline(always)]
unsafe fn run_ops(ops: &[C], q: &mut [u64], bits: &mut [u64], rng: &[u64]) -> (u64, u64, u64) {
    let (mut tof, mut cliff, mut phase) = (0u64, 0u64, 0u64);
    let mut base = u64::MAX;
    let mut stack: Vec<u64> = Vec::with_capacity(64);
    let mut ri = 0usize;
    let qp = q.as_mut_ptr();
    let bp = bits.as_mut_ptr();
    for op in ops {
        let cond = base & *bp.add(op.c as usize);
        match op.k {
            13 => {
                tof += cond.count_ones() as u64;
                *qp.add(op.t as usize) ^= cond & *qp.add(op.a as usize) & *qp.add(op.b as usize);
            }
            8 => {
                cliff += cond.count_ones() as u64;
                *qp.add(op.t as usize) ^= cond & *qp.add(op.a as usize);
            }
            14 => {
                tof += cond.count_ones() as u64;
                phase ^= cond & *qp.add(op.t as usize) & *qp.add(op.a as usize) & *qp.add(op.b as usize);
            }
            9 => {
                cliff += cond.count_ones() as u64;
                phase ^= cond & *qp.add(op.t as usize) & *qp.add(op.a as usize);
            }
            10 => {
                cliff += cond.count_ones() as u64;
                let mut c1 = *qp.add(op.a as usize);
                let mut tt = *qp.add(op.t as usize);
                c1 ^= tt;
                tt ^= cond & c1;
                c1 ^= tt;
                *qp.add(op.a as usize) = c1;
                *qp.add(op.t as usize) = tt;
            }
            11 => {
                cliff += cond.count_ones() as u64;
                let r = *rng.get_unchecked(ri);
                ri += 1;
                phase ^= *qp.add(op.t as usize) & r & cond;
                *qp.add(op.t as usize) &= !cond;
            }
            12 => {
                cliff += cond.count_ones() as u64;
                let r = *rng.get_unchecked(ri);
                ri += 1;
                *bp.add(op.b as usize) &= !cond;
                *bp.add(op.b as usize) ^= r & cond;
                phase ^= *qp.add(op.t as usize) & r & cond;
                *qp.add(op.t as usize) &= !cond;
            }
            6 => *qp.add(op.t as usize) ^= cond,
            7 => phase ^= cond & *qp.add(op.t as usize),
            0 => phase ^= cond,
            3 => *bp.add(op.t as usize) ^= cond,
            4 => *bp.add(op.t as usize) &= !cond,
            5 => *bp.add(op.t as usize) |= cond,
            15 => {
                stack.push(base);
                base &= *bp.add(op.c as usize);
            }
            16 => {
                if let Some(v) = stack.pop() {
                    base = v;
                }
            }
            _ => std::hint::unreachable_unchecked(),
        }
    }
    debug_assert_eq!(ri, rng.len());
    (tof, cliff, phase)
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "ops.bin".into());
    // the measurement stream is squeezed by one thread; a few workers keep up with it
    let threads: usize = std::env::var("FE_THREADS").ok().and_then(|s| s.parse().ok()).unwrap_or(6);
    let t0 = Instant::now();
    println!("=== fasteval (parallel exact replica of eval_circuit) ===");
    if let Ok(salt) = std::env::var("FE_SALT") {
        println!("  FE_SALT={salt}: salted shots, NOT the official score");
    }
    let mut l = match load(&path) {
        Ok(l) => l,
        Err(e) => {
            println!("!! load FAILED: {e}");
            std::process::exit(1);
        }
    };
    let regs = std::mem::take(&mut l.regs);
    println!("  loaded ops  : {} (kept {} sim ops, {} R/Hmr) in {:.1}s", l.n_ops, l.ops.len(), l.m_rng, t0.elapsed().as_secs_f64());
    if regs.len() != 4 || regs.iter().any(|r| r.len() != 256) {
        println!("!! register shape FAILED: {} registers", regs.len());
        std::process::exit(1);
    }
    for (i, r) in regs.iter().enumerate() {
        let want_q = i < 2;
        if r.iter().any(|x| matches!(x, QubitOrBit::Qubit(_)) != want_q) {
            println!("!! register {i} kind FAILED");
            std::process::exit(1);
        }
    }
    println!("  qubits      : {}", l.nq);
    println!("  bits        : {}", l.nbits);

    // shots: the first 64*9024 stream bytes, in order; curve work in parallel
    let t1 = Instant::now();
    let mut ks = vec![[[0u8; 32]; 2]; NUM_TESTS];
    for kk in ks.iter_mut() {
        l.xof.read(&mut kk[0]);
        l.xof.read(&mut kk[1]);
    }
    let curve = secp256k1();
    let mut att: Vec<Option<((U256, U256), (U256, U256), (U256, U256))>> = vec![None; NUM_TESTS];
    std::thread::scope(|s| {
        let per = (NUM_TESTS + threads - 1) / threads;
        for (ci, chunk) in att.chunks_mut(per).enumerate() {
            let ks = &ks;
            let curve = &curve;
            s.spawn(move || {
                for (j, slot) in chunk.iter_mut().enumerate() {
                    let i = ci * per + j;
                    let k1 = U256::from_le_bytes(ks[i][0]);
                    let k2 = U256::from_le_bytes(ks[i][1]);
                    let t = curve.mul(curve.gx, curve.gy, k1);
                    let o = curve.mul(curve.gx, curve.gy, k2);
                    if t.0 == o.0 || (t.0.is_zero() && t.1.is_zero()) || (o.0.is_zero() && o.1.is_zero()) {
                        continue;
                    }
                    let e = curve.add(t.0, t.1, o.0, o.1);
                    *slot = Some((t, o, e));
                }
            });
        }
    });
    let cases: Vec<_> = att.into_iter().flatten().collect();
    let n = cases.len();
    let nb = (n + 63) / 64;
    println!("  shots       : {} ({} batches) in {:.1}s", n, nb, t1.elapsed().as_secs_f64());

    // measurement stream producer + batch workers
    let t2 = Instant::now();
    let m = l.m_rng;
    let ops = &l.ops;
    let (nq, nbits) = (l.nq as usize, l.nbits as usize);
    let (tx, rx) = sync_channel::<(usize, Vec<u64>)>(2);
    let rx = Arc::new(Mutex::new(rx));
    let (ftx, frx) = std::sync::mpsc::channel::<Vec<u64>>();
    let (rtx, rrx) = std::sync::mpsc::channel::<BatchRes>();
    let mut xof = l.xof;
    let mut results: Vec<BatchRes> = std::thread::scope(|s| {
        s.spawn(move || {
            let mut busy = 0f64;
            for b in 0..nb {
                let tt = Instant::now();
                let mut v = frx.try_recv().unwrap_or_else(|_| vec![0u64; m]);
                let bytes = unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, m * 8) };
                xof.read(bytes);
                for w in v.iter_mut() {
                    *w = u64::from_le(*w);
                }
                busy += tt.elapsed().as_secs_f64();
                if tx.send((b, v)).is_err() {
                    return;
                }
            }
            eprintln!("  [sim] squeeze busy {busy:.1}s");
        });
        for _ in 0..threads {
            let rx = rx.clone();
            let rtx = rtx.clone();
            let ftx = ftx.clone();
            let cases = &cases;
            let regs = &regs;
            s.spawn(move || {
                let mut q = vec![0u64; nq];
                let mut bits = vec![0u64; nbits + 1];
                loop {
                    let job = rx.lock().unwrap().recv();
                    let Ok((b, rng)) = job else { break };
                    let bs = 64.min(n - b * 64);
                    let mask = if bs == 64 { u64::MAX } else { (1u64 << bs) - 1 };
                    q.fill(0);
                    bits.fill(0);
                    bits[0] = u64::MAX;
                    let put = |q: &mut [u64], bits: &mut [u64], reg: &[QubitOrBit], val: U256, shot: usize| {
                        for (i, it) in reg.iter().enumerate() {
                            let w = match it {
                                QubitOrBit::Qubit(id) => &mut q[id.0 as usize],
                                QubitOrBit::Bit(id) => &mut bits[id.0 as usize + 1],
                            };
                            if val.bit(i) { *w |= 1 << shot } else { *w &= !(1 << shot) }
                        }
                    };
                    for sh in 0..bs {
                        let (t, o, _) = cases[b * 64 + sh];
                        put(&mut q, &mut bits, &regs[0], t.0, sh);
                        put(&mut q, &mut bits, &regs[1], t.1, sh);
                        put(&mut q, &mut bits, &regs[2], o.0, sh);
                        put(&mut q, &mut bits, &regs[3], o.1, sh);
                    }
                    let tt = Instant::now();
                    let (tof, cliff, phase) = unsafe { run_ops(ops, &mut q, &mut bits, &rng) };
                    if b < threads {
                        eprintln!("  [sim] batch {b}: {:.2}s", tt.elapsed().as_secs_f64());
                    }
                    let _ = ftx.send(rng);
                    let get = |q: &[u64], bits: &[u64], reg: &[QubitOrBit], shot: usize| -> U256 {
                        let mut v = U256::ZERO;
                        for (i, it) in reg.iter().enumerate() {
                            let w = match it {
                                QubitOrBit::Qubit(id) => q[id.0 as usize],
                                QubitOrBit::Bit(id) => bits[id.0 as usize + 1],
                            };
                            v.set_bit(i, (w >> shot) & 1 != 0);
                        }
                        v
                    };
                    let mut r = BatchRes { batch: b, tof, cliff, classical: 0, first_fail: None, phase_bad: false, fail_shots: 0, anc_bad: false };
                    let mut lanes = 0u64;
                    for sh in 0..bs {
                        let i = b * 64 + sh;
                        let e = cases[i].2;
                        let gx = get(&q, &bits, &regs[0], sh);
                        let gy = get(&q, &bits, &regs[1], sh);
                        if gx != e.0 || gy != e.1 {
                            r.classical += 1;
                            lanes |= 1u64 << sh;
                            if r.first_fail.is_none() {
                                r.first_fail = Some(format!("CLASSICAL MISMATCH shot {i}: got ({:#x},{:#x}) exp ({:#x},{:#x})", gx, gy, e.0, e.1));
                            }
                        }
                    }
                    let ph = phase & mask;
                    lanes |= ph;
                    if ph != 0 {
                        r.phase_bad = true;
                        if r.first_fail.is_none() {
                            r.first_fail = Some(format!("PHASE GARBAGE: global_phase = {:#018x} across {} live shots (must be 0)", ph, bs));
                        }
                    }
                    for reg in regs.iter() {
                        for it in reg {
                            if let QubitOrBit::Qubit(id) = it {
                                q[id.0 as usize] = 0;
                            }
                        }
                    }
                    for g in 0..nq {
                        lanes |= q[g] & mask;
                    }
                    r.fail_shots = lanes.count_ones();
                    if let Some(g) = (0..nq).find(|&g| q[g] & mask != 0) {
                        r.anc_bad = true;
                        if r.first_fail.is_none() {
                            r.first_fail = Some(format!("ANCILLA GARBAGE: qubit {} = {:#018x} (live shots) at end of forward", g, q[g] & mask));
                        }
                    }
                    if rtx.send(r).is_err() {
                        break;
                    }
                }
            });
        }
        drop(rtx);
        drop(ftx);
        let mut out = vec![];
        for r in rrx {
            if out.len() % 16 == 15 {
                eprintln!("  .. {} / {} batches, {:.0}s", out.len() + 1, nb, t2.elapsed().as_secs_f64());
            }
            out.push(r);
        }
        out
    });
    results.sort_by_key(|r| r.batch);
    assert_eq!(results.len(), nb);
    let tot_tof: u64 = results.iter().map(|r| r.tof).sum();
    let tot_cliff: u64 = results.iter().map(|r| r.cliff).sum();
    let classical: usize = results.iter().map(|r| r.classical).sum();
    let phase_b = results.iter().filter(|r| r.phase_bad).count();
    let anc_b = results.iter().filter(|r| r.anc_bad).count();
    let fail = results.iter().find_map(|r| r.first_fail.clone());
    let denom = n.max(1) as f64;
    let avg_tof = tot_tof as f64 / denom;
    let avg_cliff = tot_cliff as f64 / denom;
    println!("  simulated {} batches in {:.1}s ({} threads)", nb, t2.elapsed().as_secs_f64(), threads);
    println!("  tested shots            : {}", n);
    println!("  classical mismatches    : {}", classical);
    println!("  phase-garbage batches   : {}", phase_b);
    println!("  ancilla-garbage batches : {}", anc_b);
    println!("  failing shots (any)     : {}", results.iter().map(|r| r.fail_shots as u64).sum::<u64>());
    println!("  avg executed Toffoli  : {:.3}", avg_tof);
    println!("  avg executed Clifford : {:.3}", avg_cliff);
    println!("  total Toffoli (sum)   : {} over {} shots", tot_tof, n);
    println!("  total Clifford (sum)  : {}", tot_cliff);
    println!("  emitted ops           : {}", l.n_ops);
    println!("  qubits                : {}", l.nq);
    match fail {
        Some(f) => println!("!! correctness FAILED: {f}"),
        None => {
            let score = (avg_tof.round() as u64).saturating_mul(l.nq);
            println!("  all {} shots OK; score = {} x {} = {}", n, avg_tof.round() as u64, l.nq, score);
        }
    }
    println!("  wall time {:.1}s", t0.elapsed().as_secs_f64());
}
