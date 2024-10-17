pub mod detector;
mod instructions;
mod liquidity;

pub use instructions::{build_swap_in_instruction};
pub use liquidity::{
    check_liquidity,
    get_amm_pool_reserves,
    LiquidityStateV4,
};

use std::str::FromStr;
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