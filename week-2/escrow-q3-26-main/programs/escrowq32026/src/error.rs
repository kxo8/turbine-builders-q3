use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("The escrow has expired")]
    EscrowExpired,

    #[msg("new expiration must be in future")]
    InvalidExpiration
}
