mod common;
use anchor_lang::{prelude::Pubkey, InstructionData};
use common::{confidential::*, *};
use litesvm::LiteSVM;
use remittance_stablecoin::{instruction, DECIMALS, TOKEN_NAME, TOKEN_SYMBOL};
use solana_clock::Clock;
use solana_keypair::Keypair;
use solana_signer::Signer;
use t22new::{
    extension::{
        confidential_transfer::ConfidentialTransferMint,
        confidential_transfer_fee::{ConfidentialTransferFeeAmount, ConfidentialTransferFeeConfig},
        default_account_state::DefaultAccountState,
        metadata_pointer::MetadataPointer,
        mint_close_authority::MintCloseAuthority,
        permanent_delegate::PermanentDelegate,
        transfer_fee::TransferFeeAmount,
        BaseStateWithExtensions, ExtensionType, StateWithExtensions,
    },
    state::{Account, AccountState, Mint},
};
use zk::encryption::elgamal::{ElGamalCiphertext, ElGamalKeypair};

fn check_mint(svm: &LiteSVM, mint: &Pubkey, issuer: &Pubkey, confidential: bool) {
    let raw = svm.get_account(mint).unwrap();
    assert_eq!(raw.owner, TOKEN);
    let state = StateWithExtensions::<Mint>::unpack(&raw.data).unwrap();
    assert_eq!(state.base.decimals, DECIMALS);
    assert_eq!(state.base.supply, 0);
    assert_eq!(
        Option::<Pubkey>::from(state.base.freeze_authority),
        Some(*issuer)
    );
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
    let fixed_len = ExtensionType::try_calculate_account_len::<Mint>(&extensions).unwrap();
    extensions.push(ExtensionType::TokenMetadata);
    assert_eq!(state.get_extension_types().unwrap(), extensions);
    let metadata = state
        .get_variable_len_extension::<token_metadata_new::state::TokenMetadata>()
        .unwrap();
    assert_eq!(metadata.mint, *mint);
    assert_eq!(metadata.name, TOKEN_NAME);
    assert_eq!(metadata.symbol, TOKEN_SYMBOL);
    let metadata_len = 64 + 16 + metadata.name.len() + metadata.symbol.len() + metadata.uri.len();
    assert_eq!(raw.data.len(), fixed_len + 4 + metadata_len);
    assert!(raw.lamports >= svm.minimum_balance_for_rent_exemption(raw.data.len()));
    assert_eq!(
        state.get_extension::<DefaultAccountState>().unwrap().state,
        AccountState::Frozen as u8
    );
    assert_eq!(
        Option::<Pubkey>::from(
            state
                .get_extension::<MetadataPointer>()
                .unwrap()
                .metadata_address
        ),
        Some(*mint)
    );
    assert_eq!(
        Option::<Pubkey>::from(
            state
                .get_extension::<MintCloseAuthority>()
                .unwrap()
                .close_authority
        ),
        Some(*issuer)
    );
    if confidential {
        assert!(!bool::from(
            state
                .get_extension::<ConfidentialTransferMint>()
                .unwrap()
                .auto_approve_new_accounts
        ));
        assert_eq!(
            Option::<Pubkey>::from(state.get_extension::<PermanentDelegate>().unwrap().delegate),
            Some(*issuer)
        );
    }
}

#[test]
fn test_standard_mint() {
    let (mut svm, payer) = setup();
    let mint = create_mint(&mut svm, &payer, None);

    check_mint(&svm, &mint.pubkey(), &payer.pubkey(), false);

    let owner = Keypair::new();
    let alice = create_holder(&mut svm, &payer, &mint.pubkey(), &owner.pubkey());
    let bob = create_holder(&mut svm, &payer, &mint.pubkey(), &payer.pubkey());
    
    assert_eq!(read_account(&svm, &alice).state, AccountState::Frozen);
    thaw(&mut svm, &payer, &mint.pubkey(), &alice);
    assert_eq!(read_account(&svm, &alice).state, AccountState::Initialized);
    assert_eq!(read_account(&svm, &bob).state, AccountState::Frozen);
    thaw(&mut svm, &payer, &mint.pubkey(), &bob);
    mint_to(&mut svm, &payer, &mint.pubkey(), &alice, 30_000);
    send(
        &mut svm,
        &payer,
        &[transfer_ix(
            mint.pubkey(),
            alice,
            bob,
            owner.pubkey(),
            10_000,
            false,
        )],
        &[&owner],
    );
    assert_eq!(read_account(&svm, &bob).amount, 9_900);

    // The next transfer must use the scheduled fee once its epoch activates.
    send(
        &mut svm,
        &payer,
        &[
            t22new::extension::transfer_fee::instruction::set_transfer_fee(
                &TOKEN,
                &mint.pubkey(),
                &payer.pubkey(),
                &[],
                250,
                1000,
            )
            .unwrap(),
        ],
        &[],
    );
    let config = fee_config(&svm, &mint.pubkey());
    let mut clock = svm.get_sysvar::<Clock>();
    clock.epoch = config.newer_transfer_fee.epoch.into();
    svm.set_sysvar(&clock);
    assert_eq!(config.calculate_epoch_fee(clock.epoch, 10_000), Some(250));
    send(
        &mut svm,
        &payer,
        &[transfer_ix(
            mint.pubkey(),
            alice,
            bob,
            owner.pubkey(),
            10_000,
            false,
        )],
        &[&owner],
    );
    assert_eq!(read_account(&svm, &alice).amount, 10_000);
    assert_eq!(read_account(&svm, &bob).amount, 19_650);
    let raw = svm.get_account(&bob).unwrap();
    let state = StateWithExtensions::<Account>::unpack(&raw.data).unwrap();
    assert_eq!(
        u64::from(
            state
                .get_extension::<TransferFeeAmount>()
                .unwrap()
                .withheld_amount
        ),
        350
    );
    let next = create_holder(&mut svm, &payer, &mint.pubkey(), &owner.pubkey());
    assert_eq!(read_account(&svm, &next).state, AccountState::Frozen);

    // Close an unused zero-supply mint through its close authority.
    let unused = create_mint(&mut svm, &payer, None);
    send(
        &mut svm,
        &payer,
        &[close_ix(unused.pubkey(), payer.pubkey(), payer.pubkey())],
        &[],
    );
    assert!(svm
        .get_account(&unused.pubkey())
        .is_none_or(|a| a.data.is_empty()));
}

#[test]
fn test_reissue_and_seize() {
    let (mut svm, payer) = setup();
    let fee_key = ElGamalKeypair::new_rand();
    let original = create_mint(&mut svm, &payer, None);
    let mint = create_mint(&mut svm, &payer, Some(&fee_key));
    assert_ne!(original.pubkey(), mint.pubkey());
    check_mint(&svm, &mint.pubkey(), &payer.pubkey(), true);
    let raw = svm.get_account(&mint.pubkey()).unwrap();
    let state = StateWithExtensions::<Mint>::unpack(&raw.data).unwrap();
    let bytes: [u8; 32] = fee_key.pubkey().into();
    assert_eq!(
        state
            .get_extension::<ConfidentialTransferFeeConfig>()
            .unwrap()
            .withdraw_withheld_authority_elgamal_pubkey
            .0,
        bytes
    );
    let owner = Keypair::new();
    let source = create_holder(&mut svm, &payer, &mint.pubkey(), &owner.pubkey());
    let treasury = create_holder(&mut svm, &payer, &mint.pubkey(), &payer.pubkey());
    for account in [source, treasury] {
        thaw(&mut svm, &payer, &mint.pubkey(), &account);
    }
    mint_to(&mut svm, &payer, &mint.pubkey(), &source, 10_000);
    // Issuer signs; the holder does not sign or grant a delegation.
    send(
        &mut svm,
        &payer,
        &[transfer_ix(
            mint.pubkey(),
            source,
            treasury,
            payer.pubkey(),
            10_000,
            true,
        )],
        &[],
    );
    assert_eq!(read_account(&svm, &source).amount, 0);
    assert_eq!(read_account(&svm, &treasury).amount, 9_900);
}

#[test]
fn test_confidential_lifecycle() {
    let (mut svm, payer) = setup();
    let fee_key = ElGamalKeypair::new_rand();
    let mint = create_mint(&mut svm, &payer, Some(&fee_key));
    let alice_owner = Keypair::new();
    let bob_owner = Keypair::new();
    // Issuer creates ATAs for Alice and Bob without their signatures.
    let alice = prepare(&mut svm, &payer, &mint.pubkey(), &alice_owner);
    let bob = prepare(&mut svm, &payer, &mint.pubkey(), &bob_owner);
    // ATA creation grants no right to configure somebody else's encrypted keys.
    let bad = configure_ixs(&alice, &mint.pubkey(), &payer.pubkey());
    fails(&mut svm, &payer, &bad, &[], "OwnerMismatch");
    configure(&mut svm, &payer, &mint.pubkey(), &alice, &alice_owner);
    configure(&mut svm, &payer, &mint.pubkey(), &bob, &bob_owner);
    mint_to(&mut svm, &payer, &mint.pubkey(), &alice.account, 10_000);
    let unapproved = account_ix(
        mint.pubkey(),
        alice.account,
        alice_owner.pubkey(),
        instruction::DepositConfidentialTokens { amount: 10_000 }.data(),
    );
    fails(
        &mut svm,
        &payer,
        &[unapproved],
        &[&alice_owner],
        "ConfidentialTransferAccountNotApproved",
    );
    let wrong_approval = account_ix(
        mint.pubkey(),
        alice.account,
        alice_owner.pubkey(),
        instruction::ApproveConfidentialAccount {}.data(),
    );
    fails(
        &mut svm,
        &payer,
        &[wrong_approval],
        &[&alice_owner],
        "MissingRequiredSignature",
    );
    approve(&mut svm, &payer, &mint.pubkey(), &alice);
    approve(&mut svm, &payer, &mint.pubkey(), &bob);
    deposit(
        &mut svm,
        &payer,
        &mint.pubkey(),
        &alice,
        &alice_owner,
        10_000,
    );
    assert_eq!(read_account(&svm, &alice.account).amount, 0);
    assert_eq!(pending(&svm, &alice), 10_000);
    assert_eq!(available(&svm, &alice), 0);
    apply_pending(&mut svm, &payer, &alice, &alice_owner);
    assert_eq!(available(&svm, &alice), 10_000);

    // GAP: the permanent delegate cannot reach the encrypted 10,000 balance.
    let treasury = create_holder(&mut svm, &payer, &mint.pubkey(), &payer.pubkey());
    thaw(&mut svm, &payer, &mint.pubkey(), &treasury);
    fails(
        &mut svm,
        &payer,
        &[transfer_ix(
            mint.pubkey(),
            alice.account,
            treasury,
            payer.pubkey(),
            1,
            true,
        )],
        &[],
        "InsufficientFunds",
    );
    assert_eq!(available(&svm, &alice), 10_000);

    let fee = transfer(
        &mut svm,
        &payer,
        &mint.pubkey(),
        &alice,
        &alice_owner,
        &bob,
        2_500,
    );
    assert_eq!(fee, 25);
    assert_eq!(available(&svm, &alice), 7_500);
    assert_eq!(pending(&svm, &bob), 2_475);
    assert_eq!(available(&svm, &bob), 0);
    let raw = svm.get_account(&bob.account).unwrap();
    let state = StateWithExtensions::<Account>::unpack(&raw.data).unwrap();
    let withheld: ElGamalCiphertext = state
        .get_extension::<ConfidentialTransferFeeAmount>()
        .unwrap()
        .withheld_amount
        .try_into()
        .unwrap();
    assert_eq!(fee_key.secret().decrypt_u32(&withheld), Some(25));
    // Cannot generate a withdrawal from pending funds: available is still zero.
    let before = read_ct(&svm, &bob.account);
    assert!(proofgen::withdraw::withdraw_proof_data(
        &before.available_balance.try_into().unwrap(),
        0,
        1_000,
        &bob.elgamal
    )
    .is_err());
    // This helper applies pending, re-reads state, proves, and withdraws.
    withdraw(&mut svm, &payer, &mint.pubkey(), &bob, &bob_owner, 1_000);
    assert_eq!(pending(&svm, &bob), 0);
    assert_eq!(available(&svm, &bob), 1_475);
    assert_eq!(read_account(&svm, &bob.account).amount, 1_000);
    assert_eq!(7_500 + 1_475 + 1_000 + 25, 10_000); // Conservation including fees.
}
