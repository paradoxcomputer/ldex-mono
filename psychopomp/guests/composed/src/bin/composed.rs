//! Composed guest. Reads HELLO_ID + the expected (sum, prod) journal of an
//! inner hello receipt, calls env::verify(HELLO_ID, expected_journal) - which
//! consumes one assumption from the env's assumption pool - and commits the
//! pair plus a "ok" marker. Proves the assumption-pass-through path end-to-end.

#![no_main]
risc0_zkvm::guest::entry!(main);

use risc0_zkvm::guest::env;

fn main() {
    // Read inner image id (8 u32 words = 32 bytes) and the expected journal
    // bytes the inner receipt should commit.
    let inner_id: [u32; 8] = env::read();
    let expected_journal: Vec<u8> = env::read();

    // Pulls one matching AssumptionReceipt from the host-provided pool. If no
    // matching assumption is present, the guest panics → host receives a
    // ProgramProveFailed error.
    env::verify(inner_id, &expected_journal).expect("inner assumption verifies");

    // Commit a marker so the outer journal is non-trivial and the client can
    // round-trip-verify exactly what was proven.
    env::commit(&(inner_id, expected_journal));
}
