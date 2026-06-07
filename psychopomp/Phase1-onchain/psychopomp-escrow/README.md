# psychopomp-escrow - Phase-1

Pure-Rust state machine: [`../psychopomp-escrow-core`](../psychopomp-escrow-core).
Per-job lifecycle (Open → Awarded → Settled | Refunded), balance-delta DSL
that the LEZ wrapper applies to client and operator accounts. Unit-tested
for the happy path, liveness fault (post-deadline only), correctness fault
(any time after award), filter mismatch on Accept.

To make this a deployable LEZ program (Phase-1, requires the SPEL toolchain
and a live sequencer):

1. Add a `methods/guest/` crate that:
   - Wraps the core state machine in nssa account I/O
   - Re-verifies the inbound `stark` (borsh-decoded `risc0_zkvm::Receipt`)
     against the per-job IMAGE_ID inside the guest's `Settle` handler.
   - Re-verifies the inbound `attestation` via `psychopomp_attest::Verifier_`
     (Phase-1 NRAS once the real attestor lands).
   - Calls `psychopomp-registry::RecordSettlement` as a chained call on
     Settle (success) and Fault (failure) so the operator's reputation is
     updated in the same proof.
2. Add `methods/` (`risc0_build::embed_methods`).
3. Deploy and capture `ProgramId`.

The two non-trivial parts (per-job lifecycle + balance-delta DSL) are done.
Wrapping is a Phase-1 step gated on having a sequencer to deploy into.
