use anchor_client::solana_sdk::instruction::AccountMeta;
use anchor_client::{Client, Cluster};
use anyhow::{anyhow, Result};
use pye_core_cpi::pye_core::client::accounts::SoloValidatorDelegateTips;
use pye_core_cpi::pye_core::accounts::SoloValidatorBond;
use pye_core_cpi::pye_core::ID as PYE_BONDS_ID;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::keypair::read_keypair_file;
use solana_sdk::signer::Signer;
use solana_sdk::stake;
use solana_sdk::system_instruction::transfer;
use solana_sdk::system_program;
use solana_sdk::sysvar;
use solana_sdk::transaction::Transaction;
use solana_sdk::{commitment_config::CommitmentConfig, pubkey::Pubkey};
use std::sync::Arc;

pub async fn transfer_excess_rewards_with_delegate_tips(
    payer_file_path: String,
    cluster: Cluster,
    bond_pubkey: &Pubkey,
    bond: &SoloValidatorBond,
    excess_rewards: u64,
) -> Result<()> {
    if excess_rewards == 0 {
        return Err(anyhow!("No excess rewards to transfer"));
    }

    let payer = Arc::new(read_keypair_file(&payer_file_path).map_err(|e| {
        anyhow!(
            "Failed to read payer keypair from {}: {}",
            payer_file_path,
            e
        )
    })?);
    let payer_pubkey = payer.pubkey();
    println!("Payer: {:?}", payer_pubkey);

    let client =
        Client::new_with_options(cluster, Arc::clone(&payer), CommitmentConfig::processed());

    // TODO: check balance and send notification if not enough balance

    let program = client.program(PYE_BONDS_ID)?;
    let (recent_blockhash, _last_valid_block_height) = program
        .rpc()
        .get_latest_blockhash_with_commitment(CommitmentConfig::finalized())
        .await
        .map_err(|e| anyhow!("Failed to fetch latest blockhash: {}", e))?;

    let mut signing_keypairs = vec![&*payer];
    let mut transfer_ixs = vec![];

    // Generate a new transient stake account keypair it doesn't exist.
    let mut transient_pubkey = bond.transient_stake_account;
    let mut transient_is_signer = false;

    let new_transient_keypair = Keypair::new();
    if transient_pubkey.eq(&Pubkey::default()) {
        transient_pubkey = new_transient_keypair.pubkey();
        signing_keypairs.push(&new_transient_keypair);
        transient_is_signer = true;
    }

    let (global_settings, _) =
        Pubkey::find_program_address(&["global_settings".as_bytes()], &program.id());

    // Transfer excess rewards from payer to stake account.
    let transfer_ix = transfer(&payer_pubkey, &bond.stake_account, excess_rewards);
    transfer_ixs.push(transfer_ix);

    // Initiate delegation of excess rewards in stake account.
    let delegate_ixs = program
        .request()
        .accounts(SoloValidatorDelegateTips {
            bond: *bond_pubkey,
            validator_vote_account: bond.validator_vote_account,
            stake_account: bond.stake_account,
            global_settings,
            clock: sysvar::clock::ID,
            stake_program: stake::program::ID,
            stake_history: sysvar::stake_history::ID,
            stake_config: stake::config::ID,
            rent: sysvar::rent::ID,
            system_program: system_program::ID,
        })
        .accounts(vec![
            AccountMeta::new(transient_pubkey, transient_is_signer),
            AccountMeta::new_readonly(PYE_BONDS_ID, false),
        ])
        .args(pye_core_cpi::pye_core::client::args::SoloValidatorDelegateTips {})
        .instructions()
        .map_err(|e| {
            anyhow!(
                "Failed to create SoloValidatorDelegateTips instructions: {}",
                e
            )
        })?;

    let tx = Transaction::new_signed_with_payer(
        &[transfer_ixs, delegate_ixs].concat(),
        Some(&payer_pubkey),
        &signing_keypairs,
        recent_blockhash,
    );
    let sig = program
        .rpc()
        .send_and_confirm_transaction_with_spinner(&tx)
        .await
        .map_err(|e| anyhow!("Failed to send and confirm transaction: {}", e))?;
    println!("Transaction Sent: {}\n", sig);

    Ok(())
}
