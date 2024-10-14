use std::sync::Arc;
use borsh::BorshDeserialize;
use log::error;
use serde::{Deserialize, Serialize};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use crate::error::handle_attempt;

#[repr(C)]
#[derive(Debug, Clone, Serialize, Deserialize, BorshDeserialize)]
pub struct MarketStateV3 {
    pub reserved: [u8; 5],

    pub account_flags: u64, // Initialized, Market

    pub own_address: Pubkey,

    pub vault_signer_nonce: u64,
    pub coin_mint: Pubkey,
    pub pc_mint: Pubkey,

    pub coin_vault: Pubkey,
    pub coin_deposits_total: u64,
    pub coin_fees_accrued: u64,

    pub pc_vault: Pubkey,
    pub pc_deposits_total: u64,
    pub pc_fees_accrued: u64,

    pub pc_dust_threshold: u64,

    pub request_queue: Pubkey,
    pub event_queue: Pubkey,

    pub bids: Pubkey,
    pub asks: Pubkey,

    pub coin_lot_size: u64,
    pub pc_lot_size: u64,

    pub fee_rate_bps: u64,
    pub referrer_rebates_accrued: u64,

    pub reserved_2: [u8; 7],
}

pub async fn get_serum_market_state(rpc_client: Arc<RpcClient>, market_id: &Pubkey) -> Result<MarketStateV3, Box<dyn std::error::Error + Send + Sync>> {
    let mut attempts = 0;
    const MAX_RETRIES: u32 = 3;

    loop {
        // Get program account data for the serum market
        let serum_market = match rpc_client.get_account(market_id).await {
            Ok(account) => account,
            Err(e) => {
                error!("Failed to get serum market account: {}", e);

                // Increment retry attempt counter and handle retry logic
                match handle_attempt(&mut attempts, MAX_RETRIES, 300).await {
                    Ok(_) => continue,
                    Err(_) => return Err(Box::new(e)),
                }
            }
        };

        // Deserialize the serum market data
        return match MarketStateV3::try_from_slice(&serum_market.data) {
            Ok(market_data) => Ok(market_data),
            Err(e) => {
                error!("Failed to unpack serum market data: {}", e);
                Err(Box::new(e))
            }
        };
    }
}