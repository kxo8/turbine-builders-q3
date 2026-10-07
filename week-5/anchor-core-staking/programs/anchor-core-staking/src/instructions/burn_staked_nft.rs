use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token_interface::{
        mint_to_checked, Mint, MintToChecked, TokenAccount, TokenInterface,
    },
};

use mpl_core::{
    accounts::{BaseAssetV1, BaseCollectionV1},
    fetch_plugin,
    instructions::{AddPluginV1CpiBuilder, UpdatePluginV1CpiBuilder,BurnV1CpiBuilder},
    types::{
        Attributes, BurnDelegate, FreezeDelegate, Plugin,
        PluginAuthority, PluginType, UpdateAuthority,
    },
    ID as MPL_CORE_ID,
};

use crate::error::ErrorCode;
use crate::state::Config;

use super::update_total_staked::{
    update_total_staked,
    StakedCountChange,
};

const SECONDS_PER_DAY: i64 = 86_400;
const BURN_BONUS_TOKENS: u64 = 100;
#[derive(Accounts)]
pub struct BurnStakedNft<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"config", collection.key().as_ref()],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,

    #[account(
        mut,
        has_one = owner @ ErrorCode::InvalidOwner,
        constraint = asset.update_authority
            == UpdateAuthority::Collection(collection.key())
            @ ErrorCode::InvalidUpdateAuthority,
    )]
    pub asset: Account<'info, BaseAssetV1>,

    #[account(
        mut,
        has_one = update_authority @ ErrorCode::InvalidUpdateAuthority,
    )]
    pub collection: Account<'info, BaseCollectionV1>,

    /// CHECK: The seeds constraint validates this authority PDA.
    #[account(
        seeds = [b"update_authority", collection.key().as_ref()],
        bump,
    )]
    pub update_authority: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [b"rewards_mint", config.key().as_ref()],
        bump = config.rewards_bump,
        mint::authority = config,
        mint::token_program = token_program,
    )]
    pub rewards_mint: InterfaceAccount<'info, Mint>,

    #[account(
        init_if_needed,
        payer = owner,
        associated_token::mint = rewards_mint,
        associated_token::authority = owner,
        associated_token::token_program = token_program,
    )]
    pub user_rewards_ata: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,

    /// CHECK: The address constraint validates the Core program ID.
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,
}

pub fn handler(ctx: Context<BurnStakedNft>) -> Result<()> {
    // Read the NFT's staking attributes.
    let (_, attributes, _) = fetch_plugin::<BaseAssetV1, Attributes>(
        &ctx.accounts.asset.to_account_info(),
        PluginType::Attributes,
    )
    .map_err(|_| ErrorCode::AssetNotStaked)?;

    // Require an explicit staking-status entry.
    let staking_status = attributes
        .attribute_list
        .iter()
        .find(|attribute| attribute.key == "staked")
        .ok_or(ErrorCode::AssetNotStaked)?;

    require!(
        staking_status.value == "true",
        ErrorCode::AssetNotStaked
    );

    // Verify the actual freeze state separately.
    let (_, freeze_delegate, _) = fetch_plugin::<BaseAssetV1, FreezeDelegate>(
        &ctx.accounts.asset.to_account_info(),
        PluginType::FreezeDelegate,
    )
    .map_err(|_| ErrorCode::AssetNotStaked)?;

    require!(
        freeze_delegate.frozen,
        ErrorCode::AssetNotStaked
    );

    // Read the original staking time.
    let staked_at = attributes
        .attribute_list
        .iter()
        .find(|attribute| attribute.key == "staked_at")
        .ok_or(ErrorCode::InvalidTimestamp)?
        .value
        .parse::<i64>()
        .map_err(|_| ErrorCode::InvalidTimestamp)?;
    
    // Read the reward checkpoint.
    // Older stakes without a checkpoint start from staked_at.
    let last_claimed_at = match attributes
        .attribute_list
        .iter()
        .find(|attribute| attribute.key == "last_claimed_at")
    {
        Some(attribute) => attribute
            .value
            .parse::<i64>()
            .map_err(|_| ErrorCode::InvalidTimestamp)?,
        None => staked_at,
    };
    
    let current_timestamp = Clock::get()?.unix_timestamp;
    
    // Reject timestamps outside the current stake's valid timeline.
    require!(
        staked_at >= 0
            && last_claimed_at >= staked_at
            && current_timestamp >= last_claimed_at,
        ErrorCode::InvalidTimestamp
    );
    
    // Burn eligibility depends on the original staking time.
    let total_staked_days = current_timestamp
        .checked_sub(staked_at)
        .ok_or(ErrorCode::InvalidTimestamp)?
        / SECONDS_PER_DAY;
    
    require!(
        total_staked_days >= ctx.accounts.config.freeze_period as i64,
        ErrorCode::FreezePeriodNotElapsed
    );
    
    // Normal rewards depend on the last paid checkpoint.
    let unpaid_days = current_timestamp
        .checked_sub(last_claimed_at)
        .ok_or(ErrorCode::InvalidTimestamp)?
        / SECONDS_PER_DAY;


    let unpaid_days = u64::try_from(unpaid_days)
        .map_err(|_| ErrorCode::InvalidTimestamp)?;
    
    // Convert whole tokens into the mint's base units.
    let token_scale = 10u64
        .checked_pow(ctx.accounts.rewards_mint.decimals as u32)
        .ok_or(ErrorCode::InvalidRewardsBps)?;
    
    // Calculate normal rewards that have not been claimed.
    let unpaid_rewards = unpaid_days
        .checked_mul(ctx.accounts.config.rewards_bps as u64)
        .and_then(|value| value.checked_mul(token_scale))
        .and_then(|value| value.checked_div(10_000))
        .ok_or(ErrorCode::InvalidRewardsBps)?;
    
    // Convert the fixed bonus into base units.
    let burn_bonus = BURN_BONUS_TOKENS
        .checked_mul(token_scale)
        .ok_or(ErrorCode::InvalidRewardsBps)?;
    
    // Combine both payments.
    let amount = unpaid_rewards
        .checked_add(burn_bonus)
        .ok_or(ErrorCode::InvalidRewardsBps)?;
    
    let collection_key = ctx.accounts.collection.key();
    
    let update_authority_seeds: &[&[u8]] = &[
        b"update_authority",
        collection_key.as_ref(),
        &[ctx.bumps.update_authority],
    ];
    
    // The existing freeze authority PDA authorizes thawing.
    UpdatePluginV1CpiBuilder::new(
        &ctx.accounts.mpl_core_program.to_account_info(),
    )
    .asset(&ctx.accounts.asset.to_account_info())
    .collection(Some(&ctx.accounts.collection.to_account_info()))
    .payer(&ctx.accounts.owner.to_account_info())
    .authority(Some(&ctx.accounts.update_authority.to_account_info()))
    .system_program(&ctx.accounts.system_program.to_account_info())
    .plugin(Plugin::FreezeDelegate(FreezeDelegate {
        frozen: false,
    }))
    .invoke_signed(&[update_authority_seeds])?;
    
    // The owner authorizes adding a BurnDelegate,
    // assigning its authority to our program's PDA.
    AddPluginV1CpiBuilder::new(
        &ctx.accounts.mpl_core_program.to_account_info(),
    )
    .asset(&ctx.accounts.asset.to_account_info())
    .collection(Some(&ctx.accounts.collection.to_account_info()))
    .payer(&ctx.accounts.owner.to_account_info())
    .authority(Some(&ctx.accounts.owner.to_account_info()))
    .system_program(&ctx.accounts.system_program.to_account_info())
    .plugin(Plugin::BurnDelegate(BurnDelegate {}))
    .init_authority(PluginAuthority::Address {
        address: ctx.accounts.update_authority.key(),
    })
    .invoke()?;

    // Burn using the PDA's BurnDelegate permission.
    BurnV1CpiBuilder::new(
        &ctx.accounts.mpl_core_program.to_account_info(),
    )
    .asset(&ctx.accounts.asset.to_account_info())
    .collection(Some(&ctx.accounts.collection.to_account_info()))
    .payer(&ctx.accounts.owner.to_account_info())
    .authority(Some(&ctx.accounts.update_authority.to_account_info()))
    .system_program(Some(&ctx.accounts.system_program.to_account_info()))
    .invoke_signed(&[update_authority_seeds])?;
    
    // Minting rewards requires a different authority: the config PDA.
    let config_seeds: &[&[u8]] = &[
        b"config",
        collection_key.as_ref(),
        &[ctx.accounts.config.bump],
    ];
    
    mint_to_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            MintToChecked {
                mint: ctx.accounts.rewards_mint.to_account_info(),
                to: ctx.accounts.user_rewards_ata.to_account_info(),
                authority: ctx.accounts.config.to_account_info(),
            },
            &[config_seeds],
        ),
        amount,
        ctx.accounts.rewards_mint.decimals,
    )?;

    update_total_staked(
        &ctx.accounts.collection.to_account_info(),
        &ctx.accounts.update_authority.to_account_info(),
        &ctx.accounts.owner.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program.to_account_info(),
        &[update_authority_seeds],
        StakedCountChange::Decrement,
    )?;
    
    Ok(())
}