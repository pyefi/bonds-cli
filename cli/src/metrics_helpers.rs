use pye_core_cpi::pye_core::types::RewardCommissions;
use solana_metrics::datapoint_info;
use solana_sdk::pubkey::Pubkey;

use crate::rewards::mev_rewards::ValidatorInfo;

pub fn log_reward_commissions(
    target_epoch: u64,
    bond_pubkey: &Pubkey,
    reward_commissions: &RewardCommissions,
) {
    datapoint_info!(
        "reward_commissions",
        ("epoch", target_epoch.to_string(), String),
        ("bond", bond_pubkey.to_string(), String),
        (
            "inflation_bps",
            reward_commissions.inflation_bps as i64,
            i64
        ),
        ("mev_tips_bps", reward_commissions.mev_tips_bps as i64, i64),
        (
            "block_rewards_bps",
            reward_commissions.block_rewards_bps as i64,
            i64
        ),
    );
}

pub fn log_validator_mev_data(target_epoch: u64, mev_data: &ValidatorInfo) {
    datapoint_info!(
        "validator_mev_data",
        ("epoch", target_epoch.to_string(), String),
        ("vote_account", mev_data.vote_account, String),
        (
            "mev_commission_bps",
            mev_data.mev_commission_bps as i64,
            i64
        ),
        ("mev_rewards", mev_data.mev_rewards as i64, i64),
        ("active_stake", mev_data.active_stake as i64, i64),
        ("running_jito", mev_data.running_jito, bool),
    );
}
