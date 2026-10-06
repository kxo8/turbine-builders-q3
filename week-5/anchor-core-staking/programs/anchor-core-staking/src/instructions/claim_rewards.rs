use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token_interface::{
        mint_to_checked, Mint, MintToChecked, TokenAccount, TokenInterface,
    },
};

use mpl_core::{
    ID as MPL_CORE_ID, accounts::{BaseAssetV1, BaseCollectionV1}, fetch_plugin, instructions::UpdatePluginV1CpiBuilder, types::{Attribute, Attributes, FreezeDelegate, PluginType, UpdateAuthority},
};
use crate::error::ErrorCode;
use crate::state::Config;

const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Accounts)]
pub struct ClaimRewards<'info> {
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


pub fn handler(ctx: Context<ClaimRewards>) -> Result<()> {
    let (_, attributes, _) = fetch_plugin::<BaseAssetV1, Attributes>(
        &ctx.accounts.asset.to_account_info(),
        PluginType::Attributes,
    )
    .map_err(|_| ErrorCode::AssetNotStaked)?;

    let staking_status = attributes
        .attribute_list
        .iter()
        .find(|attribute| attribute.key == "staked")
        .ok_or(ErrorCode::AssetNotStaked)?;

    require!(
        staking_status.value == "true",
        ErrorCode::AssetNotStaked
    );

    let (_, freeze_delegate, _) = fetch_plugin::<BaseAssetV1, FreezeDelegate>(
        &ctx.accounts.asset.to_account_info(),
        PluginType::FreezeDelegate,
    )
    .map_err(|_| ErrorCode::AssetNotStaked)?;

    require!(
        freeze_delegate.frozen,
        ErrorCode::AssetNotStaked
    );

    // Read when the current stake began.
    let staked_at = attributes
        .attribute_list
        .iter()
        .find(|attribute| attribute.key == "staked_at")
        .ok_or(ErrorCode::InvalidTimestamp)?
        .value
        .parse::<i64>()
        .map_err(|_| ErrorCode::InvalidTimestamp)?;
    
    // Read the reward checkpoint.
    // Older stakes without this attribute start from staked_at.
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
    
    // Validate the order before calculating rewards.
    require!(
        staked_at >= 0
            && last_claimed_at >= staked_at
            && current_timestamp >= last_claimed_at,
        ErrorCode::InvalidTimestamp
    );
    
    let elapsed_seconds = current_timestamp
        .checked_sub(last_claimed_at)
        .ok_or(ErrorCode::InvalidTimestamp)?;
    
    let unpaid_days = elapsed_seconds / SECONDS_PER_DAY;
    
    // Advance only through complete days, preserving unpaid partial days.
    let paid_seconds = unpaid_days
        .checked_mul(SECONDS_PER_DAY)
        .ok_or(ErrorCode::InvalidTimestamp)?;
    
    let new_last_claimed_at = last_claimed_at
        .checked_add(paid_seconds)
        .ok_or(ErrorCode::InvalidTimestamp)?;

    // No complete unpaid day means no rewards to claim yet.
    if unpaid_days == 0 {
        return Ok(());
    }
    
    let unpaid_days = u64::try_from(unpaid_days)
        .map_err(|_| ErrorCode::InvalidTimestamp)?;
    
    let token_scale = 10u64
        .checked_pow(ctx.accounts.rewards_mint.decimals as u32)
        .ok_or(ErrorCode::InvalidRewardsBps)?;
    
    let amount = unpaid_days
        .checked_mul(ctx.accounts.config.rewards_bps as u64)
        .and_then(|value| value.checked_mul(token_scale))
        .and_then(|value| value.checked_div(10_000))
        .ok_or(ErrorCode::InvalidRewardsBps)?;

    
    let collection_key = ctx.accounts.collection.key();
    
    let config_seeds: &[&[u8]] = &[
        b"config",
        collection_key.as_ref(),
        &[ctx.accounts.config.bump],
    ];
    
    let signer_seeds = &[config_seeds];

    mint_to_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            MintToChecked {
                mint: ctx.accounts.rewards_mint.to_account_info(),
                to: ctx.accounts.user_rewards_ata.to_account_info(),
                authority: ctx.accounts.config.to_account_info(),
            },
            signer_seeds,
        ),
        amount,
        ctx.accounts.rewards_mint.decimals,
    )?;


    // Keep every attribute except the old reward checkpoint.
    let mut attributes_list: Vec<Attribute> = attributes
        .attribute_list
        .iter()
        .filter(|attribute| attribute.key != "last_claimed_at")
        .cloned()
        .collect();
    
    // Add the checkpoint through which rewards were paid.
    attributes_list.push(Attribute {
        key: "last_claimed_at".to_string(),
        value: new_last_claimed_at.to_string(),
    });
    
    // Updating the Attributes plugin requires the collection authority PDA.
    let update_authority_seeds: &[&[u8]] = &[
        b"update_authority",
        collection_key.as_ref(),
        &[ctx.bumps.update_authority],
    ];
    
    UpdatePluginV1CpiBuilder::new(
        &ctx.accounts.mpl_core_program.to_account_info(),
    )
    .asset(&ctx.accounts.asset.to_account_info())
    .collection(Some(&ctx.accounts.collection.to_account_info()))
    .payer(&ctx.accounts.owner.to_account_info())
    .authority(Some(&ctx.accounts.update_authority.to_account_info()))
    .system_program(&ctx.accounts.system_program.to_account_info())
    .plugin(mpl_core::types::Plugin::Attributes(Attributes {
        attribute_list: attributes_list,
    }))
    .invoke_signed(&[update_authority_seeds])?;
    

    Ok(())
}