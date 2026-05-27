//! Re-exports HELLO_ELF and HELLO_ID built by risc0-build from
//! `../hello`. Constants are consumed by the e2e harness.

include!(concat!(env!("OUT_DIR"), "/methods.rs"));
