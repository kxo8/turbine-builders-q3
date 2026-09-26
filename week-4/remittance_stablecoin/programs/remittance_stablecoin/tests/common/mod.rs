#![allow(dead_code)]
pub mod confidential;
use anchor_lang::{
    prelude::Pubkey,
    solana_program::{instruction::Instruction, system_program},
    InstructionData, ToAccountMetas,
};
use litesvm::LiteSVM;
use remittance_stablecoin::{accounts, instruction, ID};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_signer::Signer;
use solana_transaction::Transaction;
use t22new::{
    extension::{
        transfer_fee::TransferFeeConfig, BaseStateWithExtensions, ExtensionType,
        StateWithExtensions,
    },
    state::{Account, Mint},
};
use zk::encryption::elgamal::ElGamalKeypair;
pub const TOKEN: Pubkey = t22new::ID;

pub fn setup() -> (LiteSVM, Keypair) {
    let mut svm = LiteSVM::new();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 100_000_000_000).unwrap();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/deploy/remittance_stablecoin.so");
    assert!(
        path.exists(),
        "Run cargo build-sbf first: {}",
        path.display()
    );
    svm.add_program_from_file(ID, path).unwrap();
    (svm, payer)
}
pub fn transaction(
    svm: &mut LiteSVM,
    payer: &Keypair,
    ixs: &[Instruction],
    extra: &[&Keypair],
) -> Transaction {
    // Distinct blockhash for repeated instructions, including negative-test retries.
    svm.expire_blockhash();
    let mut signers = vec![payer];
    for signer in extra {
        if !signers.iter().any(|s| s.pubkey() == signer.pubkey()) {
            signers.push(signer);
        }
    }
    let mut tx = Transaction::new_unsigned(Message::new(ixs, Some(&payer.pubkey())));
    tx.try_sign(&signers, svm.latest_blockhash()).unwrap();
    tx
}
pub fn send(svm: &mut LiteSVM, payer: &Keypair, ixs: &[Instruction], extra: &[&Keypair]) {
    let tx = transaction(svm, payer, ixs, extra);
    if let Err(e) = svm.send_transaction(tx) {
        panic!("transaction failed: {e:#?}");
    }
}
pub fn fails(
    svm: &mut LiteSVM,
    payer: &Keypair,
    ixs: &[Instruction],
    extra: &[&Keypair],
    reason: &str,
) {
    let tx = transaction(svm, payer, ixs, extra);
    let err = svm
        .send_transaction(tx)
        .expect_err("transaction unexpectedly succeeded");
    let text = format!("{err:#?}");
    use t22new::error::TokenError;
    let code = match reason {
        "OwnerMismatch" => Some(TokenError::OwnerMismatch as u32),
        "AccountFrozen" => Some(TokenError::AccountFrozen as u32),
        "FeeMismatch" => Some(TokenError::FeeMismatch as u32),
        "InsufficientFunds" => Some(TokenError::InsufficientFunds as u32),
        "ConfidentialTransferAccountNotApproved" => {
            Some(TokenError::ConfidentialTransferAccountNotApproved as u32)
        }
        _ => None,
    };
    let matches = code.map_or_else(
        || text.contains(reason),
        |n| format!("{:?}", err.err).contains(&format!("Custom({n})")),
    );
    assert!(matches, "expected {reason}, got {text}");
}
pub fn create_mint(
    svm: &mut LiteSVM,
    payer: &Keypair,
    fee_key: Option<&ElGamalKeypair>,
) -> Keypair {
    let mint = Keypair::new();
    let data = match fee_key {
        None => instruction::Initialize {}.data(),
        Some(key) => instruction::ReissueMint {
            withdraw_withheld_authority_elgamal_pubkey: key.pubkey().into(),
        }
        .data(),
    };
    send(
        svm,
        payer,
        &[Instruction {
            program_id: ID,
            accounts: accounts::CreateMint {
                payer: payer.pubkey(),
                mint: mint.pubkey(),
                token_program: TOKEN,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
            data,
        }],
        &[&mint],
    );
    mint
}
pub fn create_holder(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, owner: &Pubkey) -> Pubkey {
    let account = Keypair::new();
    let mint_account = svm.get_account(mint).unwrap();
    assert_eq!(mint_account.owner, TOKEN);
    let mint_state = StateWithExtensions::<Mint>::unpack(&mint_account.data).unwrap();
    let mut required = Vec::new();
    t22new::extension::account_len::try_for_each_required_init_account_extension(
        mint_state.get_tlv_data(),
        |extension| {
            required.push(extension);
            Ok(())
        },
    )
    .unwrap();
    let size = ExtensionType::try_calculate_account_len::<Account>(&required).unwrap();
    send(
        svm,
        payer,
        &[
            solana_system_interface::instruction::create_account(
                &payer.pubkey(),
                &account.pubkey(),
                svm.minimum_balance_for_rent_exemption(size),
                size as u64,
                &TOKEN,
            ),
            t22new::instruction::initialize_account3(&TOKEN, &account.pubkey(), mint, owner)
                .unwrap(),
        ],
        &[&account],
    );
    assert_eq!(svm.get_account(&account.pubkey()).unwrap().data.len(), size);
    account.pubkey()
}
pub fn account_ix(mint: Pubkey, account: Pubkey, authority: Pubkey, data: Vec<u8>) -> Instruction {
    Instruction {
        program_id: ID,
        accounts: accounts::AccountAuthority {
            token_account: account,
            mint,
            authority,
            token_program: TOKEN,
        }
        .to_account_metas(None),
        data,
    }
}
pub fn thaw(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, account: &Pubkey) {
    send(
        svm,
        payer,
        &[account_ix(
            *mint,
            *account,
            payer.pubkey(),
            instruction::ThawAfterKyc {}.data(),
        )],
        &[],
    );
}
pub fn mint_to(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, account: &Pubkey, amount: u64) {
    send(
        svm,
        payer,
        &[
            t22new::instruction::mint_to(&TOKEN, mint, account, &payer.pubkey(), &[], amount)
                .unwrap(),
        ],
        &[],
    );
}
pub fn read_account(svm: &LiteSVM, account: &Pubkey) -> Account {
    let raw = svm.get_account(account).unwrap();
    assert_eq!(raw.owner, TOKEN);
    StateWithExtensions::<Account>::unpack(&raw.data)
        .unwrap()
        .base
}
pub fn fee_config(svm: &LiteSVM, mint: &Pubkey) -> TransferFeeConfig {
    let raw = svm.get_account(mint).unwrap();
    assert_eq!(raw.owner, TOKEN);
    *StateWithExtensions::<Mint>::unpack(&raw.data)
        .unwrap()
        .get_extension::<TransferFeeConfig>()
        .unwrap()
}
pub fn transfer_ix(
    mint: Pubkey,
    source: Pubkey,
    destination: Pubkey,
    authority: Pubkey,
    amount: u64,
    seize: bool,
) -> Instruction {
    Instruction {
        program_id: ID,
        accounts: accounts::TransferTokens {
            source,
            mint,
            destination,
            authority,
            token_program: TOKEN,
        }
        .to_account_metas(None),
        data: if seize {
            instruction::SeizePublicTokens { amount }.data()
        } else {
            instruction::TransferWithFee { amount }.data()
        },
    }
}
pub fn close_ix(mint: Pubkey, destination: Pubkey, authority: Pubkey) -> Instruction {
    Instruction {
        program_id: ID,
        accounts: accounts::CloseMint {
            mint,
            destination,
            authority,
            token_program: TOKEN,
        }
        .to_account_metas(None),
        data: instruction::CloseMint {}.data(),
    }
}
