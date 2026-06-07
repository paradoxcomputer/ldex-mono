# Security policy

Psychopomp is the privacy-preserving outsourced-proving layer for LDEX on the
Logos Execution Zone. The whole point of the project is to make a class of
attacks expensive - so if you find one that isn't expensive, we want to know.

## Reporting a vulnerability

Please **do not** open a public GitHub issue for a security report. Instead:

- Email the maintainers at the address listed in the repo's GitHub profile
  (or use GitHub's private "Report a vulnerability" flow under
  *Security* → *Report a vulnerability*).
- Include enough detail to reproduce - minimally a description of the attack,
  the affected component, and a proof-of-concept if you have one.

We will acknowledge within 72 hours and aim to have a fix or mitigation plan
within 14 days for issues that are confirmed exploitable. Disclosure timeline
is by mutual agreement; we default to coordinated disclosure once a fix is
deployed.

## Scope

In scope:

- The Phase-0 prover daemon (`psychopomp-prover`) - wire layer, attestation
  binding, AEAD over witness, ELF cache, policy enforcement.
- The client SDK (`psychopomp-client`) - attestation verification, route
  selection, receipt verification.
- The on-chain registry / escrow state machines and bridge crates
  (`Phase1-onchain/*-core`, `Phase1-onchain/*-program`).

Out of scope, but worth noting in context:

- The current `StubAttestor` is intentionally trust-on-first-use. Phase-0
  documentation calls this out - see [BUILD.md](BUILD.md) > "Limitations".
- Vulnerabilities in upstream RISC Zero, the Logos Execution Zone, or any
  hardware TEE attestation chain belong upstream. Please report to the
  respective project; we will track downstream impact.
- Side-channel attacks on a specific GPU enclave (NRAS / SEV-SNP / TDX) are
  ultimately the hardware vendor's domain. Psychopomp's defence here is
  hardware diversity + measurement-bound encryption, not a single-vendor
  root-of-trust.

## Hardening checklist (operators)

If you're running a `psychopomp-prover` instance, the high-impact things to
get right:

- **Keep secrets off untrusted hosts.** The `bedrock_signing_key` and
  `wallet/storage.json` from a local LEZ sequencer must never leave the
  machine that generated them. `scripts/bundle-for-runpod.sh` enforces this
  by excluding `sequencer-state/` and refusing if any signing-key file is
  found outside the known locations.
- **Use TLS** (`--tls-cert` / `--tls-key`, or `--tls-dev` for pinned
  self-signed dev certs). The wire layer carries AEAD-encrypted witnesses,
  but TLS protects metadata (job IDs, attestation docs).
- **Set a policy file** (`--policy`). Cap `max_witness_ct_bytes`,
  `max_inline_elf_bytes`, set an `allowed_image_ids` allowlist once you know
  which guests you're willing to run, require `upload_bearer_tokens` for
  `POST /v0/elf`.
- **Reproducible binary.** The whole attestation story breaks if your binary
  doesn't match the published `MRENCLAVE`. Build deterministically; pin the
  toolchain.

## Public disclosures

None to date. This file will be updated with a CVE list once we have any.
