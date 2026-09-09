# Vault (Q3 2026)

An Anchor program on Solana where each user has their own program-derived SOL vault: deposit, withdraw, and close, all gated behind a per-user PDA.

Program ID: `8g7DZmoXgp5xroHbor5Z7vYc5mQ4A1n13M5RSvpHVeNT`

## Instructions

| Instruction | Signer | What happens |
|---|---|---|
| `initialize` | user | Creates the user's `VaultState` account and funds the `vault` PDA with exactly enough lamports to be rent-exempt. |
| `deposit` | user | Transfers `amount` lamports from the user into their `vault`. Rejects `amount == 0`. |
| `withdraw` | user | Transfers `amount` lamports from the `vault` back to the user, signed by the vault PDA. Rejects `amount == 0`. |
| `close` | user | Sweeps every remaining lamport out of the `vault` back to the user, then closes `vault_state` (rent → user). |

## Architecture

- `vault_state` — a PDA seeded `["state", user]` — stores the two bumps (`vault_bump`, `state_bump`) needed to re-derive and sign for the `vault` PDA later.
- `vault` — a PDA seeded `["vault", user]`, holding SOL directly (it's a plain `SystemAccount`, not a token account). It only ever holds lamports; there's no separate token mint involved.
- Moving lamports *out* of `vault` (`withdraw`, `close`) requires the vault PDA itself to sign the `system_program::transfer` CPI via `CpiContext::new_with_signer`, using `["vault", user, vault_bump]` as the signer seeds. Moving lamports *in* (`initialize`, `deposit`) is a plain signer transfer from the user — no PDA signature needed.

```
programs/q3_26_vault/src/
├── lib.rs                    # #[program] entrypoints
├── state.rs                  # VaultState: vault_bump, state_bump
├── constants.rs              # VAULT_SEED, STATE
├── error.rs                  # InvalidAmount
└── instructions/
    ├── initialize.rs         # create vault_state + fund vault to rent-exemption
    ├── deposit.rs            # user -> vault
    ├── withdraw.rs           # vault -> user (PDA-signed)
    └── close.rs              # drain vault -> user, close vault_state
```

## Testing

`programs/q3_26_vault/tests/test_initialize.rs` runs the full lifecycle against [LiteSVM](https://github.com/LiteSVM/litesvm):

1. `initialize` — asserts `vault_state` exists and `vault` holds exactly the rent-exempt minimum.
2. `deposit` 500,000,000 lamports — asserts the vault balance grows by that amount.
3. `withdraw` 100,000,000 lamports — asserts the vault balance shrinks by that amount.
4. `close` — asserts `vault_state` is gone and `vault` is fully drained back to 0.

```bash
cargo test -p q3_26_vault
```

`vault-proof.png` in this directory is a screenshot of the passing test run.

## Requirements

- Rust toolchain pinned in `rust-toolchain.toml`
- `anchor-lang` (see `programs/q3_26_vault/Cargo.toml`)
