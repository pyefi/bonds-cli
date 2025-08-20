use anyhow::{anyhow, Result};
use log::{info, warn};
use regex::Regex;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_request::RpcError;
use solana_client::rpc_response::StakeActivationState;
use solana_sdk::account::{Account, ReadableAccount};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::stake::state::StakeStateV2;
use solana_stake_program::stake_state::StakeActivationStatus;

#[derive(Debug)]
pub struct StakeActivation {
    pub state: StakeActivationState,
    pub active: u64,
    pub inactive: u64,
}

async fn fetch_stake_for_epoch(
    client: &RpcClient,
    stake_account: &Account,
    stake_state: &StakeStateV2,
    target_epoch: u64,
) -> Result<StakeActivation> {
    let delegation = stake_state
        .delegation()
        .ok_or(anyhow!("Stake is not delegated"))?;
    let rent_exempt_reserve = stake_state
        .meta()
        .ok_or(anyhow!("No rent exempt reserve data for stake found"))?
        .rent_exempt_reserve;
    let stake_history = crate::accounts::fetch_stake_history(client).await?;
    let StakeActivationStatus {
        effective,
        activating,
        deactivating,
    } = delegation.stake_activating_and_deactivating(target_epoch, &stake_history, None);
    let stake_activation_state = if deactivating > 0 {
        StakeActivationState::Deactivating
    } else if activating > 0 {
        StakeActivationState::Activating
    } else if effective > 0 {
        StakeActivationState::Active
    } else {
        StakeActivationState::Inactive
    };
    let inactive = stake_account
        .lamports()
        .saturating_sub(effective)
        .saturating_sub(rent_exempt_reserve);

    Ok(StakeActivation {
        state: stake_activation_state,
        active: effective,
        inactive,
    })
}

pub async fn fetch_bond_active_stake(
    client: &RpcClient,
    stake_account_key: &Pubkey,
    transient_stake_account_key: &Pubkey,
    target_epoch: u64,
    current_epoch: u64,
) -> Result<u64> {
    if target_epoch != current_epoch - 1 {
        return Err(anyhow!("Unsupported target epoch delta"));
    }
    let maybe_stake_account = &client.get_account(stake_account_key).await;
    let stake_account = match maybe_stake_account {
        Ok(account) => account,
        Err(err) => match &err.kind {
            solana_client::client_error::ClientErrorKind::RpcError(rpc_error) => match rpc_error {
                RpcError::RpcResponseError {
                    code: _,
                    message,
                    data: _,
                } => {
                    // SoloValidatorBond's initialize stake accounts on the first deposit. So in
                    // the case where a bond was created, but no deposits were made, the RPC will
                    // error with account not found. In this case, we short circuit and return 0
                    // as the active stake.
                    let account_not_found_match = Regex::new(r"^AccountNotFound").unwrap();
                    if account_not_found_match.is_match(&message) {
                        warn!("Fetch stake account {} error {:?}", stake_account_key, err);
                        return Ok(0);
                    } else {
                        return Err(anyhow!("Failed to fetch StakeAccount: {}", err));
                    }
                }
                _ => {
                    return Err(anyhow!("Failed to fetch StakeAccount: {}", err));
                }
            },
            _ => {
                return Err(anyhow!("Failed to fetch StakeAccount: {}", err));
            }
        },
    };
    let stake_state = &stake_account.deserialize_data::<StakeStateV2>()?;
    // Fetch inflation rewards for the target epoch
    let maybe_inflation_rewards = &client
        .get_inflation_reward(&[*stake_account_key], Some(target_epoch))
        .await?;
    let (inflation_rewards, post_balance) = maybe_inflation_rewards[0]
        .as_ref()
        .map(|x| (x.amount, x.post_balance))
        .unwrap_or((0, 0));
    let active_stake_for_current_epoch =
        fetch_stake_for_epoch(client, stake_account, stake_state, target_epoch).await?;
    info!(
        "Current Stake Account: {:?}",
        active_stake_for_current_epoch
    );
    let mut bond_active_stake = if active_stake_for_current_epoch.active >= inflation_rewards {
        active_stake_for_current_epoch.active - inflation_rewards
    } else {
        //  If the account was decativated or merged in, then the current active amount can be 0.
        //  This is used to determine the base MEV earned by the bond (since we only have total
        //  for the validator), the max MEV, and the max block rewards. If this number is higher
        //  because it includes additional lamports than it makes
        //.    A) makes base and expected MEV look higher, which should be proportional and net out.
        //.    B) makes expected block rewards highe, which is in favor of the stakers
        post_balance - inflation_rewards
    };
    info!(
        "Active stake for epoch {}: {}",
        target_epoch, bond_active_stake
    );

    if !transient_stake_account_key.eq(&Pubkey::default()) {
        let transient_account = &client
            .get_account(&transient_stake_account_key)
            .await
            .map_err(|e| anyhow!("Failed to fetch Transient StakeAccount: {}", e))?;
        let transient_state = &transient_account.deserialize_data::<StakeStateV2>()?;
        let transient_amount =
            fetch_stake_for_epoch(client, transient_account, transient_state, target_epoch).await?;
        let maybe_inflation_rewards = &client
            .get_inflation_reward(&[*stake_account_key], Some(target_epoch))
            .await?;
        let (inflation_rewards, post_balance) = maybe_inflation_rewards[0]
            .as_ref()
            .map(|x| (x.amount, x.post_balance))
            .unwrap_or((0, 0));
        let transient_stake_at_target_epoch = if transient_amount.active >= inflation_rewards {
            transient_amount.active - inflation_rewards
        } else {
            //  If the account was decativated or merged in, then the current active amount can be 0.
            //  This is used to determine the base MEV earned by the bond (since we only have total
            //  for the validator), the max MEV, and the max block rewards. If this number is higher
            //  because it includes additional lamports than it makes
            //.    A) makes base and expected MEV look higher, which should be proportional and net out.
            //.    B) makes expected block rewards highe, which is in favor of the stakers
            post_balance - inflation_rewards
        };
        info!(
            "Transient Stake Account: {:?}",
            transient_stake_at_target_epoch
        );
        info!(
            "Transient active stake for epoch {}: {}",
            target_epoch, transient_stake_at_target_epoch
        );
        bond_active_stake += transient_stake_at_target_epoch;
    }

    info!("Total Bond Active Stake: {}\n", bond_active_stake);
    Ok(bond_active_stake)
}
