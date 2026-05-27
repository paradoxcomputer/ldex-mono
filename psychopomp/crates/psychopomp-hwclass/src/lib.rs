//! `HwClass` — minimal shared type the LEZ guests need.
//!
//! Kept in its own no-deps crate so the LEZ guest crates can pull it without
//! cascading the full `psychopomp-types` dep graph (which brings uuid /
//! serde_json / sha2 and breaks the `riscv32im-risc0-zkvm-elf` target).

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum HwClass {
    Stub = 0,
    H100CC = 1,
    MI300SEV = 2,
    TDX = 3,
}

impl std::str::FromStr for HwClass {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "stub" => Ok(Self::Stub),
            "h100" | "h100cc" => Ok(Self::H100CC),
            "mi300" | "mi300sev" => Ok(Self::MI300SEV),
            "tdx" => Ok(Self::TDX),
            other => Err(format!("unknown hw_class: {other}")),
        }
    }
}
