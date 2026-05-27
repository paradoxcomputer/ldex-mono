//! Synthetic heavy RISC0 guest. Reads (rounds: u32, seed: u32), iterates
//! `rounds` rounds of SHA-256 over a 32-byte state initialised from the seed,
//! commits (rounds, final_digest). Each round is one sha2::Sha256 hash of 32
//! bytes — ~24K RISC0 cycles in software, no accelerator. The workload scales
//! linearly with `rounds`, so the host can dial it to a target wall-clock.
//!
//! Purpose: produce a non-trivial proving job so a GPU prover's speedup over
//! the CPU baseline is visible in the e2e harness's wall_clock line. The
//! `hello` guest finishes in tens of ms and is dominated by HTTP + RISC0
//! setup; this guest is dominated by the proving work itself.

#![no_main]
risc0_zkvm::guest::entry!(main);

use risc0_zkvm::guest::env;
use sha2::{Digest, Sha256};

fn main() {
    let rounds: u32 = env::read();
    let seed: u32 = env::read();

    let mut state = [0u8; 32];
    state[..4].copy_from_slice(&seed.to_le_bytes());

    for _ in 0..rounds {
        let mut h = Sha256::new();
        h.update(state);
        state = h.finalize().into();
    }

    env::commit(&(rounds, state));
}
