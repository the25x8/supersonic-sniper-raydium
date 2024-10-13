use solana_program::instruction::{AccountMeta, Instruction};
use solana_program::pubkey::Pubkey;
use crate::detector::PoolKeys;
use crate::raydium::MainnetProgramId;

// Functions for building swap instructions would go here
pub fn build_swap_in_instruction(
    user_pubkey: &Pubkey,
    user_token_source: &Pubkey,
    user_token_destination: &Pubkey,
    amount_in: u64,
    min_amount_out: u64,
    pool_keys: &PoolKeys,
    version: u8,
) -> Instruction {
    let amm_program_id = MainnetProgramId::AmmV4.get_pubkey(); // Raydium AMM program ID

    // Amm program accounts
    let mut accounts = vec![
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new(pool_keys.id, false),
        AccountMeta::new_readonly(pool_keys.authority, false),
        AccountMeta::new(pool_keys.open_orders, false),
    ];

    if version == 4 {
        accounts.push(AccountMeta::new(pool_keys.target_orders, false));
    }

    accounts.push(AccountMeta::new(pool_keys.base_vault, false));
    accounts.push(AccountMeta::new(pool_keys.quote_vault, false));

    // if version == 5 {
    //     accounts.push(AccountMeta::new(MODEL_DATA_PUBKEY, false));
    // }

    // Expand the accounts with the serum accounts
    for account_meta in vec![
        // Serum program accounts
        AccountMeta::new_readonly(pool_keys.market_program_id, false),
        AccountMeta::new(pool_keys.market_id, false),
        AccountMeta::new(pool_keys.market_bids, false),
        AccountMeta::new(pool_keys.market_asks, false),
        AccountMeta::new(pool_keys.market_event_queue, false),
        AccountMeta::new(pool_keys.market_base_vault, false),
        AccountMeta::new(pool_keys.market_quote_vault, false),
        AccountMeta::new_readonly(pool_keys.market_authority, false),

        // User accounts
        AccountMeta::new(*user_token_source, false),
        AccountMeta::new(*user_token_destination, false),
        AccountMeta::new_readonly(*user_pubkey, true),
    ] {
        accounts.push(account_meta);
    }

    // Instruction data
    let instruction_data = {
        // Instruction ID for swap_base_in is 9
        let mut data = vec![];
        data.push(9u8); // Instruction ID for swap_base_in
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());
        data
    };

    Instruction::new_with_borsh(amm_program_id, &instruction_data, accounts)
}

pub fn build_swap_out_instruction(
    user_pubkey: &Pubkey,
    user_token_source: &Pubkey,
    user_token_destination: &Pubkey,
    amount_out: u64,
    max_amount_in: u64,
    pool_keys: &PoolKeys,
    version: u8,
) -> Instruction {
    let amm_program_id = MainnetProgramId::AmmV4.get_pubkey();

    // Amm program accounts
    let mut accounts = vec![
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new(pool_keys.id, false),
        AccountMeta::new_readonly(pool_keys.authority, false),
        AccountMeta::new(pool_keys.open_orders, false),
        AccountMeta::new(pool_keys.target_orders, false),
        AccountMeta::new(pool_keys.base_vault, false),
        AccountMeta::new(pool_keys.quote_vault, false),
    ];

    // if version == 5 {
    //     accounts.push(AccountMeta::new(MODEL_DATA_PUBKEY, false));
    // }

    // Expand the accounts with the serum accounts
    for account_meta in vec![
        // Serum program accounts
        AccountMeta::new_readonly(pool_keys.market_program_id, false),
        AccountMeta::new(pool_keys.market_id, false),
        AccountMeta::new(pool_keys.market_bids, false),
        AccountMeta::new(pool_keys.market_asks, false),
        AccountMeta::new(pool_keys.market_event_queue, false),
        AccountMeta::new(pool_keys.market_base_vault, false),
        AccountMeta::new(pool_keys.market_quote_vault, false),
        AccountMeta::new_readonly(pool_keys.market_authority, false),

        // User accounts
        AccountMeta::new(*user_token_source, false),
        AccountMeta::new(*user_token_destination, false),
        AccountMeta::new_readonly(*user_pubkey, true),
    ] {
        accounts.push(account_meta);
    }

    // Instruction data
    let instruction_data = {
        // Instruction ID for swap_base_in is 9
        let mut data = vec![];
        data.push(11u8); // Instruction ID for swap_base_out
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&amount_out.to_le_bytes());
        data
    };

    Instruction::new_with_borsh(amm_program_id, &instruction_data, accounts)
}