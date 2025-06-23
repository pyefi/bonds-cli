use anchor_client::{Client, Cluster};
use anyhow::{anyhow, Result};
use pye_core_cpi::pye_core::accounts::SoloValidatorBond;
use pye_core_cpi::pye_core::ID as PYE_BONDS_ID;
use solana_sdk::message::Message;
use solana_sdk::signer::keypair::read_keypair_file;
use solana_sdk::signer::Signer;
use solana_sdk::system_instruction::transfer;
use solana_sdk::transaction::Transaction;
use solana_sdk::{commitment_config::CommitmentConfig, pubkey::Pubkey};
use std::sync::Arc;

pub async fn transfer_excess_rewards(
    payer_file_path: String,
    cluster: Cluster,
    bond_pubkey: &Pubkey,
    _bond: &SoloValidatorBond,
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

    let mut transfer_ixs = vec![];

    // Transfer excess rewards from payer to stake account.
    let transfer_ix = transfer(&payer_pubkey, bond_pubkey, excess_rewards);
    transfer_ixs.push(transfer_ix);

    let message = Message::new(&[transfer_ixs].concat(), Some(&payer_pubkey));

    let tx = Transaction::new(&[payer], message, recent_blockhash);
    let sig = program
        .rpc()
        .send_and_confirm_transaction_with_spinner(&tx)
        .await
        .map_err(|e| anyhow!("Failed to send and confirm transaction: {}", e))?;
    println!("Transaction Sent: {}\n", sig);

    Ok(())
}
