use std::time::Duration;

use log::{error, info};
use regex::Regex;
use solana_client::client_error::ClientErrorKind;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcBlockConfig;
use solana_client::{client_error::ClientError, rpc_request::RpcError};
use solana_commitment_config::CommitmentConfig;
use solana_sdk::epoch_info::EpochInfo;
use solana_sdk::slot_history;
use solana_sdk::sysvar::slot_history::SlotHistory;
use solana_transaction_status_client_types::{
    TransactionDetails, UiConfirmedBlock, UiTransactionEncoding,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum PriorityFeeKeeperError {
    #[error("SolanaClientError error: {0}")]
    SolanaClientError(#[from] ClientError),
    #[error(transparent)]
    RpcError(#[from] RpcError),
    #[error("No leader schedule for epoch found")]
    ErrorGettingLeaderSchedule,
    #[error("Block was skipped")]
    SkippedBlock,
    #[error("Vote key not found for identity {0}")]
    MissingVoteKey(String),
    #[error("Slot {0} not found. SlotHistory not up to date or slot in future")]
    SlotInFuture(u64),
    #[error("Slot {0} not found on RPC, but on SlotHistory sysvar")]
    InSlotHistoryNotOnRpc(u64),
}

// rpc_utils.rs
/// Wrapper on Solana RPC get_block, but propagates skipped blocks as PriorityFeeKeeperError
pub async fn get_block(
    client: &RpcClient,
    slot: u64,
    slot_history: &SlotHistory,
) -> Result<UiConfirmedBlock, PriorityFeeKeeperError> {
    let block_res = client
        .get_block_with_config(
            slot,
            RpcBlockConfig {
                encoding: Some(UiTransactionEncoding::Json),
                transaction_details: Some(TransactionDetails::None),
                rewards: Some(true),
                commitment: Some(CommitmentConfig::finalized()),
                max_supported_transaction_version: Some(0),
            },
        )
        .await;
    match block_res {
        Ok(block) => return Ok(block),
        Err(err) => match err.kind {
            ClientErrorKind::RpcError(client_rpc_err) => match client_rpc_err {
                RpcError::RpcResponseError {
                    code,
                    message,
                    data,
                } => {
                    // These slot skipped errors come from RpcCustomError::SlotSkipped or
                    //  RpcCustomError::LongTermStorageSlotSkipped and may not always mean
                    //  there is no block for a given slot. The additional context are:
                    //  "...or missing due to ledger jump to recent snapshot"
                    //  "...or missing in long-term storage"
                    // Meaning they can arise from RPC issues or lack of history (limit ledger
                    //  space, no big table) accesible  by an RPC. This is why we check
                    // SlotHistory and then follow up with redundant RPC checks.
                    let slot_skipped_regex = Regex::new(r"^Slot [\d]+ was skipped").unwrap();
                    if slot_skipped_regex.is_match(&message) {
                        match slot_history.check(slot) {
                            slot_history::Check::Future => {
                                return Err(PriorityFeeKeeperError::SlotInFuture(slot));
                            }
                            slot_history::Check::NotFound => {
                                return Err(PriorityFeeKeeperError::SkippedBlock);
                            }
                            slot_history::Check::TooOld | slot_history::Check::Found => {
                                return Err(PriorityFeeKeeperError::InSlotHistoryNotOnRpc(slot));
                            }
                        }
                    }
                    return Err(PriorityFeeKeeperError::RpcError(
                        RpcError::RpcResponseError {
                            code,
                            message,
                            data,
                        },
                    ));
                }
                _ => return Err(PriorityFeeKeeperError::RpcError(client_rpc_err)),
            },
            _ => return Err(PriorityFeeKeeperError::SolanaClientError(err)),
        },
    };
}

pub async fn wait_for_next_epoch(
    rpc_client: &RpcClient,
    current_epoch: u64,
    cycle_secs: u64,
) -> EpochInfo {
    loop {
        tokio::time::sleep(Duration::from_secs(cycle_secs)).await;
        info!(
            "Checking for epoch boundary... current_epoch: {}",
            current_epoch
        );

        let new_epoch_info = match rpc_client.get_epoch_info().await {
            Ok(info) => info,
            Err(e) => {
                error!("Error getting epoch info: {:?}", e);
                continue;
            }
        };

        if new_epoch_info.epoch > current_epoch {
            info!(
                "New epoch detected: {} -> {}",
                current_epoch, new_epoch_info.epoch
            );
            return new_epoch_info;
        }
    }
}
