use super::*;
use anchor_lang::{InstructionData, ToAccountMetas};
use proofext::instruction::ProofLocation;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use std::num::NonZeroI8;
use t22new::extension::{
    confidential_transfer::{instruction as ct, ConfidentialTransferAccount},
    confidential_transfer_fee::ConfidentialTransferFeeConfig,
};
use zk::{
    encryption::{
        auth_encryption::AeKey,
        derivation::derive_confidential_keys,
        elgamal::{ElGamalCiphertext, ElGamalPubkey},
    },
    zk_elgamal_proof_program::pubkey_validity::build_pubkey_validity_proof_data,
};
use zkif::{
    instruction::{close_context_state, ContextStateInfo, ProofInstruction},
    proof_data::ZkProofData,
    state::ProofContextState,
};

pub struct Holder {
    pub account: Pubkey,
    pub elgamal: ElGamalKeypair,
    pub aes: AeKey,
}

/// Anyone can fund and create another owner's ATA. Owner does NOT sign here.
pub fn create_ata(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, owner: &Pubkey) -> Pubkey {
    use anchor_spl::associated_token::{
        get_associated_token_address_with_program_id, spl_associated_token_account,
    };
    let address = get_associated_token_address_with_program_id(owner, mint, &TOKEN);
    let ix = spl_associated_token_account::instruction::create_associated_token_account(
        &payer.pubkey(),
        owner,
        mint,
        &TOKEN,
    );
    send(svm, payer, &[ix], &[]);
    address
}

/// Owner signature authorizes extension reallocation, separately from ATA creation.
pub fn prepare(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, owner: &Keypair) -> Holder {
    let account = create_ata(svm, payer, mint, &owner.pubkey());
    assert_eq!(
        read_account(svm, &account).state,
        t22new::state::AccountState::Frozen
    );
    thaw(svm, payer, mint, &account);
    send(
        svm,
        payer,
        &[t22new::instruction::reallocate(
            &TOKEN,
            &account,
            &payer.pubkey(),
            &owner.pubkey(),
            &[],
            &[
                ExtensionType::ConfidentialTransferAccount,
                ExtensionType::ConfidentialTransferFeeAmount,
            ],
        )
        .unwrap()],
        &[owner],
    );
    let (elgamal, aes) = derive_confidential_keys(owner, b"").unwrap();
    Holder {
        account,
        elgamal,
        aes,
    }
}

pub fn configure_ixs(holder: &Holder, mint: &Pubkey, authority: &Pubkey) -> Vec<Instruction> {
    let proof = build_pubkey_validity_proof_data(&holder.elgamal).unwrap();
    ct::configure_account(
        &TOKEN,
        &holder.account,
        mint,
        &holder.aes.encrypt(0).into(),
        65536,
        authority,
        &[],
        ProofLocation::InstructionOffset(NonZeroI8::new(1).unwrap(), &proof),
    )
    .unwrap()
}
pub fn configure(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    holder: &Holder,
    owner: &Keypair,
) {
    send(
        svm,
        payer,
        &configure_ixs(holder, mint, &owner.pubkey()),
        &[owner],
    );
    assert!(!bool::from(read_ct(svm, &holder.account).approved));
}
pub fn approve(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, holder: &Holder) {
    send(
        svm,
        payer,
        &[account_ix(
            *mint,
            holder.account,
            payer.pubkey(),
            instruction::ApproveConfidentialAccount {}.data(),
        )],
        &[],
    );
    assert!(bool::from(read_ct(svm, &holder.account).approved));
}
pub fn deposit(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    holder: &Holder,
    owner: &Keypair,
    amount: u64,
) {
    send(
        svm,
        payer,
        &[account_ix(
            *mint,
            holder.account,
            owner.pubkey(),
            instruction::DepositConfidentialTokens { amount }.data(),
        )],
        &[owner],
    );
}
pub fn read_ct(svm: &LiteSVM, address: &Pubkey) -> ConfidentialTransferAccount {
    let raw = svm.get_account(address).unwrap();
    assert_eq!(raw.owner, TOKEN);
    *StateWithExtensions::<Account>::unpack(&raw.data)
        .unwrap()
        .get_extension::<ConfidentialTransferAccount>()
        .unwrap()
}
pub fn available(svm: &LiteSVM, holder: &Holder) -> u64 {
    let ct = read_ct(svm, &holder.account);
    // AES decrypts arbitrary u64 balances without a discrete-log search.
    holder
        .aes
        .decrypt(&ct.decryptable_available_balance.try_into().unwrap())
        .unwrap()
}
pub fn pending(svm: &LiteSVM, holder: &Holder) -> u64 {
    let ct = read_ct(svm, &holder.account);
    let lo: ElGamalCiphertext = ct.pending_balance_lo.try_into().unwrap();
    let hi: ElGamalCiphertext = ct.pending_balance_hi.try_into().unwrap();
    holder.elgamal.secret().decrypt_u32(&lo).unwrap()
        + (holder.elgamal.secret().decrypt_u32(&hi).unwrap() << 16)
}
pub fn apply_pending(svm: &mut LiteSVM, payer: &Keypair, holder: &Holder, owner: &Keypair) {
    let state = read_ct(svm, &holder.account);
    let value = available(svm, holder)
        .checked_add(pending(svm, holder))
        .unwrap();
    let ix = Instruction {
        program_id: ID,
        accounts: accounts::ApplyPendingBalance {
            token_account: holder.account,
            authority: owner.pubkey(),
            token_program: TOKEN,
        }
        .to_account_metas(None),
        data: instruction::ApplyPendingBalance {
            expected_pending_balance_credit_counter: state.pending_balance_credit_counter.into(),
            new_decryptable_available_balance: holder.aes.encrypt(value).to_bytes(),
        }
        .data(),
    };
    send(svm, payer, &[ix], &[owner]);
    assert_eq!(pending(svm, holder), 0);
    assert_eq!(available(svm, holder), value);
}

/// Real verification through the ZK program, not injected or fabricated contexts.
pub fn stage_proof<T: bytemuck::Pod + ZkProofData<U>, U: bytemuck::Pod>(
    svm: &mut LiteSVM,
    payer: &Keypair,
    kind: ProofInstruction,
    proof: &T,
) -> Pubkey {
    let context = Keypair::new();
    let size = std::mem::size_of::<ProofContextState<U>>();
    send(
        svm,
        payer,
        &[solana_system_interface::instruction::create_account(
            &payer.pubkey(),
            &context.pubkey(),
            svm.minimum_balance_for_rent_exemption(size),
            size as u64,
            &zkif::ID,
        )],
        &[&context],
    );
    let ix = kind.encode_verify_proof(
        Some(ContextStateInfo {
            context_state_account: &context.pubkey(),
            context_state_authority: &payer.pubkey(),
        }),
        proof,
    );
    send(
        svm,
        payer,
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            ix,
        ],
        &[],
    );
    context.pubkey()
}
pub fn close_contexts(svm: &mut LiteSVM, payer: &Keypair, contexts: &[Pubkey]) {
    let ixs: Vec<_> = contexts
        .iter()
        .map(|c| {
            close_context_state(
                ContextStateInfo {
                    context_state_account: c,
                    context_state_authority: &payer.pubkey(),
                },
                &payer.pubkey(),
            )
        })
        .collect();
    send(svm, payer, &ixs, &[]);
    for c in contexts {
        assert!(svm.get_account(c).is_none_or(|a| a.data.is_empty()));
    }
}

/// Confidential fee-bearing transfer. Read live epoch parameters before making proofs.
pub fn transfer(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    sender: &Holder,
    owner: &Keypair,
    receiver: &Holder,
    amount: u64,
) -> u64 {
    let state = read_ct(svm, &sender.account);
    let balance = available(svm, sender);
    let config = fee_config(svm, mint);
    let epoch = svm.get_sysvar::<solana_clock::Clock>().epoch;
    let fee = config.calculate_epoch_fee(epoch, amount).unwrap();
    let rate = config.get_epoch_fee(epoch);
    let raw = svm.get_account(mint).unwrap();
    let mint_state = StateWithExtensions::<Mint>::unpack(&raw.data).unwrap();
    let fee_pubkey: ElGamalPubkey = mint_state
        .get_extension::<ConfidentialTransferFeeConfig>()
        .unwrap()
        .withdraw_withheld_authority_elgamal_pubkey
        .try_into()
        .unwrap();
    let recipient_pubkey: ElGamalPubkey = read_ct(svm, &receiver.account)
        .elgamal_pubkey
        .try_into()
        .unwrap();
    let proofs = proofgen::transfer_with_fee::transfer_with_fee_split_proof_data(
        &state.available_balance.try_into().unwrap(),
        &state.decryptable_available_balance.try_into().unwrap(),
        amount,
        &sender.elgamal,
        &sender.aes,
        &recipient_pubkey,
        None,
        &fee_pubkey,
        rate.transfer_fee_basis_points.into(),
        rate.maximum_fee.into(),
    )
    .unwrap();
    let eq = stage_proof(
        svm,
        payer,
        ProofInstruction::VerifyCiphertextCommitmentEquality,
        &proofs.equality_proof_data,
    );
    let validity = stage_proof(
        svm,
        payer,
        ProofInstruction::VerifyBatchedGroupedCiphertext3HandlesValidity,
        &proofs
            .transfer_amount_ciphertext_validity_proof_data_with_ciphertext
            .proof_data,
    );
    let percent = stage_proof(
        svm,
        payer,
        ProofInstruction::VerifyPercentageWithCap,
        &proofs.percentage_with_cap_proof_data,
    );
    let fee_validity = stage_proof(
        svm,
        payer,
        ProofInstruction::VerifyBatchedGroupedCiphertext2HandlesValidity,
        &proofs.fee_ciphertext_validity_proof_data,
    );
    let range = stage_proof(
        svm,
        payer,
        ProofInstruction::VerifyBatchedRangeProofU256,
        &proofs.range_proof_data,
    );
    let ixs = ct::transfer_with_fee(
        &TOKEN,
        &sender.account,
        mint,
        &receiver.account,
        &sender
            .aes
            .encrypt(balance.checked_sub(amount).unwrap())
            .into(),
        &proofs
            .transfer_amount_ciphertext_validity_proof_data_with_ciphertext
            .ciphertext_lo,
        &proofs
            .transfer_amount_ciphertext_validity_proof_data_with_ciphertext
            .ciphertext_hi,
        &owner.pubkey(),
        &[],
        ProofLocation::ContextStateAccount(&eq),
        ProofLocation::ContextStateAccount(&validity),
        ProofLocation::ContextStateAccount(&percent),
        ProofLocation::ContextStateAccount(&fee_validity),
        ProofLocation::ContextStateAccount(&range),
    )
    .unwrap();
    send(svm, payer, &ixs, &[owner]);
    close_contexts(svm, payer, &[eq, validity, percent, fee_validity, range]);
    fee
}

/// Apply pending FIRST, re-read the resulting state, THEN generate withdrawal proofs.
/// Withdrawal makes the amount public again. It is not a confidential transfer.
pub fn withdraw(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    holder: &Holder,
    owner: &Keypair,
    amount: u64,
) {
    apply_pending(svm, payer, holder, owner);
    let state = read_ct(svm, &holder.account);
    let balance = available(svm, holder);
    let proofs = proofgen::withdraw::withdraw_proof_data(
        &state.available_balance.try_into().unwrap(),
        balance,
        amount,
        &holder.elgamal,
    )
    .unwrap();
    let eq = stage_proof(
        svm,
        payer,
        ProofInstruction::VerifyCiphertextCommitmentEquality,
        &proofs.equality_proof_data,
    );
    let range = stage_proof(
        svm,
        payer,
        ProofInstruction::VerifyBatchedRangeProofU64,
        &proofs.range_proof_data,
    );
    let raw = svm.get_account(mint).unwrap();
    let decimals = StateWithExtensions::<Mint>::unpack(&raw.data)
        .unwrap()
        .base
        .decimals;
    let ixs = ct::withdraw(
        &TOKEN,
        &holder.account,
        mint,
        amount,
        decimals,
        &holder
            .aes
            .encrypt(balance.checked_sub(amount).unwrap())
            .into(),
        &owner.pubkey(),
        &[],
        ProofLocation::ContextStateAccount(&eq),
        ProofLocation::ContextStateAccount(&range),
    )
    .unwrap();
    send(svm, payer, &ixs, &[owner]);
    close_contexts(svm, payer, &[eq, range]);
}
