use anchor_client::Cluster;
use anyhow::{anyhow, Result};
use clap::Parser;
use log::info;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_metrics::{datapoint_error, datapoint_info, flush};
use solana_sdk::pubkey::Pubkey;

use crate::{
    accounts::fetch_active_solo_validator_bonds_by_vote_key,
    active_stake::fetch_bond_active_stake,
    metrics_helpers::{log_reward_commissions, log_validator_mev_data},
    rewards::{
        block_rewards::{calculate_block_rewards, compute_excess_block_commission},
        inflation_rewards::calculate_excess_inflation_reward,
        mev_rewards::{calculate_excess_mev_reward, fetch_and_filter_mev_data},
    },
    rpc_utils::wait_for_next_epoch,
    transactions::transfer_excess_rewards,
};

#[derive(Clone, Debug, Parser)]
pub struct ValidatorBondManagerArgs {
    /// RPC Endpoint
    #[arg(
        short,
        long,
        env,
        default_value = "https://api.mainnet-beta.solana.com"
    )]
    rpc: String,
    /// The Pye program ID
    #[arg(long, default_value = "PYEQZ2qYHPQapnw8Ms8MSPMNzoq59NHHfNwAtuV26wx")]
    program_id: Pubkey,
    /// Validator's vote accoutn
    #[arg(short, long, env)]
    vote_pubkey: Pubkey,
    /// Path to payer keypair
    #[arg(short, long, env)]
    payer: String,
    /// Maximum RPC requests to send concurrently.
    #[arg(long, env, default_value = "50")]
    concurrency: usize,
    /// Dry mode to calculate excess rewards without transferring.
    #[arg(long, env)]
    dry_run: bool,
}

pub async fn handle_validator_bond_manager(args: ValidatorBondManagerArgs) -> Result<()> {
    let rpc_client =
        RpcClient::new_with_commitment(args.rpc.clone(), CommitmentConfig::confirmed());
    // TODO: Load all the bond accounts owned by the validator
    let mut current_epoch_info = match rpc_client.get_epoch_info().await {
        Ok(info) => info,
        Err(err) => {
            datapoint_error!(
                "handle_validator_bond_manager",
                ("error", err.to_string(), String),
            );
            return Err(anyhow!("Error getting epoch info: {:?}", err));
        }
    };
    loop {
        // Fetch bonds that are still active prior to waiting for the next epoch, to make sure we
        // don't miss any.
        let active_bonds = match fetch_active_solo_validator_bonds_by_vote_key(
            &rpc_client,
            &args.program_id,
            &args.vote_pubkey,
        )
        .await
        {
            Ok(bonds) => bonds,
            Err(err) => {
                datapoint_error!(
                    "handle_validator_bond_manager",
                    ("error", err.to_string(), String),
                );
                return Err(anyhow!("Error fetching active bonds: {:?}", err));
            }
        };
        // We block the flow until the next epoch
        current_epoch_info = wait_for_next_epoch(&rpc_client, current_epoch_info.epoch).await;
        info!("Current epoch: {}\n", current_epoch_info.epoch);
        let target_epoch = current_epoch_info.epoch - 1;

        // For all active bonds, log their commission structures
        active_bonds.iter().for_each(|(bond_pubkey, bond)| {
            log_reward_commissions(target_epoch, &bond_pubkey, &bond.reward_commissions)
        });

        // Load MEV data
        let mev_data = fetch_and_filter_mev_data(&args.vote_pubkey, target_epoch).await?;
        log_validator_mev_data(target_epoch, &mev_data);

        let validators_total_block_rewards = calculate_block_rewards(
            &rpc_client,
            &args.vote_pubkey,
            &current_epoch_info,
            args.concurrency,
        )
        .await?;

        // Note: could add concurrency in this loop
        // For each bond calculate the additional rewards required for each category
        for (bond_pubkey, bond) in active_bonds.into_iter() {
            // Fetch the SoloValidatorBond's active stake during target epoch.
            let bond_active_stake = fetch_bond_active_stake(
                &rpc_client,
                &bond.stake_account,
                &bond.transient_stake_account,
                target_epoch,
                current_epoch_info.epoch,
            )
            .await?;
            // Calculate the excess inflation reward to be refunded by validator to SoloValidatorBond.
            let excess_inflation_reward = calculate_excess_inflation_reward(
                &rpc_client,
                &bond.stake_account,
                &bond.transient_stake_account,
                target_epoch,
                &bond.reward_commissions,
            )
            .await;

            // Calculate the excess MEV reward to be refunded by validator to SoloValidatorBond.
            let excess_mev_commission =
                calculate_excess_mev_reward(&mev_data, bond_active_stake, &bond.reward_commissions);

            // Calculate the excess block reward to be funded by validator to SoloValidatorBond.
            let excess_block_commission = compute_excess_block_commission(
                validators_total_block_rewards,
                bond_active_stake,
                mev_data.active_stake,
                bond.reward_commissions.block_rewards_bps,
            );

            let excess_rewards =
                excess_inflation_reward + excess_block_commission + excess_mev_commission;
            info!(
                "Bond: {}\nSOL to transfer: {}\n\n",
                excess_rewards, excess_rewards
            );

            datapoint_info!(
                "excess_reward",
                ("vote_pubkey", args.vote_pubkey.to_string(), String),
                ("epoch", target_epoch.to_string(), String),
                ("bond", bond_pubkey.to_string(), String),
                ("bond_active_stake", bond_active_stake as i64, i64),
                ("excess_inflation_rewards", excess_inflation_reward, i64),
                ("excess_mev_rewards", excess_mev_commission, i64),
                ("excess_block_rewards", excess_block_commission, i64),
                ("total_excess_rewards", excess_rewards, i64),
            );

            // TODO: Make the actual SOL transfer if not a dry run
            if !args.dry_run {
                // transfer_excess_rewards_with_delegate_tips
                let cluster = Cluster::Custom(args.rpc.clone(), args.rpc.replace("http", "ws"));
                transfer_excess_rewards(
                    args.payer.clone(),
                    cluster,
                    &bond_pubkey,
                    &bond,
                    u64::try_from(excess_rewards)?,
                )
                .await
                .map_err(|e| anyhow!("Failed to transfer excess rewards: {}", e))?
            }
        }
        flush();
    }
}
