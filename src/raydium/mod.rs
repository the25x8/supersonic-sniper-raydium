pub mod detector;
mod instructions;

pub use instructions::{build_swap_in_instruction, build_swap_out_instruction};

use std::str::FromStr;
use borsh_derive::BorshDeserialize;
use solana_program::pubkey::Pubkey;

/// Enum to represent the mainnet program IDs.
pub enum MainnetProgramId {
    SerumMarket,
    OpenbookMarket,
    Util1216,
    FarmV3,
    FarmV5,
    FarmV6,
    AmmV4,
    AmmStable,
    Clmm,
    Router,
}

impl MainnetProgramId {
    pub fn get_pubkey(&self) -> Pubkey {
        match self {
            MainnetProgramId::AmmV4 => Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").unwrap(),
            MainnetProgramId::AmmStable => Pubkey::from_str("5quBtoiQqxF9Jv6KYKctB59NT3gtJD2Y65kdnB1Uev3h").unwrap(),
            MainnetProgramId::Clmm => Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK").unwrap(),
            MainnetProgramId::SerumMarket => Pubkey::from_str("9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin").unwrap(),
            MainnetProgramId::OpenbookMarket => Pubkey::from_str("srmqPvymJeFKQ4zGQed1GFppgkRHL9kaELCbyksJtPX").unwrap(),
            MainnetProgramId::Util1216 => Pubkey::from_str("CLaimxFqjHzgTJtAGHU47NPhg6qrc5sCnpC4tBLyABQS").unwrap(),
            MainnetProgramId::FarmV3 => Pubkey::from_str("EhhTKczWMGQt46ynNeRX1WfeagwwJd7ufHvCDjRxjo5Q").unwrap(),
            MainnetProgramId::FarmV5 => Pubkey::from_str("9KEPoZmtHUrBbhWN1v1KWLMkkvwY6WLtAVUCPRtRjP4z").unwrap(),
            MainnetProgramId::FarmV6 => Pubkey::from_str("FarmqiPv5eAj3j1GMdMCMUGXqPUvmquZtMy86QH6rzhG").unwrap(),
            MainnetProgramId::Router => Pubkey::from_str("routeUGWgWzqBWFcrCfv8tritsqukccJPu3q5GPP3xS").unwrap(),
        }
    }
}

#[repr(u64)]
pub enum AmmStatus {
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
#[derive(Clone, Copy, Debug, Default, PartialEq, BorshDeserialize)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, BorshDeserialize)]
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
#[derive(Debug, BorshDeserialize)]
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