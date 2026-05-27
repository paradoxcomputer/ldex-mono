//! `cargo run --release -p psychopomp-chain --example image-id -- <user-elf>`
//!
//! Wraps the user ELF + the default risc0 v1-compat kernel into a
//! `ProgramBinary`, computes the IMAGE_ID, and prints it. This is exactly
//! what `cargo risczero build` would write to `<name>.bin`'s header.
//!
//! Outputs a binary suitable for `wallet deploy-program` to stdout when
//! invoked with `--bin <out.bin>`; otherwise just prints the image id.

use risc0_binfmt::ProgramBinary;
use risc0_zkos_v1compat::V1COMPAT_ELF;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let user_elf_path: PathBuf = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: image-id <user-elf> [--bin <out.bin>]"))?
        .into();
    let mut bin_out: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        if a == "--bin" {
            bin_out = Some(args.next().expect("--bin requires path").into());
        }
    }
    let user_elf = std::fs::read(&user_elf_path)?;
    let pb = ProgramBinary::new(&user_elf, V1COMPAT_ELF);
    let digest = pb.compute_image_id()?;
    println!("{}", hex::encode(digest.as_bytes()));
    if let Some(out) = bin_out {
        std::fs::write(&out, pb.encode())?;
        eprintln!("wrote {} ({} bytes)", out.display(), std::fs::metadata(&out)?.len());
    }
    Ok(())
}
