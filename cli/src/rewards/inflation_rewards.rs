use anyhow::{anyhow, Result};
use log::{error, info};
use pye_core_cpi::pye_core::types::RewardCommissions;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;

/// Computes the excess inflation commission owed to bond holders.
///
/// # Arguments
/// - `amount_after_commission`: actual reward received after commission
/// - `commission_rate`: commission rate reported by validator (0-100)
/// - `expected_bps`: expected commission rate in basis points (0-10_000)
pub fn compute_excess_inflation_commission(
    amount_after_commission: u64,
    commission_rate: u64,
    expected_bps: u16,
) -> i64 {
    let total_reward = amount_after_commission * 100 / (100 - commission_rate);
    let actual_commission = (total_reward - amount_after_commission) as i64;
    let expected_commission = (total_reward * expected_bps as u64 / 10000) as i64;
    actual_commission - expected_commission
}

async fn get_excess_inflation_reward(
    client: &RpcClient,
    address: &Pubkey,
    target_epoch: u64,
    reward_commissions: &RewardCommissions,
) -> Result<i64> {
    let inflation_rewards = client
        .get_inflation_reward(&[*address], Some(target_epoch))
        .await
        .map_err(|e| anyhow!("Failed to fetch inflation reward: {}", e))?;

    if inflation_rewards.is_empty() {
        return Err(anyhow!("No inflation rewards found for {}", address));
    }

    if let Some(reward) = &inflation_rewards[0] {
        let commission_rate = u64::from(
            reward
                .commission
                .ok_or_else(|| anyhow!("Commission data missing for {}", address))?,
        );
        let excess = compute_excess_inflation_commission(
            reward.amount,
            commission_rate,
            reward_commissions.inflation_bps,
        );
        Ok(excess)
    } else {
        // This is the case for stake accounts that are activating
        return Ok(0);
    }
}

pub async fn calculate_excess_inflation_reward(
    client: &RpcClient,
    stake_pubkey: &Pubkey,
    transient_pubkey: &Pubkey,
    target_epoch: u64,
    reward_commissions: &RewardCommissions,
) -> i64 {
    let excess_stake_inflation_commission =
        match get_excess_inflation_reward(client, stake_pubkey, target_epoch, reward_commissions)
            .await
        {
            Ok(amount) => {
                info!("Excess Stake Account Inflation Commission: {:?}", amount);
                amount
            }
            Err(e) => {
                error!("Error for stake account: {}", e);
                0 // Return 0 for stake account on error.
            }
        };

    let excess_transient_inflation_commission = if !transient_pubkey.eq(&Pubkey::default()) {
        match get_excess_inflation_reward(
            client,
            transient_pubkey,
            target_epoch,
            reward_commissions,
        )
        .await
        {
            Ok(amount) => {
                info!(
                    "Excess Transient Account Inflation Commission: {:?}\n",
                    amount
                );
                amount
            }
            Err(e) => {
                error!("Error for transient account: {}\n", e);
                0 // Return 0 for transient account on error
            }
        }
    } else {
        0 // No transient account specified
    };

    // Commissions in excess of stated rate taken by validator. If negative,
    // this is the amount of commission owned to validator.
    excess_stake_inflation_commission + excess_transient_inflation_commission
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_excess_inflation_commission_exact() {
        // Validator took 10% commission, expected was also 10%
        let result = compute_excess_inflation_commission(900_000, 10, 1000);
        assert_eq!(result, 0); // no excess
    }

    #[test]
    fn test_excess_inflation_commission_took_more() {
        // Validator took 12%, expected 10%
        let result = compute_excess_inflation_commission(880_000, 12, 1000);
        assert_eq!(result, 20_000);
    }

    #[test]
    fn test_excess_inflation_commission_took_less() {
        // Validator took 8%, expected 10%
        let result = compute_excess_inflation_commission(920_000, 8, 1000);
        assert_eq!(result, -20_000);
    }
}
