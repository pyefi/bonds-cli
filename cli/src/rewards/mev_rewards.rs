use std::time::Duration;

use anyhow::{anyhow, Result};
use pye_core_cpi::pye_core::types::RewardCommissions;
use reqwest::Client;
use serde::Deserialize;
use solana_sdk::pubkey::Pubkey;

#[derive(Clone, Deserialize, Debug)]
pub struct ValidatorInfo {
    pub vote_account: String,
    pub mev_commission_bps: u64,
    pub mev_rewards: u64,
    pub running_jito: bool,
    pub active_stake: u64,
}

#[derive(Deserialize, Debug)]
pub struct ValidatorsResponse {
    pub validators: Vec<ValidatorInfo>,
}

/// Computes the excess MEV commission owed to bond holders.
///
/// # Arguments
/// - `total_mev_rewards`: total MEV rewards earned by the validator in the epoch
/// - `bond_active_stake`: bond's active stake at epoch (subset of validator_active_stake)
/// - `validator_active_stake`: validator's total active stake at epoch
/// - `validator_mev_commission_bps`: actual commission rate taken by validator (0-10000)
/// - `expected_mev_commission_bps`: expected commission rate (0-10000)
pub fn compute_excess_mev_commission(
    total_mev_rewards: u64,
    bond_active_stake: u64,
    validator_active_stake: u64,
    validator_mev_commission_bps: u64,
    expected_mev_commission_bps: u16,
) -> i64 {
    if validator_active_stake == 0 {
        return 0;
    }

    let bond_mev_reward = ((u128::from(bond_active_stake) * u128::from(total_mev_rewards))
        / u128::from(validator_active_stake)) as u64;
    let mev_commission_taken = (bond_mev_reward * validator_mev_commission_bps / 10000) as i64;
    let expected_mev_commission =
        (bond_mev_reward * expected_mev_commission_bps as u64 / 10000) as i64;

    println!(
        "Total MEV Reward: {}, Bond's MEV Reward (incl. commission): {}",
        total_mev_rewards, bond_mev_reward,
    );
    println!(
        "MEV Commission Taken ({:.2}%): {}",
        validator_mev_commission_bps as f64 / 100.0,
        mev_commission_taken
    );
    println!(
        "Expected MEV Commission ({:.2}%): {}",
        expected_mev_commission_bps as f64 / 100.0,
        expected_mev_commission
    );

    mev_commission_taken - expected_mev_commission
}

pub async fn fetch_and_filter_mev_data(
    vote_pubkey: &Pubkey,
    target_epoch: u64,
) -> Result<ValidatorInfo> {
    let response = fetch_mev_with_retry(target_epoch, 12, Duration::from_secs(3600)).await?;
    filter_mev_data(response, vote_pubkey)
}

// REVIEW: When does MEV epoch data get uploaded to the API? If operators are waiting for epoch
// transition, there could be a race condition for MEV epoch data
pub async fn fetch_mev_data(target_epoch: u64) -> Result<ValidatorsResponse> {
    let http = Client::new();

    http.post("https://kobe.mainnet.jito.network/api/v1/validators")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "epoch": target_epoch }))
        .send()
        .await
        .map_err(|e| anyhow!("Failed to send request: {}", e))?
        .error_for_status()
        .map_err(|e| anyhow!("Server returned error status: {}", e))?
        .json::<ValidatorsResponse>()
        .await
        .map_err(|e| anyhow!("Failed to deserialize response: {}", e))
}

pub async fn fetch_mev_with_retry(
    target_epoch: u64,
    max_attempts: u64,
    duration: Duration,
) -> Result<ValidatorsResponse> {
    let mut attempt: u64 = 0;
    loop {
        match fetch_mev_data(target_epoch).await {
            Ok(res) => return Ok(res),
            Err(err) => {
                attempt += 1;
                if attempt >= max_attempts {
                    return Err(err);
                } else {
                    tokio::time::sleep(duration).await;
                }
            }
        }
    }
}

fn filter_mev_data(response: ValidatorsResponse, vote_pubkey: &Pubkey) -> Result<ValidatorInfo> {
    let vote_str = vote_pubkey.to_string();
    let validator = response
        .validators
        .into_iter()
        .find(|v| v.vote_account == vote_str);

    if let Some(info) = validator {
        Ok(info.clone())
    } else {
        Err(anyhow!(
            "Validator with vote account {} not found with Jito MEV API. Assuming that validator does not have MEV.\n",
            vote_str
        ))
    }
}

pub fn calculate_excess_mev_reward(
    mev_data: &ValidatorInfo,
    bond_active_stake: u64,
    reward_commissions: &RewardCommissions,
) -> i64 {
    if !mev_data.running_jito {
        // No MEV rewards if validator is not running Jito.
        return 0;
    }

    let excess_mev_commission = compute_excess_mev_commission(
        mev_data.mev_rewards,
        bond_active_stake,
        mev_data.active_stake,
        mev_data.mev_commission_bps,
        reward_commissions.mev_tips_bps,
    );
    println!("Excess MEV Commission: {}\n", excess_mev_commission);

    excess_mev_commission
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_mev_commission() {
        let result = compute_excess_mev_commission(1_000_000, 500_000, 1_000_000, 500, 500);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_validator_took_more_commission() {
        let result = compute_excess_mev_commission(1_000_000, 500_000, 1_000_000, 700, 500);
        assert_eq!(result, 10000);
    }

    #[test]
    fn test_validator_took_less_commission() {
        let result = compute_excess_mev_commission(1_000_000, 500_000, 1_000_000, 300, 500);
        assert_eq!(result, -10000);
    }

    #[test]
    fn test_validator_zero_stake() {
        let result = compute_excess_mev_commission(1_000_000, 500_000, 0, 500, 500);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_bond_zero_stake() {
        let result = compute_excess_mev_commission(1_000_000, 0, 1_000_000, 500, 500);
        assert_eq!(result, 0);
    }
}
