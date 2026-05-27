//! Print the deterministic LEZ program id (RISC0 image id) of a guest .bin,
//! as 32 hex bytes in the same native-endian [u32;8] layout the shim/PDA
//! derivation uses. Used by scripts/bootstrap.sh to get the deployed AMM
//! program id (the CLI's `deploy-program` prints nothing).
//!
//!   cargo run -q --release --example amm_program_id -- <path-to.bin>

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: amm_program_id <path-to-guest.bin>");
    let bytes = std::fs::read(&path).expect("read program bin");
    let program = nssa::program::Program::new(bytes).expect("valid RISC0 program");
    let pid = program.id(); // [u32; 8]
    let mut b = [0u8; 32];
    for (i, w) in pid.iter().enumerate() {
        b[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
    }
    let mut s = String::with_capacity(64);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    println!("{s}");
}
