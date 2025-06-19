use crate::accounts::fetch_solo_validator_bond;
use crate::active_stake::fetch_bond_active_stake;
use crate::metrics_helpers::*;
use crate::rewards::block_rewards::calculate_excess_block_reward;
use crate::rewards::inflation_rewards::calculate_excess_inflation_reward;
use crate::rewards::mev_rewards::{calculate_excess_mev_reward, fetch_and_filter_mev_data};
use crate::transactions::transfer_excess_rewards_with_delegate_tips;
use anchor_client::Cluster;
use anyhow::{anyhow, Result};
use dialoguer::Confirm;
use log::info;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_metrics::{datapoint_info, flush};
use solana_sdk::{commitment_config::CommitmentConfig, pubkey::Pubkey};
use std::str::FromStr;

pub struct TransferExcessRewardsArgs {
    pub rpc: String,
    pub payer_file_path: String,
    pub bond: String,
    pub concurrency: usize,
    pub dry_run: bool,
}

pub async fn handle_transfer_excess_rewards(args: TransferExcessRewardsArgs) -> Result<()> {
    let client = RpcClient::new_with_commitment(args.rpc.clone(), CommitmentConfig::confirmed());
    let bond_pubkey = Pubkey::from_str(&args.bond).map_err(|e| anyhow!("Invalid Bond: {}", e))?;

    // Fetch RewardCommissions configured on SoloValidatorBond.
    let bond = fetch_solo_validator_bond(&client, &bond_pubkey).await?;
    let reward_commissions = bond.reward_commissions.clone();
    info!("Current: {:?}", reward_commissions);

    // Fetch the current Solana Network epoch.
    let epoch_info = client.get_epoch_info().await?;
    let current_epoch = epoch_info.epoch;
    let target_epoch = current_epoch - 1;
    println!("Current epoch: {}\n", current_epoch);
    log_reward_commissions(target_epoch, &bond_pubkey, &reward_commissions);

    // Fetch info about MEV rewards for target epoch from Jito's API.
    let mev_data = fetch_and_filter_mev_data(&bond.validator_vote_account, target_epoch).await?;
    log_validator_mev_data(target_epoch, &mev_data);

    // Fetch the SoloValidatorBond's active stake during target epoch.
    let bond_active_stake = fetch_bond_active_stake(
        &client,
        &bond.stake_account,
        &bond.transient_stake_account,
        target_epoch,
    )
    .await?;

    // Calculate the excess inflation reward to be refunded by validator to SoloValidatorBond.
    let excess_inflation_reward = calculate_excess_inflation_reward(
        &client,
        &bond.stake_account,
        &bond.transient_stake_account,
        target_epoch,
        &reward_commissions,
    )
    .await;

    // Calculate the excess MEV reward to be refunded by validator to SoloValidatorBond.
    let excess_mev_commission =
        calculate_excess_mev_reward(&mev_data, bond_active_stake, &reward_commissions);

    // Calculate the excess block reward to be refunded by validator to SoloValidatorBond.
    let excess_block_commission = calculate_excess_block_reward(
        &client,
        &bond.validator_vote_account,
        &epoch_info,
        bond_active_stake,
        mev_data.active_stake,
        &reward_commissions,
        args.concurrency,
    )
    .await?;

    let excess_rewards = excess_inflation_reward + excess_block_commission + excess_mev_commission;
    println!("Total Excess Rewards: {}\n", excess_rewards);

    datapoint_info!(
        "excess_reward",
        (
            "vote_pubkey",
            bond.validator_vote_account.to_string(),
            String
        ),
        ("epoch", target_epoch.to_string(), String),
        ("bond", bond_pubkey.to_string(), String),
        ("bond_active_stake", bond_active_stake as i64, i64),
        ("excess_inflation_rewards", excess_inflation_reward, i64),
        ("excess_mev_rewards", excess_mev_commission, i64),
        ("excess_block_rewards", excess_block_commission, i64),
        ("total_excess_rewards", excess_rewards, i64),
    );
    flush();

    if excess_rewards <= 0 {
        println!(
            "No excess rewards to transfer to SoloValidatorBond for epoch {}\n",
            target_epoch
        );
        return Ok(());
    }

    if args.dry_run {
        println!("Dry run complete");
        return Ok(());
    }

    if Confirm::new()
        .with_prompt(format!(
            "Transfer {} lamports in excess rewards to SoloValidatorBond at {}?",
            excess_rewards, bond_pubkey
        ))
        .interact()?
    {
        let cluster = Cluster::Custom(args.rpc.clone(), args.rpc.replace("http", "ws"));
        transfer_excess_rewards_with_delegate_tips(
            args.payer_file_path,
            cluster,
            &bond_pubkey,
            &bond,
            u64::try_from(excess_rewards)?,
        )
        .await
        .map_err(|e| anyhow!("Failed to transfer excess rewards: {}", e))
    } else {
        println!("Aborted: user declined to transfer excess rewards.");
        Ok(())
    }
}
