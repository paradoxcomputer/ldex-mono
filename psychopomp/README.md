# Psychopomp

> **Phase-0 PoC is implemented and tested.** See [BUILD.md](BUILD.md) for the
> full runbook (laptop e2e + RunPod / GPU deployment), and
> [CONTRIBUTING.md](CONTRIBUTING.md) for the test layout.

**Privacy-preserving outsourced proving for the Logos Execution Zone.**

## Quick start

```bash
git clone https://github.com/<your-org>/psychopomp.git
cd psychopomp
./scripts/bootstrap-dev.sh        # installs Rust + RISC Zero toolchain, builds, tests
```

The bootstrap script is idempotent - re-running on a working setup is just a
build + test pass. It deliberately skips the Phase-1 on-chain LEZ guest ELFs
(those need `docker buildx` and a separate LEZ checkout - see [BUILD.md](BUILD.md)).

What you have after bootstrap:
- `target/release/psychopomp-prover` - the operator daemon (axum HTTP + RISC0).
- `target/release/psychopomp-e2e` - the client-side end-to-end harness.
- 41 passing unit tests across 10 Phase-0 + state-machine crates.
- A dev-mode e2e pass against a localhost prover.

One-liner localhost real-STARK run:

```bash
./scripts/e2e-localhost.sh --release            # ~10s on a Ryzen-class CPU
```

GPU box (RunPod / Vast / Lambda):

```bash
./scripts/bundle-for-runpod.sh                  # secrets-safe by construction
# upload psychopomp.tar.gz to the pod, then on the pod:
mkdir -p /opt && cd /opt
REPO_TARBALL=$PWD/psychopomp.tar.gz bash psychopomp/scripts/runpod-bootstrap.sh
# ... start the prover, then from the laptop:
ENDPOINT=https://<pod>-8088.proxy.runpod.net ./scripts/run-remote-e2e.sh \
    --test heavy --rounds 100000
```

See [BUILD.md](BUILD.md) for the full GPU recipe.

---



A decentralised marketplace of TEE-attested GPU provers that generate zk-STARK receipts on a user's behalf *without ever exposing the unsealed witness*. The on-chain artifact is the standard RISC Zero STARK - verifiable by any LEZ node with no trust in the prover. The off-chain confidentiality property is enforced by hardware attestation. The economic layer (registration, escrow, slashing, rewards) lives as a small set of LEZ programs.

The first client is [LDEX](../ldex), whose private-swap path needs ~10-21 min of CPU STARK generation per swap on a Ryzen-class laptop. Psychopomp targets ~1-2 min wall-clock on an H100-class GPU enclave while preserving the privacy property that the chain otherwise guarantees.

---

## Why not Boundless / Bonsai alone

Vanilla outsourced proving (e.g. the RISC Zero Bonsai network, the Boundless prover marketplace) is **privacy-breaking for LDEX**. The privacy circuit's witness contains PrivateOwned balances, nullifier secrets, the unshielded swap amounts, and the wallet's viewing keys. The on-chain receipt reveals none of that - but to *generate* the receipt, you have to hand the prover the raw witness. A third-party prover with no enclave sees everything the chain hides. The privacy property collapses to "trust the prover not to log, leak, or be subpoenaed" - a custodial model dressed as zk.

Psychopomp's bet: outsourced proving for a privacy chain only makes sense if the prover runs inside a TEE-attested enclave. That's the gap.

## Why not TEE alone

A TEE-only prover (no zk on the output) would mean the LEZ verifier trusts the hardware attestation directly. That collapses to "trust the hardware vendor + firmware" - a worse trust model than zk on every dimension that matters at chain layer (correctness, censorship resistance, decentralisation).

**Psychopomp uses both. Each layer protects a different thing:**

| Layer | Protects | Failure mode if absent |
|---|---|---|
| zk-STARK (on-chain) | Computation integrity | TEE compromise → chain integrity break |
| TEE attestation (off-chain) | Witness confidentiality | Off-chain witness leakage → privacy break |

A TEE compromise in the Psychopomp design leaks one user's secrets but cannot corrupt the chain. A zk-only outsourcing design exposes every outsourced user's secrets to the prover.

---

## Architecture

```
                ┌─────────────────┐                ┌──────────────────────────┐
                │  Client wallet  │                │  Psychopomp operator     │
                │  (LDEX, etc)    │                │  (TEE-attested GPU)      │
                └────────┬────────┘                ├──────────────────────────┤
                         │                         │                          │
                         │ 1) Attestation handshake│                          │
                         ◄────────────────────────►│  Enclave publishes:      │
                         │  (verify MRENCLAVE +    │   - ephemeral pubkey     │
                         │   attestation report)   │   - MRENCLAVE measurement│
                         │                         │   - HW attestation chain │
                         │                         │                          │
                         │ 2) Encrypt(witness, pk) │                          │
                         │    → ciphertext         │                          │
                         │                         │                          │
                         │ 3) Job post (on-chain): │                          │
                         │    ciphertext_hash,     │                          │
                         │    measurement filter,  │                          │
                         │    deadline, max-bid,   │                          │
                         │    escrow LEZ           │                          │
                         │                         │                          │
                         │                         │ 4) Operator picks job:   │
                         │                         │    - decrypt inside TEE  │
                         │                         │    - run RISC0 STARK gen │
                         │                         │      on CUDA-accelerated │
                         │                         │      enclave             │
                         │                         │    - emit STARK + report │
                         │                         │                          │
                         │ 5) Operator submits:    │                          │
                         ◄────────────────────────►│  {STARK receipt,         │
                         │  verify attestation +   │   attestation document}  │
                         │  STARK locally          │                          │
                         │                         │                          │
                         │ 6) Submit STARK to LEZ  │                          │
                         │  (standard receipt -    │                          │
                         │   chain doesn't know it │                          │
                         │   was outsourced)       │                          │
                         │                         │                          │
                         │ 7) On-chain settlement: │                          │
                         │    verifier checks      │                          │
                         │    STARK → escrow       │                          │
                         │    releases to operator │                          │
                         └─────────────────────────┘                          │
                                                                              │
                                                                              ▼
                                                                  Bond preserved
                                                                  or slashed on
                                                                  liveness /
                                                                  correctness fault
```

Three properties of the composition:

- **RISC0 is unchanged.** The prover binary inside the enclave is the stock RISC0 CUDA prover. Same binary, same STARK output.
- **The TEE is opaque to RISC0.** The enclave runs the measured binary; it doesn't know what it's proving.
- **The chain is opaque to both.** The LEZ verifier checks the STARK receipt as if it were locally proved. No protocol change at the chain layer.

---

## Components

### `psychopomp-prover` - enclave-side binary

The deterministically reproducible binary that runs inside the TEE-attested enclave. Open source. The compiled measurement (`MRENCLAVE`) is published on-chain in the operator registry and re-derivable by anyone from the source.

Responsibilities:
- Bootstrap attestation: generate an ephemeral keypair inside the enclave, bind it to the hardware attestation report.
- Decrypt the inbound witness ciphertext inside the enclave (witness plaintext never leaves the sealed memory region).
- Drive the RISC0 CUDA prover.
- Emit `{STARK receipt, attestation document}` and tear down the ephemeral key.

### `psychopomp-client` - wallet-side SDK

A small Rust library the wallet uses to outsource a proof instead of running it locally.

Responsibilities:
- Discover operators from the on-chain registry, filter by measurement whitelist + hardware class + price + reputation.
- Perform the attestation handshake (verify the report's signature chain to the hardware vendor's root).
- Encrypt the witness to the operator's ephemeral pubkey.
- Submit the job on-chain (or via a public off-chain mempool, settling on-chain).
- Verify the returned `{STARK, attestation}` bundle.
- Hand the verified STARK to the chain submission path.

The integration surface in LDEX is one function call: replace the local `risc0_zkvm::Prover::prove(...)` with `psychopomp_client::prove(...)`. Everything else (witness construction, on-chain submission, balance tracking) is unchanged.

### `psychopomp-registry` - on-chain LEZ program

State per operator:
- Operator pubkey (signing key for registry updates).
- Attestation pubkey root (the long-term key that signs ephemeral per-job keys).
- Allowed `MRENCLAVE` measurements (one per supported binary version).
- Hardware class enum (`H100CC | MI300SEV | TDX | ...`).
- Stake bond (in LEZ).
- Reputation counters (success / fault counts; updated by settlement program).
- Active status + unbonding timer.

Instructions:
- `Register { attestation_root, measurements, hw_class }` - stake-gated.
- `UpdateMeasurements { measurements }` - re-stake + cooldown.
- `Unbond` - opens a 2-week timer before stake withdraws.
- `Withdraw` - only after unbonding completes.

### `psychopomp-escrow` - on-chain LEZ program

State per job:
- Client pubkey, ciphertext hash, measurement filter, hw-class filter.
- Max-bid amount (LEZ), escrow balance.
- Deadline (block height).
- Status: `Open | Awarded(operator) | Settled | Refunded`.

Instructions:
- `Post { ciphertext_hash, filter, max_bid, deadline }` - locks `max_bid` LEZ.
- `Accept { job_id }` - operator commits to deliver. Locks an additional per-job stake from the operator's bond.
- `Settle { job_id, stark, attestation }` - verifies both, releases escrow to operator, returns per-job stake to operator's bond.
- `Fault { job_id }` - callable by anyone after the deadline OR on attestation/STARK rejection; refunds escrow to client, slashes operator's per-job stake.

---

## Hardware reality

| Class | Maturity | Confidential compute model | Notes |
|---|---|---|---|
| **NVIDIA H100 CC** | Mature (2024+) | Hopper Confidential Compute - entire GPU + connected CPU memory sealed; remote attestation via NVIDIA NRAS | Available on Azure, GCP, OCI. ~5-10% throughput overhead vs bare-metal. The default target. |
| **AMD MI300 SEV-SNP** | Mid-maturity | SEV-SNP CPU + Instinct GPU passthrough | Cheaper per FLOP than H100. Smaller cloud footprint. |
| **Intel TDX + GPU passthrough** | Early (2025+) | TDX trust domain with PCIe-attached GPU | Most flexible (any GPU), least proven attestation chain. |

The protocol is hardware-agnostic at the registry level - operators declare their class and `MRENCLAVE`, clients filter by class.

Diversifying across vendors hedges side-channel risk: a compromise of NVIDIA's attestation chain doesn't compromise MI300 or TDX operators. High-value jobs can require *diverse attestation* (the same job co-proved by operators on two different hardware classes; STARKs aggregated client-side).

---

## Decentralisation + economics

### Job market

Operators are first-come, first-served on a public job mempool (on-chain or a fast off-chain queue with on-chain settlement). The first valid `{STARK, attestation}` response within the deadline wins. Clients set a max-bid; competitive operators drive the price down.

Open membership: anyone can stake + register, no permissioning. Reputation is per-pubkey and grows from successful settlements; clients route by latency + success rate + price.

### Rewards

- **Per-proof fee** - the primary revenue stream. Paid by clients in LEZ from the escrow.
- **Optional treasury subsidy** - early supply-side bootstrapping; a portion of LDEX trading fees (or a separate Psychopomp treasury) underwrites operator rewards to grow the supply side until organic demand sustains it.
- **Slashed-bond redistribution** - slashed stakes are partly burnt, partly redistributed pro-rata to honest operators in the same epoch (so honest operators benefit from policing).

### Stake-to-revenue ratio

The lever against bribery and Sybil-flavored gaming. Per-operator stake `S ≥ K · R_epoch`, where `R_epoch` is the operator's expected per-epoch revenue and `K` is a network-governed multiplier (initial proposal: `K = 100`). A bribe to misbehave has to exceed `100×` an hour's earnings for the bribe to be profitable.

Per-job stake adds an additional lock: operators commit `J = α · job_escrow` (initial proposal: `α = 10`) for the job duration. Slashed on fault. Per-job stake means high-value jobs require operators to risk more bond, scaling the deterrent automatically.

### Unbonding cooldown

Stake withdrawal requires a 2-week unbonding timer. Late-discovered faults (e.g. an off-chain attestation forgery proof that surfaces a day after the job) can still slash an operator who tried to exit.

---

## Threat model + defences

Mapped attack class → mitigation. The honest scoreboard: liveness/correctness/censorship/sybil/MEV all have objective on-chain defences; confidentiality is cryptographic + economic with no on-chain proof-of-leak.

### Liveness - operator accepts a job, doesn't deliver

- Tight per-job deadline (default 1-2 blocks; client-settable).
- After deadline, any keeper submits a fault tx → escrow refunds to client → operator's per-job stake slashed.
- Reputation decay on missed deadlines; bad operators get routed around.

### Correctness - operator returns invalid STARK or bad attestation

- Verifier program (in `psychopomp-escrow`) re-checks both:
  - STARK verifies under upstream `PRIVACY_PRESERVING_CIRCUIT_ID` (or the relevant program's `IMAGE_ID`).
  - Attestation chains to a chain-governed hardware-vendor root key.
- Failure → fault path. Same slash as liveness.

### Confidentiality - operator extracts the witness

The hard one. Layered defences, no single layer sufficient:

- **Measurement-bound encryption.** Client encrypts to a *fresh ephemeral key derived inside the enclave* and *bound to the published `MRENCLAVE`*. Operator's long-term key never sees plaintext. If the operator runs a modified binary, attestation rolls up to a different measurement and the client refuses to send.
- **Open prover binary + governance-controlled measurement whitelist.** Anyone can rebuild Psychopomp and verify the published measurement matches the source. Updates require a governance proposal; no unilateral operator key rotation.
- **Hardware diversity.** Network supports multiple TEE backends. A vendor-specific compromise (or class-specific side-channel) doesn't compromise the whole network.
- **Optional MPC threshold proving.** For ultra-sensitive jobs: split the witness into `M` shares via Shamir or similar; route to `N ≥ t` independent operators on diverse hardware; threshold proof reconstruction inside enclaves. Cryptographic confidentiality on top of TEE - even if one operator's hardware is broken, their share is meaningless without `t-1` others colluding.

What you cannot do: prove leakage on-chain after the fact. The protocol's job is to make leakage *expensive and rare*, not *impossible*. Accept that the residual risk reduces to "one user's secrets leak if one operator's hardware is broken" - and design every protocol decision so a breach affects one user, never chain integrity, never many users.

### MEV / front-running based on witness contents

A malicious operator who could see the witness contents could attempt to extract value (e.g., front-run a private swap once they see its intent).

- **Commit-reveal cipher delivery.** Client posts `H(ciphertext)` on-chain when the job is awarded; the actual ciphertext is delivered to the winning operator over an attested channel only after the on-chain award. Operator can't peek-then-re-bid.
- **Tight deadlines.** A 1-2 block window means delaying = forfeiting.
- **Optional time-locked encryption** for the highest-sensitivity jobs: ciphertext is decryptable only after a future block, regardless of who has it. Lifts the constraint that "operator gets cipher → operator can decrypt immediately."

### Censorship

A coordinated subset of operators refuses certain users / jobs.

- **Multi-route by default.** Client wallet submits to `k` operators in parallel; first valid response wins.
- **Open membership.** Censoring coalition has to outbid the entire honest market.
- **Public job mempool** → systemic censorship is observable → reputation hit → routing weights fall.

### Sybil - single entity registers many "operators"

- **Stake gating.** Each operator-pubkey requires a meaningful LEZ bond.
- **Per-pubkey reputation.** New sybils start at zero rep, have to earn their way up against established operators.
- **Routing favours diversity.** Client wallets can be configured to require routes that don't co-locate on the same operator group (defined by self-declared organisation or by network-inferred clustering).

### Bribery / collusion

- **Stake-to-revenue ratio** as the primary lever (see Economics).
- **Per-job stake scaling** so high-value jobs are economically expensive to attack.
- **Unbonding cooldown** so late-detected misbehaviour catches operators who tried to exit before evidence surfaced.

### Operator-side malice patterns we explicitly accept

- **One-off witness leak via TEE compromise:** unenforceable on-chain; mitigated by hardware diversity + MPC threshold for sensitive jobs.
- **Operator runs slower hardware than declared:** acceptable as long as deadline is met; market sorts this via reputation.
- **Operator goes offline:** liveness slash on unmet deadlines; no further action needed.

---

## LDEX integration

LDEX's wallet currently calls a local RISC Zero prover via `wallet::send_privacy_preserving_tx_with_pre_check`, which constructs the witness and drives the prover. The integration is a one-function swap at that call site:

```rust
// before
let receipt = risc0_zkvm::default_prover().prove(env, &program.elf)?;

// after (optional path, selected via env or config)
let receipt = match psychopomp::Mode::from_env() {
    psychopomp::Mode::Local => {
        risc0_zkvm::default_prover().prove(env, &program.elf)?
    }
    psychopomp::Mode::Remote { endpoint } => {
        psychopomp_client::prove(endpoint, env, &program.elf, &program.id).await?
    }
};
```

`psychopomp_client::prove` handles the attestation handshake, witness encryption, job posting, and receipt verification. Returns the same `Receipt` type the local prover returns. The on-chain submission path doesn't change.

The witness boundary is the `env` argument (RISC0's `ExecutorEnv`), which is constructed wallet-side from plaintext secrets. The client encrypts the *serialised* `env` to the operator's measurement-bound key.

---

## Roadmap

### Phase 0 - proof of concept (1 prover, 1 cloud)

- `psychopomp-prover` binary running inside H100 CC on Azure or GCP.
- Stub registry + escrow as off-chain HTTP for fast iteration; on-chain settlement later.
- LDEX wallet wired to optionally outsource one swap → measure wall-clock vs local.
- **Success criteria:** mode-2 LDEX swap < 2 min wall-clock (vs 10-14 min CPU baseline), attestation verifies client-side, balance deltas match.

### Phase 1 - single-operator on-chain

- `psychopomp-registry` + `psychopomp-escrow` as LEZ programs on dev.
- One operator (us) on H100 CC; one or two clients (LDEX testnet wallets).
- Slashing path live: simulate a missed deadline, simulate an invalid STARK, verify on-chain fault handling works.

### Phase 2 - multi-operator + diverse hardware

- Add MI300 SEV-SNP operator; demonstrate diverse-attestation co-proving on a high-value job.
- Reputation mechanics live; client SDK does routing.
- Open operator registration on dev.

### Phase 3 - economic equilibrium + governance

- Treasury subsidy schedule defined; LEZ-denominated rewards live.
- Governance program for `MRENCLAVE` whitelist updates + parameter tuning (`K`, `α`, deadline floor, unbonding period).
- Public testnet open to third-party operators.

### Phase 4 - optional MPC threshold

- Witness sharing scheme + threshold RISC0 proving spec.
- Targeted at single high-value flows (large privacy swaps, institutional users) - not the common path.

### Phase 5 - mainnet + multi-app

- Mainnet deployment on Logos.
- Open the SDK to other privacy apps on LEZ.
- Long-tail features: encrypted mempool, time-locked encryption modes, custom client policy hooks.

---

## Open questions

These are real and unresolved; the design assumes plausible answers but doesn't fix them yet.

- **Attestation chain freshness:** how often does the client re-attest the operator's enclave? Per job? Per session? Per epoch? Tradeoff between RTT and detection latency for a compromised enclave.
- **Hardware-vendor root key rotation:** what happens when NVIDIA / AMD / Intel rotate their attestation roots? Chain governance must absorb the update; we need a fast-track path for vendor-driven rotations vs the normal proposal cadence.
- **MPC threshold proving feasibility:** the cryptography is published (folded into existing MPC-on-RISC0 work) but the practical overhead may make it a niche path rather than a default. Phase 4 will quantify.
- **Sybil clustering inference:** routing-by-diversity needs a way to identify operators that are nominally distinct but actually co-located. Network-level clustering (BGP, ASN, datacenter) is a starting heuristic; better signals may emerge.
- **Reward bootstrapping:** the chicken-and-egg between operator supply (needs paying jobs) and client demand (needs reliable supply). Phase 0-1 self-bootstraps with LDEX as the only client; the right subsidy curve to escape that into a real market is an open economic question.

---

## Related

- [LDEX](../ldex) - the first client. The full LDEX architecture + privacy-circuit description lives there; Psychopomp is the prover-offload layer for it.
- [RISC Zero zkVM](https://risczero.com/) - the proving system whose STARKs Psychopomp outsources.
- [Logos Execution Zone](https://github.com/logos-co) - the chain Psychopomp settles on.
- Boundless / Bonsai - the comparable proving marketplaces that Psychopomp differs from by inserting the TEE attestation layer.
- NVIDIA Hopper Confidential Compute - the primary hardware target.
