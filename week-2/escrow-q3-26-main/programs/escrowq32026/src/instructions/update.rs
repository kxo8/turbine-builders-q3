use anchor_lang::prelude::*;

use crate::{ESCROW_SEED, error::ErrorCode, state::Escrow};

#[derive(Accounts)]
pub struct Update<'info> {
    #[account(mut)]
    pub maker: Signer<'info>,

    #[account(
        mut,
        has_one = maker,
        seeds = [
            ESCROW_SEED,
            maker.key().as_ref(),
            escrow.seed.to_le_bytes().as_ref()
        ],
        bump = escrow.bump,
    )]
    pub escrow: Account<'info, Escrow>,

    pub clock: Sysvar<'info, Clock>,
}

impl<'info> Update<'info> {
    pub fn update(&mut self, expiration: i64) -> Result<()> {

        // The existing escrow must not already be expired.
        require!(
            self.escrow.expiration > self.clock.unix_timestamp,
            ErrorCode::EscrowExpired
        );

        // The new expiration must be in the future.
        require!(
            expiration > self.clock.unix_timestamp,
            ErrorCode::InvalidExpiration
        );

        // Update the escrow's expiration time.
        self.escrow.expiration = expiration;

        Ok(())
    }
}

