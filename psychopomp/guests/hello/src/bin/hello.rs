//! Hello-world RISC0 guest. Reads two u32s from the host, writes (sum, product)
//! to the journal. Used as the Phase-0 end-to-end test vector.

#![no_main]
risc0_zkvm::guest::entry!(main);

use risc0_zkvm::guest::env;

fn main() {
    let a: u32 = env::read();
    let b: u32 = env::read();
    let sum = a.wrapping_add(b);
    let prod = a.wrapping_mul(b);
    env::commit(&(sum, prod));
}
