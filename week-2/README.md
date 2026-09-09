# Week 2 — Anchor Programs

Two standalone Solana Anchor programs, each with its own test suite run against LiteSVM and a screenshot proving the tests pass. Start with each project's own README for the full write-up — this page is just for jumping straight to a file.

## [`escrow-q3-26-main/`](./escrow-q3-26-main) — Token Escrow

A trustless SPL token swap: a maker deposits token A and asks for token B in return; any taker holding token B can complete the swap atomically, or the maker can refund themselves if no one takes the offer.

**[→ Full README](./escrow-q3-26-main/README.md)**

| | |
|---|---|
| Program entrypoints | [`src/lib.rs`](./escrow-q3-26-main/programs/escrowq32026/src/lib.rs) |
| Escrow account (`Escrow`) | [`src/state.rs`](./escrow-q3-26-main/programs/escrowq32026/src/state.rs) |
| `make` — create escrow + deposit | [`src/instructions/make.rs`](./escrow-q3-26-main/programs/escrowq32026/src/instructions/make.rs) |
| `take` — swap + close | [`src/instructions/take.rs`](./escrow-q3-26-main/programs/escrowq32026/src/instructions/take.rs) |
| `refund` — maker reclaims deposit | [`src/instructions/refund.rs`](./escrow-q3-26-main/programs/escrowq32026/src/instructions/refund.rs) |
| `update` — extend expiration | [`src/instructions/update.rs`](./escrow-q3-26-main/programs/escrowq32026/src/instructions/update.rs) |
| Errors | [`src/error.rs`](./escrow-q3-26-main/programs/escrowq32026/src/error.rs) |
| Tests (3 tests) | [`tests/mod.rs`](./escrow-q3-26-main/programs/escrowq32026/tests/mod.rs) |
| Architecture diagrams | [`arch/make.png`](./escrow-q3-26-main/arch/make.png) · [`arch/take.png`](./escrow-q3-26-main/arch/take.png) · [`arch/refund.png`](./escrow-q3-26-main/arch/refund.png) |
| Passing-test proof | [`escrow-proof.png`](./escrow-q3-26-main/escrow-proof.png) |

```bash
cd escrow-q3-26-main && cargo test -p escrowq32026
```

## [`anchor_vault_starter_q3_26-main/`](./anchor_vault_starter_q3_26-main) — SOL Vault

A per-user PDA vault for native SOL: initialize, deposit, withdraw, and close.

**[→ Full README](./anchor_vault_starter_q3_26-main/README.md)**

| | |
|---|---|
| Program entrypoints | [`src/lib.rs`](./anchor_vault_starter_q3_26-main/programs/q3_26_vault/src/lib.rs) |
| Vault account (`VaultState`) | [`src/state.rs`](./anchor_vault_starter_q3_26-main/programs/q3_26_vault/src/state.rs) |
| `initialize` — create vault | [`src/instructions/initialize.rs`](./anchor_vault_starter_q3_26-main/programs/q3_26_vault/src/instructions/initialize.rs) |
| `deposit` — user → vault | [`src/instructions/deposit.rs`](./anchor_vault_starter_q3_26-main/programs/q3_26_vault/src/instructions/deposit.rs) |
| `withdraw` — vault → user | [`src/instructions/withdraw.rs`](./anchor_vault_starter_q3_26-main/programs/q3_26_vault/src/instructions/withdraw.rs) |
| `close` — drain + close vault | [`src/instructions/close.rs`](./anchor_vault_starter_q3_26-main/programs/q3_26_vault/src/instructions/close.rs) |
| Errors | [`src/error.rs`](./anchor_vault_starter_q3_26-main/programs/q3_26_vault/src/error.rs) |
| Tests (full lifecycle) | [`tests/test_initialize.rs`](./anchor_vault_starter_q3_26-main/programs/q3_26_vault/tests/test_initialize.rs) |
| Passing-test proof | [`vault-proof.png`](./anchor_vault_starter_q3_26-main/vault-proof.png) |

```bash
cd anchor_vault_starter_q3_26-main && cargo test -p q3_26_vault
```

## Notes

Both projects use [LiteSVM](https://github.com/LiteSVM/litesvm), an in-process Solana runtime — no local validator or `solana-test-validator` needed to run either test suite. Each is its own independent Cargo workspace, so `cd` into it before running `cargo test`.
