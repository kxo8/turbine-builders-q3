//! Token-2022 mint management, transfer fees, and issuer controls.
use anchor_lang::{prelude::*, solana_program::program::invoke};

use anchor_spl::token_2022::{spl_token_2022 as token, Token2022};


use spl_token_metadata_interface::state::TokenMetadata;
use spl_type_length_value::variable_len_pack::VariableLenPack;

use token::{
    extension::{
        confidential_transfer::instruction as ct,
        confidential_transfer_fee::instruction as ct_fee,
        default_account_state::instruction::initialize_default_account_state,
        metadata_pointer::instruction::initialize as initialize_metadata_pointer,
        permanent_delegate::PermanentDelegate,
        transfer_fee::{instruction as fees, TransferFeeConfig},
        BaseStateWithExtensions, ExtensionType, StateWithExtensions,
    },
    state::{Account as TokenAccount, AccountState, Mint},
};

declare_id!("ApagpE9wV8WPwbu7nzBUeYeSJwN1AwE7TJhGZ8Dhbqky");
pub const DECIMALS: u8 = 6;
pub const BASIS_POINTS: u16 = 100; // 1%.
pub const MAXIMUM_FEE: u64 = 1_000_000; // One token, in base units.
pub const TOKEN_NAME: &str = "Remittance USD";
pub const TOKEN_SYMBOL: &str = "RUSD";
pub const TOKEN_URI: &str = ""; // Name/symbol are on-chain; no external JSON needed here.
pub const AE_CIPHERTEXT_LEN: usize = 36;

#[program]
pub mod remittance_stablecoin {
    use super::*;

    pub fn initialize(ctx: Context<CreateMint>) -> Result<()> {
        create_mint(&ctx.accounts, None)
    }

    /// A NEW mint address: existing V1 balances are not migrated by this call.
    pub fn reissue_mint(
        ctx: Context<CreateMint>,
        withdraw_withheld_authority_elgamal_pubkey: [u8; 32],
    ) -> Result<()> {
        require!(
            withdraw_withheld_authority_elgamal_pubkey != [0; 32],
            RemittanceError::InvalidFeeKey
        );
        create_mint(
            &ctx.accounts,
            Some(withdraw_withheld_authority_elgamal_pubkey),
        )
    }

    pub fn transfer_with_fee(ctx: Context<TransferTokens>, amount: u64) -> Result<()> {
        checked_fee_transfer(&ctx.accounts, amount, false)
    }

    /// Public funds ONLY. Frozen accounts must first be thawed by the freeze authority.
    pub fn seize_public_tokens(ctx: Context<TransferTokens>, amount: u64) -> Result<()> {
        checked_fee_transfer(&ctx.accounts, amount, true)
    }

    /// KYC happens off-chain; the freeze authority signs its decision here.
    /// Thawing one account does not change the mint's Frozen default.
    pub fn thaw_after_kyc(ctx: Context<AccountAuthority>) -> Result<()> {
        invoke(
            &token::instruction::thaw_account(
                &token::ID,
                &ctx.accounts.token_account.key(),
                &ctx.accounts.mint.key(),
                &ctx.accounts.authority.key(),
                &[],
            )?,
            &ctx.accounts.infos(),
        )?;
        Ok(())
    }

    /// Manual CT approval is a separate gate from KYC thaw and owner configuration.
    pub fn approve_confidential_account(ctx: Context<AccountAuthority>) -> Result<()> {
        invoke(
            &ct::approve_account(
                &token::ID,
                &ctx.accounts.token_account.key(),
                &ctx.accounts.mint.key(),
                &ctx.accounts.authority.key(),
                &[],
            )?,
            &ctx.accounts.infos(),
        )?;
        Ok(())
    }

    /// Public -> encrypted pending. The deposit amount itself remains public.
    pub fn deposit_confidential_tokens(ctx: Context<AccountAuthority>, amount: u64) -> Result<()> {
        let decimals = {
            let data = ctx.accounts.mint.try_borrow_data()?;
            StateWithExtensions::<Mint>::unpack(&data)?.base.decimals
        };
        invoke(
            &ct::deposit(
                &token::ID,
                &ctx.accounts.token_account.key(),
                &ctx.accounts.mint.key(),
                amount,
                decimals,
                &ctx.accounts.authority.key(),
                &[],
            )?,
            &ctx.accounts.infos(),
        )?;
        Ok(())
    }

    /// Only the client knows the AES key. Pass the new ciphertext, never secret keys.
    pub fn apply_pending_balance(
        ctx: Context<ApplyPendingBalance>,
        expected_pending_balance_credit_counter: u64,
        new_decryptable_available_balance: [u8; AE_CIPHERTEXT_LEN],
    ) -> Result<()> {
        invoke(
            &ct::apply_pending_balance(
                &token::ID,
                &ctx.accounts.token_account.key(),
                expected_pending_balance_credit_counter,
                &new_decryptable_available_balance.into(),
                &ctx.accounts.authority.key(),
                &[],
            )?,
            &[
                ctx.accounts.token_account.to_account_info(),
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.token_program.to_account_info(),
            ],
        )?;
        Ok(())
    }

    pub fn close_mint(ctx: Context<CloseMint>) -> Result<()> {
        {
            let data = ctx.accounts.mint.try_borrow_data()?;
            require!(
                StateWithExtensions::<Mint>::unpack(&data)?.base.supply == 0,
                RemittanceError::SupplyNotZero
            );
        }
        // Token-2022 enforces close authority and all extension-specific close rules.
        invoke(
            &token::instruction::close_account(
                &token::ID,
                &ctx.accounts.mint.key(),
                &ctx.accounts.destination.key(),
                &ctx.accounts.authority.key(),
                &[],
            )?,
            &[
                ctx.accounts.mint.to_account_info(),
                ctx.accounts.destination.to_account_info(),
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.token_program.to_account_info(),
            ],
        )?;
        Ok(())
    }
}

pub fn mint_extensions(confidential: bool) -> Vec<ExtensionType> {
    let mut extensions = vec![
        ExtensionType::TransferFeeConfig,
        ExtensionType::MetadataPointer,
        ExtensionType::DefaultAccountState,
        ExtensionType::MintCloseAuthority,
    ];
    if confidential {
        extensions.extend([
            ExtensionType::PermanentDelegate,
            ExtensionType::ConfidentialTransferMint,
            ExtensionType::ConfidentialTransferFeeConfig,
        ]);
    }
    extensions
}

fn create_mint(accounts: &CreateMint<'_>, fee_key: Option<[u8; 32]>) -> Result<()> {
    // The payer is assigned all issuer authorities at mint creation.
    let authority = accounts.payer.key();
    let mint = accounts.mint.key();
    let metadata = TokenMetadata {
        update_authority: Some(authority).try_into()?,
        mint,
        name: TOKEN_NAME.into(),
        symbol: TOKEN_SYMBOL.into(),
        uri: TOKEN_URI.into(),
        additional_metadata: vec![],
    };
    // Phase 1: allocate fixed extensions, fund rent for the final metadata size.
    // Variable-length TokenMetadata must NOT go in try_calculate_account_len.
    let space =
        ExtensionType::try_calculate_account_len::<Mint>(&mint_extensions(fee_key.is_some()))?;
    // Token-2022's TLV header is u16 type + u16 length, not the standalone
    // metadata interface's larger discriminator/header.
    let metadata_size = metadata.get_packed_len()?;
    let final_space = space
        .checked_add(4)
        .and_then(|n| n.checked_add(metadata_size))
        .ok_or(error!(RemittanceError::ArithmeticOverflow))?;
    anchor_lang::system_program::create_account(
        CpiContext::new(
            accounts.system_program.key(),
            anchor_lang::system_program::CreateAccount {
                from: accounts.payer.to_account_info(),
                to: accounts.mint.to_account_info(),
            },
        ),
        Rent::get()?.minimum_balance(final_space),
        space as u64,
        &token::ID,
    )?;
    let infos = [
        accounts.mint.to_account_info(),
        accounts.token_program.to_account_info(),
    ];
    // Phase 2: all fixed extension initializers precede InitializeMint2.
    invoke(
        &fees::initialize_transfer_fee_config(
            &token::ID,
            &mint,
            Some(&authority),
            Some(&authority),
            BASIS_POINTS,
            MAXIMUM_FEE,
        )?,
        &infos,
    )?;
    invoke(
        &initialize_metadata_pointer(&token::ID, &mint, Some(authority), Some(mint))?,
        &infos,
    )?;
    invoke(
        &initialize_default_account_state(&token::ID, &mint, &AccountState::Frozen)?,
        &infos,
    )?;
    invoke(
        &token::instruction::initialize_mint_close_authority(&token::ID, &mint, Some(&authority))?,
        &infos,
    )?;
    if let Some(fee_key) = fee_key {
        invoke(
            &token::instruction::initialize_permanent_delegate(&token::ID, &mint, &authority)?,
            &infos,
        )?;
        invoke(
            &ct::initialize_mint(&token::ID, &mint, Some(authority), false, None)?,
            &infos,
        )?;
        invoke(
            &ct_fee::initialize_confidential_transfer_fee_config(
                &token::ID,
                &mint,
                Some(authority),
                &fee_key.into(),
            )?,
            &infos,
        )?;
    }
    // Phase 3: Frozen defaults require a freeze authority.
    invoke(
        &token::instruction::initialize_mint2(
            &token::ID,
            &mint,
            &authority,
            Some(&authority),
            DECIMALS,
        )?,
        &infos,
    )?;
    // Actual metadata requires an initialized mint and grows its allocation.
    invoke(
        &spl_token_metadata_interface::instruction::initialize(
            &token::ID,
            &mint,
            &authority,
            &mint,
            &authority,
            metadata.name,
            metadata.symbol,
            metadata.uri,
        ),
        &[
            accounts.mint.to_account_info(),
            accounts.payer.to_account_info(),
            accounts.token_program.to_account_info(),
        ],
    )?;
    Ok(())
}

fn checked_fee_transfer(accounts: &TransferTokens<'_>, amount: u64, seizure: bool) -> Result<()> {
    let (decimals, fee) = {
        let data = accounts.mint.try_borrow_data()?;
        let mint = StateWithExtensions::<Mint>::unpack(&data)?;
        if seizure {
            let delegate = mint.get_extension::<PermanentDelegate>()?;
            require!(
                Option::<Pubkey>::from(delegate.delegate) == Some(accounts.authority.key()),
                RemittanceError::NotPermanentDelegate
            );
        }
        let fee = mint
            .get_extension::<TransferFeeConfig>()?
            .calculate_epoch_fee(Clock::get()?.epoch, amount)
            .ok_or(error!(RemittanceError::ArithmeticOverflow))?;
        (mint.base.decimals, fee)
    }; // End the data borrow before CPI.
    for account in [&accounts.source, &accounts.destination] {
        let data = account.try_borrow_data()?;
        let state = StateWithExtensions::<TokenAccount>::unpack(&data)?;
        require_keys_eq!(
            state.base.mint,
            accounts.mint.key(),
            RemittanceError::WrongMint
        );
    }
    invoke(
        &fees::transfer_checked_with_fee(
            &token::ID,
            &accounts.source.key(),
            &accounts.mint.key(),
            &accounts.destination.key(),
            &accounts.authority.key(),
            &[],
            amount,
            decimals,
            fee,
        )?,
        &[
            accounts.source.to_account_info(),
            accounts.mint.to_account_info(),
            accounts.destination.to_account_info(),
            accounts.authority.to_account_info(),
            accounts.token_program.to_account_info(),
        ],
    )?;
    Ok(())
}

#[derive(Accounts)]
pub struct CreateMint<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: fresh keypair account, created and initialized by the handler.
    #[account(mut, signer)]
    pub mint: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct TransferTokens<'info> {
    /// CHECK: owner checked here; StateWithExtensions and Token-2022 check data.
    #[account(mut, owner = token_program.key())]
    pub source: UncheckedAccount<'info>,
    /// CHECK: owner checked here; read with StateWithExtensions.
    #[account(owner = token_program.key())]
    pub mint: UncheckedAccount<'info>,
    /// CHECK: owner checked here; StateWithExtensions and Token-2022 check data.
    #[account(mut, owner = token_program.key())]
    pub destination: UncheckedAccount<'info>,
    pub authority: Signer<'info>,
    pub token_program: Program<'info, Token2022>,
}

#[derive(Accounts)]
pub struct AccountAuthority<'info> {
    /// CHECK: Token-2022 checks mint, state and the operation-specific authority.
    #[account(mut, owner = token_program.key())]
    pub token_account: UncheckedAccount<'info>,
    /// CHECK: owner checked here; Token-2022 validates mint contents.
    #[account(owner = token_program.key())]
    pub mint: UncheckedAccount<'info>,
    pub authority: Signer<'info>,
    pub token_program: Program<'info, Token2022>,
}
impl<'info> AccountAuthority<'info> {
    fn infos(&self) -> [AccountInfo<'info>; 4] {
        [
            self.token_account.to_account_info(),
            self.mint.to_account_info(),
            self.authority.to_account_info(),
            self.token_program.to_account_info(),
        ]
    }
}

#[derive(Accounts)]
pub struct ApplyPendingBalance<'info> {
    /// CHECK: Token-2022 checks confidential state and the owner's signature.
    #[account(mut, owner = token_program.key())]
    pub token_account: UncheckedAccount<'info>,
    pub authority: Signer<'info>,
    pub token_program: Program<'info, Token2022>,
}

#[derive(Accounts)]
pub struct CloseMint<'info> {
    /// CHECK: owner checked here; StateWithExtensions and Token-2022 validate contents.
    #[account(mut, owner = token_program.key())]
    pub mint: UncheckedAccount<'info>,
    /// CHECK: receives the mint's lamports; no data is read.
    #[account(mut)]
    pub destination: UncheckedAccount<'info>,
    pub authority: Signer<'info>,
    pub token_program: Program<'info, Token2022>,
}

#[error_code]
pub enum RemittanceError {
    #[msg("Fee or size calculation overflowed")]
    ArithmeticOverflow,
    #[msg("Token account belongs to a different mint")]
    WrongMint,
    #[msg("Signer is not this mint's permanent delegate")]
    NotPermanentDelegate,
    #[msg("Burn outstanding supply before closing the mint")]
    SupplyNotZero,
    #[msg("Provide a real nonzero ElGamal fee-withdraw public key")]
    InvalidFeeKey,
}
