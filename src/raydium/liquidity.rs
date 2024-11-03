use std::error::Error;
use std::sync::Arc;
use borsh::BorshDeserialize;
use serde::{Deserialize, Serialize};
use log::error;
use solana_account_decoder::parse_token::UiTokenAmount;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_program::program_pack::Pack;
use solana_sdk::pubkey::Pubkey;
use spl_token::state::Mint;
use crate::error::handle_attempt;

const MAX_FETCH_RETRIES: u32 = 3;

#[repr(u64)]
enum AmmStatus {
    Uninitialized = 0u64,
    Initialized = 1u64,
    Disabled = 2u64,
    WithdrawOnly = 3u64,
    // pool only can add or remove liquidity, can't swap and plan orders
    LiquidityOnly = 4u64,
    // pool only can add or remove liquidity and plan orders, can't swap
    OrderBookOnly = 5u64,
    // pool only can add or remove liquidity and swap, can't plan orders
    SwapOnly = 6u64,
    // pool status after created and will auto update to SwapOnly during swap after open_time
    WaitingTrade = 7u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, BorshDeserialize)]
pub struct Fees {
    /// numerator of the min_separate
    pub min_separate_numerator: u64,
    /// denominator of the min_separate
    pub min_separate_denominator: u64,

    /// numerator of the fee
    pub trade_fee_numerator: u64,
    /// denominator of the fee
    /// and 'trade_fee_denominator' must be equal to 'min_separate_denominator'
    pub trade_fee_denominator: u64,

    /// numerator of the pnl
    pub pnl_numerator: u64,
    /// denominator of the pnl
    pub pnl_denominator: u64,

    /// numerator of the swap_fee
    pub swap_fee_numerator: u64,
    /// denominator of the swap_fee
    pub swap_fee_denominator: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, BorshDeserialize)]
pub struct StateData {
    /// delay to take pnl coin
    pub need_take_pnl_coin: u64,
    /// delay to take pnl pc
    pub need_take_pnl_pc: u64,
    /// total pnl pc
    pub total_pnl_pc: u64,
    /// total pnl coin
    pub total_pnl_coin: u64,
    /// ido pool open time
    pub pool_open_time: u64,
    /// padding for future updates
    pub padding: [u64; 2],
    /// switch from orderbookonly to init
    pub orderbook_to_init_time: u64,

    /// swap coin in amount
    pub swap_coin_in_amount: u128,
    /// swap pc out amount
    pub swap_pc_out_amount: u128,
    /// charge pc as swap fee while swap pc to coin
    pub swap_acc_pc_fee: u64,

    /// swap pc in amount
    pub swap_pc_in_amount: u128,
    /// swap coin out amount
    pub swap_coin_out_amount: u128,
    /// charge coin as swap fee while swap coin to pc
    pub swap_acc_coin_fee: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Serialize, Deserialize, BorshDeserialize)]
pub struct LiquidityStateV4 {
    /// Initialized status.
    pub status: u64,
    /// Nonce used in program address.
    /// The program address is created deterministically with the nonce,
    /// amm program id, and amm account pubkey.  This program address has
    /// authority over the amm's token coin account, token pc account, and pool
    /// token mint.
    pub nonce: u64,
    /// max order count
    pub order_num: u64,
    /// within this range, 5 => 5% range
    pub depth: u64,
    /// coin decimal
    pub coin_decimals: u64,
    /// pc decimal
    pub pc_decimals: u64,
    /// amm machine state
    pub state: u64,
    /// amm reset_flag
    pub reset_flag: u64,
    /// min size 1->0.000001
    pub min_size: u64,
    /// vol_max_cut_ratio numerator, sys_decimal_value as denominator
    pub vol_max_cut_ratio: u64,
    /// amount wave numerator, sys_decimal_value as denominator
    pub amount_wave: u64,
    /// coinLotSize 1 -> 0.000001
    pub coin_lot_size: u64,
    /// pcLotSize 1 -> 0.000001
    pub pc_lot_size: u64,
    /// min_cur_price: (2 * amm.order_num * amm.pc_lot_size) * max_price_multiplier
    pub min_price_multiplier: u64,
    /// max_cur_price: (2 * amm.order_num * amm.pc_lot_size) * max_price_multiplier
    pub max_price_multiplier: u64,
    /// system decimal value, used to normalize the value of coin and pc amount
    pub sys_decimal_value: u64,
    /// All fee information
    pub fees: Fees,
    /// Statistical data
    pub state_data: StateData,
    /// Coin vault
    pub coin_vault: Pubkey,
    /// Pc vault
    pub pc_vault: Pubkey,
    /// Coin vault mint
    pub coin_vault_mint: Pubkey,
    /// Pc vault mint
    pub pc_vault_mint: Pubkey,
    /// lp mint
    pub lp_mint: Pubkey,
    /// open_orders key
    pub open_orders: Pubkey,
    /// market key
    pub market: Pubkey,
    /// market program key
    pub market_program: Pubkey,
    /// target_orders key
    pub target_orders: Pubkey,
    /// padding
    pub padding1: [u64; 8],
    /// amm owner key
    pub amm_owner: Pubkey,
    /// pool lp amount
    pub lp_amount: u64,
    /// client order id
    pub client_order_id: u64,
    /// recent epoch
    pub recent_epoch: u64,
    /// padding
    pub padding2: u64,
}

// impl LiquidityStateV4 {
//     pub const OFFSET_STATUS: usize = unsafe {
//         let state = std::mem::MaybeUninit::uninit();
//         let state_ptr: *const LiquidityStateV4 = state.as_ptr();
//
//         // cast to u8 pointers so we get offset in bytes
//         let state_u8_ptr = state_ptr as *const u8;
//         let f_u8_ptr = std::ptr::addr_of!((*state_ptr).status) as *const u8;
//
//         f_u8_ptr.offset_from(state_u8_ptr) as usize
//     };
// }

#[derive(Debug)]
pub struct LiquidityCheckResult {
    pub locked: bool,
    pub burnt: f64,
    pub lp_mint: Pubkey,
}

pub async fn get_amm_liquidity_state(rpc_client: Arc<RpcClient>, amm_id: &Pubkey) -> Result<LiquidityStateV4, Box<dyn std::error::Error + Send + Sync>> {
    let mut attempts = 0;

    loop {
        // Get program account data for the Raydium AMM account
        let data = match rpc_client.get_account_data(amm_id).await {
            Ok(data) => data,
            Err(e) => {
                error!("Failed to get amm account: {}", e);

                // Increment retry attempt counter and handle retry logic
                match handle_attempt(&mut attempts, MAX_FETCH_RETRIES, 300).await {
                    Ok(_) => continue,
                    Err(_) => return Err(Box::new(e)),
                }
            }
        };

        // Deserialize the serum market data
        return match LiquidityStateV4::try_from_slice(&data) {
            Ok(amm_data) => Ok(amm_data),
            Err(e) => {
                error!("Failed to unpack amm data: {}", e);
                return Err(Box::new(e));
            }
        };
    }
}

pub async fn get_amm_pool_reserves(
    client: Arc<RpcClient>,
    base_vault: &Pubkey,
    quote_vault: &Pubkey,
) -> Result<Vec<UiTokenAmount>, Box<dyn std::error::Error + Send + Sync>> {
    let mut attempts = 0;

    loop {
        // Fetch both base and quote vault balances concurrently
        let base_vault_future = client.get_token_account_balance(base_vault);
        let quote_vault_future = client.get_token_account_balance(quote_vault);

        match tokio::join!(base_vault_future, quote_vault_future) {
            (Ok(base_vault_balance), Ok(quote_vault_balance)) => {
                return Ok(vec![base_vault_balance, quote_vault_balance]);
            }
            (Err(e), _) | (_, Err(e)) => {
                error!("An error occurred while fetching AMM pool reserves: {}", e);

                // Increment retry attempt counter and handle retry logic
                match handle_attempt(&mut attempts, MAX_FETCH_RETRIES, 300).await {
                    Ok(_) => continue,
                    Err(_) => return Err(Box::new(e)),
                }
            }
        }
    }
}

/// Check if liquidity is locked for a given AMM pool. Liquidity is considered
/// locked if the percentage of burnt LP tokens is greater than 95%.
/// If lp_amount isn't provided, the function will fetch liquidity pool data.
pub async fn check_liquidity(
    lp_mint: &Pubkey,
    mut lp_reserve: f64,
    rpc_client: Arc<RpcClient>,
) -> Result<LiquidityCheckResult, Box<dyn Error + Send + Sync>> {
    // Fetch mint account info for lp_mint
    let mint_account_info = rpc_client.get_account(lp_mint).await?;
    let mint_data = mint_account_info.data;

    // Parse the mint account data
    let mint_info = Mint::unpack(&mint_data)?;

    // Get decimals and supply
    let decimals = mint_info.decimals;
    let supply = mint_info.supply;

    // Calculate lp_reserve / 10^decimals
    lp_reserve /= 10u64.pow(decimals as u32) as f64;

    // Calculate actualSupply
    let actual_supply = supply as f64 / 10u64.pow(decimals as u32) as f64;

    // Calculate maxLpSupply
    let max_lp_supply = actual_supply.max(lp_reserve - 1.0);

    // Calculate burnAmt
    let burn_amt = lp_reserve - actual_supply;

    // Calculate burnPct
    let burn_pct = (burn_amt / lp_reserve) * 100.0;

    // Determine if liquidity is locked
    let is_liquidity_locked = burn_pct > 95.0;

    // Build the result
    let result = LiquidityCheckResult {
        locked: is_liquidity_locked,
        burnt: burn_pct,
        lp_mint: *lp_mint,
    };

    Ok(result)
}