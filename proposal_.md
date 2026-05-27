# Your Project Name

LDEX and Psychopomp

# Team or Organization Name

Paradox Computer

# Primary Contact
logos@paradox.computer

Team Members

# Team

**Member 1**
Name or pseudonym: degenatronix
Role: Lead dev, engineer
Status: Full-time

**Member 2**
Name or pseudonym: wolverine
Role: Designer, frontend dev, UI/X
Status: Part-time

**Member 3**
Name or pseudonym: KaosTse
Role: PM, accounting, legal
Status: Part-time

//

# Project Summary

RFP-004 asks for a privacy-preserving DEX on the Logos Execution Zone. LDEX delivers it: a constant-product AMM with a 4-tier fee schedule (0.01 / 0.05 / 0.30 / 1.00 %), atomic deshield → swap → re-shield proven inside a single RISC-Zero STARK, ATA-based public flows, on-chain analytics, and three per-trade privacy modes (Public, PrivateOwned, Disposable account-A) so users choose their own latency/linkability trade-off, including the literal RFP "fresh account per op" model. The already existing codebase is close to feature-complete on a self-hosted L1-backed devnet today, mainly through cli; functional, usability, reliability, performance and privacy requirements in `docs/request.md` were live-verified and mapped in `docs/request_met.md`.

Although functionality is well advanced, it is not the bar. A swap that takes 12–25 minutes is not a DEX users will trust, it's a demo. LDEX's privacy modes are CPU-bound on a real STARK; on commodity hardware (Ryzen 7 PRO 7840U) mode-1 swaps take 12 m 29 s and mode-2 disposable swaps 23 m 38 s. That gap between "functional" and "usable" is exactly the gap that **Psychopomp** closes. Psychopomp is a TEE-attested GPU prover marketplace, native to LEZ, that lets a user outsource STARK generation without exposing the unsealed witness. Phase-0 + Phase-1 are already shipped (operator registration, escrow, settlement, fault path can be tested on a dedicated sequencer with full chained-call mechanics); what's missing is the LDEX consumer side: the mini-app/CLI wiring that picks a Psychopomp operator per swap, posts the witness over the encrypted bridge, and consumes the returned STARK. With that wiring, the same mode-2 swap drops from ~24 min CPU to ~1–2 min on a GPU/TEE prover, with no protocol change on the LEZ side, as the chain still verifies a normal `PRIVACY_PRESERVING_CIRCUIT_ID` STARK.

This proposal packages the two halves as one deliverable because they are one deliverable. LDEX without a fast prover is a benchmark; a fast
prover without an application is infrastructure looking for users. The combination is a solid end-user-shaped privacy product on LEZ, with the prover layer already engineered for future privacy apps too. The work is mostly hardening and integration: land LDEX on the canonical Logos testnet, harden and enhance LDEX-core, wire the mini-app/CLI, integrate Psychopomp's operator marketplace, build the test framework that protects the UX over time (we used it during this proposal's drafting to find and fix a real shielded-balance display bug), and pay for an audit before mainnet.

LDEX matters because it is the canonical demonstration that LEZ's privacy-preserving transactions are practical for end-user applications, not just primitive transfers. It exercises every part of the stack: privacy circuit, chained-call orchestration, wallet FFI, SPEL, Basecamp module hosting, and the prover. It produces a reusable template for every later privacy app on LEZ. Psychopomp matters because it makes that template viable on real hardware. Funded together, LDEX + Psychopomp ship a powerful, usable privacy-app + prover combination Logos can point at and say: this is what privacy on LEZ feels like.

//

# Technical Approach

Two parallel tracks, joined at one integration point (Milestone 3).

## Track A: LDEX (the privacy DEX)

**Architecture**

- AMM program (`amm_v2`): single LEZ program covering pool create,   add/remove liquidity, mode-0 / mode-1 (PrivateOwned) / mode-2
  (Disposable) swaps and native-LEZ batched variants. Eight instructions, one upstream-compatible IMAGE_ID. No nssa changes,
  no sequencer changes, ships directly to the canonical Logos testnet.

- WLEZ (wrapped native LEZ): bridge so native LEZ participates in the AMM. Wrap and unwrap chain inside private swaps that touch native
  LEZ, so the user pays in LEZ and receives LEZ without ever holding  WLEZ outside the proof.

- ATA program: deterministic associated token accounts derived from  `(owner, def_id)`. Every public AMM op routes through PDA-owned
  holdings; LP positions live in `ATA(owner, lp_def)`.

**Stack**

- Programs: Rust + RISC-Zero zkVM + SPEL framework. Constant-product  math in `amm_core` with `u128` reserves. Deterministic IMAGE_IDs so the same source produces the same program id on any L1.

- FFI (`ldex-amm-ffi`): single C-ABI cdylib exposing ~30 functions to the Basecamp module. Wraps the wallet, privacy-tx builder, program
  serialisers, with poll-for-inclusion semantics so `rc=0` means "landed in a block" not "in mempool".

- Mini-app: Logos Basecamp module pair (`ldex_ui` QML + `ldex_core` native C++). `ldex_core_plugin.cpp` adapts the
  Q_INVOKABLE surface to the FFI. Sandboxed; QML drives every flow through `logos.callModule`.

- CLI (`ldex`): single-binary command-line client mirroring every mini-app action (status, balance, pools, quote, swap, wrap, shield,
  fund, pool-create, liq add/remove). Same FFI as the mini-app, so the CLI is the floor for "does the chain side work" and a complete
  alternative when the GUI is being polished.

- Build: Nix flakes for the mini-app + .lgx packaging; cargo workspaces for programs/FFI; `setup.sh` for a fresh-clone single-step bootstrap.

## Track B: Psychopomp (the prover backbone)

**Architecture (Phase-0 and Phase-1 in active development)**

- Operator registry: on-chain `Register → OperatorState` with a reputation/stake record per operator.

- Escrow — user posts a STARK request with bond + payment; operator   accepts with their own bond; payment unlocks on settlement.

- Settlement — `Post → Accept → Settle` chained calls that increment   `OperatorState.rep.successes` and refund the user's bond on success.

- Fault path — liveness-fault timer; if the operator doesn't settle by   deadline, `Fault::Liveness` chains to refund + `rep.liveness_faults++`.

- TEE attestation — operators run in confidential compute (SEV-SNP / TDX / SGX) and attach a quoted attestation to each receipt. The on-
  chain artifact is the standard RISC-Zero STARK. Verifiable by any LEZ node with no trust in the operator. The off-chain confidentiality
  (the unsealed witness never leaves the TEE) is what attestation buys; the chain-side verifiability is unchanged.

**Stack**

- LEZ programs: `psychopomp_registry`, `psychopomp_escrow`. 
- Operator binary: Rust, links the same `wallet-ffi` LDEX uses, runs the RISC-Zero prover (CUDA / cuMemcpy bridge inside the TEE).
- 43 unit tests + clippy-strict + 11/11 end-to-end harness. Phase-0 (off-chain) and Phase-1 (on-chain economic loop) both under test; resume
  notes in psychopomp/PROGRESS.md.

## Integration (Milestone 3)

LDEX side: a single env-var switch in the mini-app/CLI configuration (`LDEX_PROVER=local|psychopomp) selects the backend per swap. The Psychopomp path is the substantive work: a small client library that picks an operator from the on-chain registry by latency + price + reputation, posts the witness via the encrypted operator-channel, and returns the receipt to the mini-app's FFI. The chain-side artifact is unchanged (a standard PRIVACY_PRESERVING_CIRCUIT_ID receipt).

Latency target after M3: mode-1 swap from 12 m 29 s → ~1 min, mode-2 disposable from 23 m 38 s → ~1–2 min on the operator's GPU. The
acceptance criterion is concrete and measured.

## Anticipated Challenges and Solutions

1. Mini-app QtRO bridge timeout. The Logos host's standalone-app bridges QML ↔ plugin via QtRO with a hardcoded 20-second timeout
   for synchronous callModule. Any plugin op that opens a fresh wallet    handle pays a sync cost; under load, the 20-second window closes
   before the response arrives, and the UI shows "Failed to invoke callRemoteMethod". Mitigation: cache the wallet handle in the
   plugin (open once at module init, refresh on a debounced timer), route every chain-mutating call through callModuleAsync. Both are
   tracked in M2 alongside the test framework that catches the class of bug.

2. CPU STARK latency. Mode-1 / mode-2 swaps are CPU-bound on the real STARK; users won't wait 20+ minutes for a swap. Mitigation: M3
   (Psychopomp wiring) drops this to ~1–2 min and is work this proposal funds.

3. Psychopomp operator economics. The marketplace's fee + bond curve matters: too cheap and operators don't bother; too expensive and users won't switch from local proving. The on-chain registry + escrow are already in place; M3 calibrates the default fee + writes a runbook for operators who want to join.

# Milestones

**Duration:** 10 weeks

## Milestone 0: Already shipped
Payout:   $11,000
Duration: done and already in repo
Scope basis:  

Approximate market value of work already on disk at a professional dev-studio rate, discounted because this was undertaken at the team's own risk before the grant existed. The discount converts sunk-cost dollars into delivery insurance for Logos. You're not buying speculative work, you're             paying for live-verified artifacts a reviewer can compile, run, and grep `tx=…` against right now.

Delivered:
**LDEX track**

- amm_v2 program (8-instruction superset of the canonical amm; mode-0/1/2 swaps, native-LEZ batched in+out)
- token, ata, private_swap_router, wlez programs
- ldex-amm-ffi C-ABI cdylib (~30 functions, poll-for-inclusion semantics)
- mini-app (QML ui module + C++ ldex_core plugin)
- ldex CLI (full-surface single binary)
- Layer A integration tests + regression for the real shielded-balance display bug we found
- SETUP.md + setup.sh + portability sweep
 - 4 in-tree design / progress / request-met docs

**Psychopomp track**

- psychopomp_registry + psychopomp_escrow LEZ programs (deployed, live on dedicated sequencer)
- Phase-0 off-chain operator economic loop
- Phase-1 on-chain Register→Post→Accept→Settle chained-call mechanics + fault path
- operator binary linking the same wallet-ffi
 - 43 unit tests + clippy-strict + 11/11 e2e harness, all green

## Milestone 1: LDEX on canonical Logos testnet
Payout:        $5,000
Duration: 1 week
Deliverables:  
- LDEX programs deployed to public Logos testnet (token, ata, amm, amm_v2, wlez)
- Faucet-fed bootstrap variant (no genesis funding; derives funds from the Logos testnet faucet)
- End-to-end smoke proven on canonical testnet: pool create, mode-0 ATA swap, wrap/unwrap, mode-1 PrivateOwned swap, mode-2 Disposable swap, native-batched IN/OUT, public LP add/remove, private LP add/remove
- Public tx hashes + reconciliation doc

## Milestone 2: Test framework + UX hardening
Payout:        $10,000
Duration:      2 weeks
Deliverables:  
- Layer A: plugin/FFI integration tests (Rust harness against a live bootstrap, every Q_INVOKABLE method, JSON-shape + value assertions). Scaffold + 7 tests already in repo as M2 preview.
- Layer B: per-screen render baselines (qmltestrunner offscreen with stubbed `logos`, text/colour asserts to catch label drift other types of visual confusion)
- Layer C: headless end-to-end (drive button clicks against a real chain, assert state diffs, catches refresh staleness and async-pump bugs)
- CI: GitHub Actions running all three on every PR, gated on PASS before merge
- Plugin fix: cache the wallet handle in the C++ plugin so a Q_INVOKABLE call doesn't open + sync a fresh wallet per call (root cause of the QtRO 20-second-timeout cascade errors)
- UX pass: STARK-in-progress overlay with the real ETA; visual disambiguations; post-action balance refresh

## Milestone 3: Psychopomp prover integration
Payout:        $15,000
Duration:      4 weeks
Deliverables:  

- Psychopomp consumer client library (Rust crate) that picks an operator from the on-chain registry by latency + price + reputation, posts the witness via the encrypted operator-channel, and returns the receipt
- Mini-app + CLI selector: `LDEX_PROVER=local|psychopomp` env var (and a per-swap UI toggle in the mini-app)
- Operator runbook + reference Docker image so a third party can stand up a Psychopomp operator against the LDEX user demand
- Latency measurements before/after on a fixed reference workload: mode-1, mode-2, native-batched
- Acceptance criterion: mode-2 disposable < 2 min wall-clock end-to-end (submission → returned receipt → tx landed) on the chosen GPU/TEE backend

## Milestone 4: Audit + remediation
Payout:        $5,000 + audit costs
Duration:      2 weeks (post-audit; audit window itself is auditor-paced)
Deliverables:  
- Audit report from a Logos-approved security firm (team suggestion: <https://cure53.de> or <https://symbolic.software>) covering: amm_v2 swap/LP math, fee-tier accounting, privacy proof composition, WLEZ wrap/unwrap, FFI surface, Psychopomp encrypted operator channel, mini-app input validation
- All Critical / High findings remediated
- Medium findings either fixed or documented with acknowledged risk
- Re-audit sign-off

## Milestone 5: Logos-docs PR
Payout:        $1,000
Duration:      1 week
Deliverables:  
- PR to logos-co/logos-docs landing the LDEX module documentation (already drafted at docs/logos-docs-packet.md) plus a Psychopomp operator-onboarding page

# Total Requested Budget (USD)

$47,000

# Existing Work

LDEX repository with clear instructions to bootstrap and run the mini-app: https://gitlab.com/paradoxcomputer/ldex-mono
Psychopomp repository: https://gitlab.com/paradoxcomputer/ldex-mono/-/tree/main/psychopomp

//

# Future Work

As discussed, the team would like to have a discussion about post-delivery plans after the RFP is completed. Overall, the team is open to maintaining and managing the project after delivery, but prefer to stay cautious and under-promising on the front as of now.

# Permissions and Consent

I confirm Logos may contact me using the primary contact information provided above for follow-ups and next steps.

I consent to Logos using information from this proposal publicly such as blogs case studies social posts or analytical reporting. Redactions can be requested at any time.

# Program Requirements

We understand this project must be open-sourced under the MIT and Apache 2.0 Licenses unless explicitly approved otherwise.

We are prepared to deliver milestone-based outcomes.
