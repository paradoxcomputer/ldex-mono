# Psychopomp — build & run

Phase-0 + Phase-1 (off-chain). The protocol surface is real (attestation
handshake → ECDH → measurement-bound AEAD → encrypted-witness → STARK +
binding); the TEE itself is stubbed by an ed25519 root key + sha256-of-binary
"MRENCLAVE" so the artifact runs on commodity GPUs while a future swap to
NVIDIA NRAS / AMD SEV-SNP attestors changes only the `psychopomp-attest`
impl, not the wire.

## Crate layout

| Crate / path | Role |
|---|---|
| `crates/psychopomp-types` | Wire schema (AttestationDoc, WitnessPayload, JobRequest, JobResult, JobBinding, GuestElfRef) |
| `crates/psychopomp-attest` | `Attestor` / `Verifier_` traits + Phase-0 `StubAttestor`/`StubVerifier` |
| `crates/psychopomp-client` | Wallet SDK: `prove(...)`, `prove_multi(...)`, `ensure_elf_cached(...)` |
| `crates/psychopomp-prover` | Operator daemon: axum HTTP server + RISC0 (CUDA via `--features gpu`) |
| `crates/psychopomp-e2e` | End-to-end harness: `--test {hello|cached|composed|multi}` |
| `guests/hello` + `guests/hello-methods` | Tiny test guest: `(a, b) -> (a+b, a*b)` |
| `guests/composed` + `guests/composed-methods` | Tests assumption pass-through (`env::verify(HELLO_ID, ...)`) |
| `Phase1-onchain/psychopomp-registry-core` | Pure-Rust state machine for the LEZ registry program (operator pubkey, MRENCLAVE list, stake, reputation, unbond/withdraw). Unit-tested. The LEZ guest wrapper is the deployment-gated Phase-1 step. |
| `Phase1-onchain/psychopomp-escrow-core` | Pure-Rust state machine for the LEZ escrow program (Post / Accept / Settle / Fault, balance-delta DSL). Unit-tested. |
| `scripts/e2e-all.sh` | Dual-prover dev-mode run of all four protocol modes + metrics |
| `scripts/e2e-localhost.sh` | Single-prover real-STARK or dev-mode run |
| `scripts/test-persistence.sh` | Kill+restart prover, verify Done status survives |
| `scripts/runpod-bootstrap.sh` | One-shot install for a fresh CUDA pod |
| `scripts/run-remote-e2e.sh` | Laptop-side run against a deployed prover |

## Protocol surface (HTTP/JSON wrapping borsh-encoded payloads)

```
GET  /v0/health                  liveness
GET  /v0/metrics                 prometheus text-format counters
GET  /v0/attestation             { AttestationDoc }
GET  /v0/attestation/roots       { roots: [TrustedRoot] }
HEAD /v0/elf/{image_id_hex}      200 if cached, 404 otherwise
POST /v0/elf/{image_id_hex}      upload ELF bytes; server recomputes IMAGE_ID, rejects mismatch
POST /v0/jobs                    JobRequest -> { job_id, accepted_at }
GET  /v0/jobs/{job_id}           JobStatus { pending | running | done | failed }
```

## Build & run on this laptop

```bash
cargo build --workspace --release
cargo test --workspace                          # 20 unit tests across 6 crates
RISC0_DEV_MODE=1 ./scripts/e2e-all.sh           # all 4 protocol modes (~2s of proving)
RISC0_DEV_MODE=0 ./scripts/e2e-localhost.sh --release   # real STARK (~11s on this CPU)
./scripts/test-persistence.sh                   # kill prover, restart, Done jobs survive
```

On the first build, `risc0-circuit-recursion`'s `build.rs` downloads its
recursion zkr from S3. If your network mangles that download (see
[Build notes → Recursion zkr](#recursion-zkr)), drop a verified copy at
`scripts/recursion_zkr.zip` and the scripts will pick it up automatically.

The `e2e-all.sh` script:
- launches two prover instances on :8088 and :8089 with separate `--state-dir`
- runs `--test hello` (inline ELF), `--test cached` (upload then `GuestElfRef::Cached`), `--test composed` (assumption pass-through), `--test multi` (`prove_multi` across both ports)
- dumps `/v0/metrics` from each prover at the end

## Deploy on a rented GPU box (RunPod / Vast / Lambda)

Tested target: a pod backed by `nvidia/cuda:12.4.1-devel-ubuntu22.04` (or any
image with `nvcc` and `nvidia-smi`).

```bash
# 1. Bundle:
./scripts/bundle-for-runpod.sh                  # → ../psychopomp.tar.gz (~50 MB)

# 2. Upload to the pod, then on the pod (as root):
mkdir -p /opt && cd /opt
REPO_TARBALL=$PWD/psychopomp.tar.gz bash psychopomp/scripts/runpod-bootstrap.sh

# 3. Start the prover:
cd /opt/psychopomp && \
  RISC0_DEV_MODE=0 \
  RUST_LOG=info \
  ./target/release/psychopomp-prover \
      --bind 0.0.0.0:8088 \
      --state-dir /var/lib/psychopomp
```

Expose port 8088 via the pod's TCP proxy; from the laptop:

```bash
ENDPOINT=https://<pod-id>-8088.proxy.runpod.net ./scripts/run-remote-e2e.sh
```

Same `PASS` banner with a `wall_clock` materially under the laptop CPU
baseline.

### Operator-side tuning

- `--max-concurrent N` — number of in-flight proof jobs (default 2).
- `--policy /path/to/policy.toml` — restrict accepted IMAGE_IDs and limits:
  ```toml
  allowed_image_ids = [
      "fbfe622b...",   # e.g. LDEX amm_v2 program id, once you outsource its proofs
  ]
  max_session_limit = 8388608
  max_inline_elf_bytes = 262144     # forces big ELFs through POST /v0/elf
  max_witness_ct_bytes = 4194304
  ```
- `--attestation-valid-secs 300` — fresh attestation doc window.

### Wallet-side ergonomics (client SDK)

```rust
let cfg = psychopomp_client::ClientConfig::local(
    "https://<pod>-8088.proxy.runpod.net".into(),
    expected_mrenclave,            // pinned in wallet config
    trusted_root,                  // ditto
);

// First call for a guest: upload the ELF once.
psychopomp_client::ensure_elf_cached(&cfg, &MY_IMAGE_ID, MY_ELF).await?;

// Subsequent calls: skip the ELF in every request.
let receipt = psychopomp_client::prove(
    &cfg,
    witness_payload,
    psychopomp_types::GuestElfRef::Cached,
    MY_IMAGE_ID,
).await?;

// Multi-route for censorship resistance:
let receipt = psychopomp_client::prove_multi(
    &[cfg_a, cfg_b, cfg_c],
    witness_payload,
    psychopomp_types::GuestElfRef::Cached,
    MY_IMAGE_ID,
).await?;
```

## Feature matrix (Phase-0 + Phase-1 off-chain)

| Feature | Status | Where |
|---|---|---|
| Attestation handshake (stub TEE) | ✓ | `psychopomp-attest`, all e2e tests |
| ECDH + AEAD over witness | ✓ | `psychopomp-client::prove`, `psychopomp-prover::server::run_job` |
| RISC0 prove via `default_prover()` | ✓ | tested real-STARK 11.3s for hello on CPU |
| GPU build | ✓ | `cargo build --features gpu -p psychopomp-prover` |
| AssumptionReceipt pass-through | ✓ | `--test composed` (outer guest `env::verify`s inner) |
| ELF cache (`GuestElfRef::Cached`) | ✓ | `--test cached`; `HEAD/POST /v0/elf/<hex>` |
| Multi-route (`prove_multi`) | ✓ | `--test multi` against two prover instances |
| Persistent job table | ✓ | `test-persistence.sh` kill+restart |
| Operator metrics | ✓ | `/v0/metrics` (prometheus text) |
| Operator policy (allowlist + limits) | ✓ | `--policy` TOML |
| Time-lock encryption | ✓ | `--test timelock`; iterated SHA-256 puzzle in `TimelockPuzzle` |
| Commit-reveal cipher delivery | ✓ | `--test commit-reveal`; `POST /v0/jobs/precommit` then `/{id}/ciphertext` |
| Operator discovery (file source) | ✓ | `--test discover --registry registry.json`; `psychopomp_client::discovery` |
| Reputation-weighted routing | ✓ | `--test ranked`; `psychopomp_client::reputation::ReputationLedger` |
| Diverse-attestation co-proving | ✓ | `--test diverse`; `prove_diverse(..., min_diverse=k)` |
| TLS (rustls + self-signed dev cert) | ✓ | `--tls-dev` or `--tls-cert/--tls-key`; verified by curl over https |
| ELF upload bearer-token auth | ✓ | `upload_bearer_tokens` in policy; verified 401→201 in e2e-all |
| Registry state machine | ✓ | `psychopomp-registry-core` + 5 unit tests |
| Escrow state machine | ✓ | `psychopomp-escrow-core` + 4 unit tests |
| Registry/escrow program (account bridge) | ✓ | `psychopomp-{registry,escrow}-program` + tests; wraps state machines in nssa AccountWithMetadata I/O |
| Registry/escrow LEZ guest source | ✓ | `psychopomp-{registry,escrow}/methods/guest/`: `#[lez_program]` + `#[instruction]` |
| Registry/escrow guest ELF compilation | ⏸ | needs `docker buildx` for the upstream `cargo risczero build`; pure-Rust state machine + bridge crates are fully tested instead |
| Real NRAS / SEV-SNP attestor | ⏸ | gated on H100 CC or MI300 hardware (Phase-1) |
| MPC threshold proving | ⏸ | Phase-4 (README §Roadmap) |

## Build notes

### Recursion zkr

`risc0-circuit-recursion-4.0.4`'s `build.rs` downloads
`https://risc0-artifacts.s3.us-west-2.amazonaws.com/zkr/744b999f….zip` and
verifies the SHA. On most networks this Just Works.

On some networks the download is mangled (`Verification: FAILED with status
200`). Workaround: download the zkr archive yourself (verify the SHA against
the `build.rs` constant), drop it at `psychopomp/scripts/recursion_zkr.zip`,
and the e2e scripts auto-export `RECURSION_SRC_PATH` pointing at it. The
file is `.gitignore`d so a local vendored copy never lands in the public
repo.

### `cargo` features

- Default workspace build: NO CUDA, NO `prove` feature in `psychopomp-client`
  — keeps wallet integrators CUDA-free.
- `cargo build --features gpu -p psychopomp-prover`: enables
  `risc0-zkvm/cuda` for GPU-accelerated proving.

### Path-param quirk

`axum 0.7` uses `:job_id` syntax, not `{job_id}` (which is `axum 0.8`).

### `ExecutorEnv` is not Serializable

The wire ships `WitnessPayload { stdin, stdin_frames, assumptions, ... }` and
the prover reconstructs the env. `assumptions[i] = borsh::to_vec(&Receipt)`;
the server `From<Receipt> for AssumptionReceipt`s it back.

For the LDEX privacy circuit:
```rust
payload.stdin = bytemuck::cast_slice(
    &risc0_zkvm::serde::to_vec(&PrivacyPreservingCircuitInput)
);
for inner in chained_inner_receipts {
    payload.assumptions.push(borsh::to_vec(&inner)?);
}
```

## Phase 1 — on-chain registry/escrow (optional)

Phase-0 doesn't need a chain. Skip this section unless you specifically want
to exercise the on-chain registry/escrow programs on a local LEZ sequencer.

Requires a checkout of the Logos Execution Zone (LEZ) that builds the
`sequencer_service` and `wallet` binaries:

```bash
git clone https://github.com/logos-blockchain/logos-execution-zone.git ~/lez
cd ~/lez
cargo build --release -p sequencer_service --features standalone
cargo build --release -p wallet
cargo build --release -p spel
```

Then, from the psychopomp checkout (with `LEZ_HOME` pointed at your LEZ
checkout):

```bash
# 1. Build the LEZ guest ELFs (needs docker buildx for cargo risczero build)
./scripts/build-lez-guests.sh

# 2. Run the sequencer (background, leave running)
LEZ_HOME=~/lez ./scripts/run-psychopomp-sequencer.sh &

# 3. Initialize a wallet + deploy the programs
LEZ_HOME=~/lez ./scripts/deploy-onchain.sh

# 4. Live e2e
cargo run --release -p psychopomp-chain --example verify-deployment
cargo run --release -p psychopomp-chain --example live-register
cargo run --release -p psychopomp-chain --example live-lifecycle
cargo run --release -p psychopomp-chain --example live-fault
```

Wallet/sequencer paths can be overridden via env vars:
`PSYCHOPOMP_WALLET_CFG`, `PSYCHOPOMP_WALLET_STORAGE`. See
[PROGRESS.md](PROGRESS.md) for the current state of the chained-call work.

## Limitations to be honest about

- **No real TEE.** Phase-0 uses `StubAttestor`. A malicious operator can read
  the witness. Swap to a Phase-1 attestor before any privacy-critical use.
- **No on-chain registry/escrow yet.** State machine logic is implemented and
  tested (`Phase1-onchain/*-core/`), but the LEZ guest binaries that plumb
  them through nssa account I/O require the SPEL toolchain + a sequencer to
  deploy. Phase-1 deliverable.
- **No real NRAS / SEV-SNP attestor yet.** The `Verifier_` trait is sized for
  that swap; today's `StubVerifier` accepts ed25519-signed DER-shaped
  "vendor chains" so the wire layout doesn't change.
- **No commit-reveal yet** (README §"MEV / front-running"). Operators can see
  the ciphertext before they've publicly committed to running the job. The
  AEAD AAD still binds the witness to a specific operator's MRENCLAVE, so a
  peek-then-re-bid attack would still need that operator's enclave key — but
  a fully malicious operator could leak the request even if they can't decrypt.

## LDEX integration

The only remaining hooks before LDEX can outsource via psychopomp:

1. In `ldex/ffi/ldex-amm-ffi` (the wallet path), replace
   `risc0_zkvm::default_prover().prove(env, &elf)?` with
   `psychopomp_client::prove(&cfg, payload, GuestElfRef::Cached, image_id).await?`.
2. `psychopomp_client::ensure_elf_cached` should be called once at wallet
   startup (or lazy on first prove) per pinned image_id.
3. The `payload.assumptions` field is wired to take borsh-encoded inner
   receipts from each chained call — psychopomp's prover passes them
   through to `ExecutorEnv::add_assumption`.

Per the standing rule, this repo does not touch the LDEX tree.
