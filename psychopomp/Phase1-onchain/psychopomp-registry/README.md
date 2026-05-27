# psychopomp-registry — Phase-1

Pure-Rust state machine: [`../psychopomp-registry-core`](../psychopomp-registry-core).
Schema, instructions, and validation logic are implemented and unit-tested
there (no nssa coupling, no zkVM).

What's still needed to make this a deployable LEZ program (Phase-1, requires
the SPEL toolchain and a live sequencer):

1. Add a `methods/guest/` crate that wraps the core state machine in the
   nssa account-I/O conventions:
   - `#[lez_program]` + `#[instruction]` proc macros from `spel-framework`
   - Borsh-encode/decode `OperatorState` to/from `AccountWithMetadata`
   - Enforce caller-signed authorization (operator's ed25519 over the
     instruction bytes) for Update/Unbond/Withdraw
   - Gate `RecordSettlement` on caller-program-id == psychopomp-escrow
2. Add a `methods/` host crate (`risc0_build::embed_methods`)
3. Deploy via `wallet deploy-program` to the target sequencer, capture the
   resulting `ProgramId`, pin it in the escrow's allowed callers.

The wrapping is mechanical; the state machine is the substantive part and
is already complete.
