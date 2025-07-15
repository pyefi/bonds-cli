use crate::rpc_utils::{self, PriorityFeeKeeperError};
use anyhow::{anyhow, Result};
use futures::stream::{self, StreamExt};
use log::info;
use pye_core_cpi::pye_core::types::RewardCommissions;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcLeaderScheduleConfig;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::epoch_info::EpochInfo;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::reward_type::RewardType;
use std::sync::Arc;
use std::time::Duration;

/// Computes the excess block commission owed to bond holders.
///
/// # Arguments
/// - `total_block_reward`: total block reward fees earned by the validator in the epoch
/// - `bond_active_stake`: bond's active stake at epoch (subset of validator_active_stake)
/// - `validator_active_stake`: validator's total active stake at epoch
/// - `block_rewards_bps`: expected commission rate in basis points (0–10000)
pub fn compute_excess_block_commission(
    total_block_reward: u64,
    bond_active_stake: u64,
    validator_active_stake: u64,
    block_rewards_bps: u16,
) -> i64 {
    if validator_active_stake == 0 {
        return 0;
    }

    let bond_block_reward = ((u128::from(bond_active_stake) * u128::from(total_block_reward))
        / u128::from(validator_active_stake)) as u64;

    let excess_block_commission =
        (bond_block_reward * u64::from(10000 - block_rewards_bps) / 10000) as i64;

    excess_block_commission
}

/// Uses and RPC client to fetch the block rewards for a given validator
pub async fn calculate_block_rewards(
    rpc: &RpcClient,
    vote_pubkey: &Pubkey,
    epoch_info: &EpochInfo,
    concurrency: usize,
) -> Result<u64> {
    let vote_str = vote_pubkey.to_string();
    let vote_accounts = rpc
        .get_vote_accounts()
        .await
        .map_err(|e| anyhow!("Failed to fetch vote accounts: {}", e))?;

    let node_identity = vote_accounts
        .current
        .into_iter()
        .find(|va| va.vote_pubkey == vote_str)
        .ok_or_else(|| anyhow!("Validator with vote pubkey {} not found", vote_str))?
        .node_pubkey;

    // 1) Get slot of first block in previous epoch
    let first = epoch_info
        .absolute_slot
        .saturating_sub(epoch_info.slot_index)
        .saturating_sub(epoch_info.slots_in_epoch);

    // 2) Fetch the leader schedule for specified node.
    let schedule = rpc
        .get_leader_schedule_with_config(
            Some(first),
            RpcLeaderScheduleConfig {
                identity: Some(node_identity.clone()),
                commitment: Some(CommitmentConfig::finalized()),
            },
        )
        .await
        .map_err(|e| anyhow!("Failed to fetch leader schedule: {}", e))?
        .ok_or_else(|| anyhow!("Leader schedule not found for node {}", node_identity))?;

    let indices = schedule
        .get(&node_identity)
        .cloned()
        .ok_or(anyhow!("Err looking up leader schedule"))?;
    let slots: Vec<u64> = indices.into_iter().map(|i| first + i as u64).collect();

    // 3) Fetch each block that the leader produced to calculate total block rewards earned.
    let slot_history = Arc::new(crate::accounts::fetch_slot_history(rpc).await?);

    // TODO: Replace with a batched JSON-RPC call to reduce HTTP overhead.
    info!(
        "Fetching {} Blocks Produced in Epoch {}",
        slots.len(),
        epoch_info.epoch - 1,
    );
    let total_fees: u64 = stream::iter(slots)
        .map(|slot| {
            let node_identity = node_identity.clone();
            let slot_history = Arc::clone(&slot_history);
            async move {
                let mut attempts: u8 = 0;
                loop {
                    attempts += 1;
                    match rpc_utils::get_block(rpc, slot, &slot_history).await {
                        Ok(block) => {
                            let mut total = 0;
                            if let Some(rewards) = block.rewards {
                                for r in rewards {
                                    if r.pubkey == node_identity {
                                        if let Some(RewardType::Fee) = r.reward_type {
                                            total += r.lamports as u64;
                                        }
                                    }
                                }
                            }
                            return Ok(total);
                        }
                        Err(e) => {
                            match e {
                                PriorityFeeKeeperError::SkippedBlock => {
                                    return Err(anyhow!(
                                        "Failed to fetch block at slot {}: {}",
                                        slot,
                                        e
                                    ));
                                }
                                _ => {
                                    if attempts >= 5 {
                                        return Err(anyhow!(
                                            "Failed to fetch block at slot {}: {}",
                                            slot,
                                            e
                                        ));
                                    } else {
                                        // sleep for 30min before trying this block again. Max wait time is currently 2.5 hours
                                        tokio::time::sleep(Duration::from_secs(1800)).await;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })
        .buffer_unordered(concurrency)
        .fold(Ok(0u64), |acc, fee_result| async move {
            match (acc, fee_result) {
                (Ok(acc), Ok(fee)) => Ok(acc + fee),
                (Err(e), _) | (_, Err(e)) => Err(e),
            }
        })
        .await?;

    Ok(total_fees)
}

pub async fn calculate_excess_block_reward(
    client: &RpcClient,
    vote_pubkey: &Pubkey,
    epoch_info: &EpochInfo,
    bond_active_stake: u64,
    validator_active_stake: u64,
    reward_commissions: &RewardCommissions,
    concurrency: usize,
) -> Result<i64> {
    let total_block_reward: std::result::Result<u64, anyhow::Error> =
        calculate_block_rewards(client, vote_pubkey, epoch_info, concurrency).await;

    if validator_active_stake == 0 {
        info!("No excess block reward when validator active stake is 0");
        return Ok(0);
    }

    match total_block_reward {
        Ok(amount) => {
            let excess_block_commission = compute_excess_block_commission(
                amount,
                bond_active_stake,
                validator_active_stake,
                reward_commissions.block_rewards_bps,
            );
            info!(
                "Total Block Reward: {}, Excess Block Commission: {}\n",
                amount, excess_block_commission
            );
            Ok(excess_block_commission)
        }
        Err(e) => {
            info!(
                "Error fetching block reward: {}. Assuming no block reward earned.\n",
                e
            );
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partial_bond_stake() {
        let result = compute_excess_block_commission(1_000_000, 500_000, 1_000_000, 5000);
        assert_eq!(result, 250_000);
    }

    #[test]
    fn test_zero_validator_stake() {
        let result = compute_excess_block_commission(1_000_000, 500_000, 0, 5000);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_zero_bond_stake() {
        let result = compute_excess_block_commission(1_000_000, 0, 0, 5000);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_fully_bond_stake() {
        let result = compute_excess_block_commission(1_000_000, 1_000_000, 1_000_000, 0);
        assert_eq!(result, 1_000_000);
    }

    #[test]
    fn test_fully_bond_stake_with_commission() {
        let result = compute_excess_block_commission(1_000_000, 1_000_000, 1_000_000, 2000);
        assert_eq!(result, 800_000);
    }
}
