use anyhow::Result;
use clap::{Parser, Subcommand};
use commands::transfer_excess_rewards::*;
use commands::validator_bond_manager::*;

pub mod accounts;
pub mod active_stake;
pub mod commands;
pub mod metrics_helpers;
pub mod rewards;
pub mod rpc_utils;
pub mod transactions;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Transfer excess rewards collected for the last completed epoch to SoloValiatorBond.
    TransferExcessRewards {
        /// RPC Endpoint
        #[arg(
            short,
            long,
            env,
            default_value = "https://api.mainnet-beta.solana.com"
        )]
        rpc: String,
        /// Path to payer keypair
        #[arg(short, long, env)]
        payer: String,
        /// SoloValidatorBond's pubkey
        #[arg(short, long, env)]
        bond: String,
        /// Maximum RPC requests to send concurrently.
        #[arg(long, env, default_value = "50")]
        concurrency: usize,
        /// Dry mode to calculate excess rewards without transferring.
        #[arg(long, env)]
        dry_run: bool,
    },

    /// Will run the excess rewards stuff for all bonds owned by a validator
    ValidatorBondManager {
        #[command(flatten)]
        args: ValidatorBondManagerArgs,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Setup logging to InfluxDB with solana_metrics
    env_logger::init();
    solana_metrics::set_host_id("pye_cli".to_string());
    solana_metrics::set_panic_hook("pye_cli", Some(env!("CARGO_PKG_VERSION").to_string()));

    match cli.command {
        Commands::TransferExcessRewards {
            rpc,
            payer,
            bond,
            concurrency,
            dry_run,
        } => {
            handle_transfer_excess_rewards(TransferExcessRewardsArgs {
                rpc,
                payer_file_path: payer,
                bond,
                concurrency,
                dry_run,
            })
            .await
        }
        Commands::ValidatorBondManager { args } => handle_validator_bond_manager(args).await,
    }
}
