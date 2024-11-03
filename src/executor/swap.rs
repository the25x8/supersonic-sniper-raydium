use log::error;
use solana_program::pubkey::Pubkey;
use solana_program::system_instruction;
use spl_associated_token_account::{get_associated_token_address, instruction as ata_instruction};
use solana_program::hash::Hash;
use solana_program::message::{v0, VersionedMessage};
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::signature::{Keypair, Signer};
use crate::config::ExecutorType;
use crate::executor::order::Order;
use crate::raydium::{build_swap_in_instruction};
use crate::solana;

const BLOXROUTE_TIP_ACCOUNT: &str = "HWEoBxYs7ssKuudEjzjmpfJVX7Dvi7wescFsVx2L5yoY";

/// Builds a message to swap tokens in on the Raydium AMM.
pub async fn create_amm_swap_in(
    payer: &Keypair,
    order: &Order,
    recent_blockhash: &Hash,
    compute_unit_limit: u32,
    compute_unit_price: u64,
    executor: &ExecutorType,
    tip_account: Option<&Pubkey>,
    tip_amount: u64, // lamports
) -> Result<VersionedMessage, Box<dyn std::error::Error + Send + Sync>> {
    let payer_pubkey = payer.pubkey();

    // User token source is the associated token account for the quote currency
    let user_token_source = get_associated_token_address(
        &payer_pubkey,
        &order.in_mint,
    );

    // Get the associated token account address for the out mint
    let user_token_destination = get_associated_token_address(
        &payer_pubkey,
        &order.out_mint,
    );

    // Create Associated Token Account instruction
    let create_ata_ix = ata_instruction::create_associated_token_account(
        &payer_pubkey,
        &payer_pubkey, // Owner of the token account
        &order.out_mint, // Mint of the token account
        &spl_token::id(),
    );

    // Build swapIn instruction
    let swap_instruction = build_swap_in_instruction(
        &payer_pubkey, // Current wallet
        &user_token_source,
        &user_token_destination,
        order.amount,
        order.limit_amount,
        &order.pool_keys,
        4,
    );

    // Compute Budget Instructions
    let compute_unit_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit);
    let compute_unit_price_ix = ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price);

    // Build the transaction
    let mut instructions = vec![
        compute_unit_limit_ix,
        compute_unit_price_ix,
        create_ata_ix,
        swap_instruction,
    ];

    // If order executor isn't RPC add bribe transfer instruction
    if *executor != ExecutorType::RPC {
        // Check if tip account and amount are provided
        if tip_account.is_none() || tip_amount == 0 {
            error!("Tip account and amount must be provided for bribe transactions");
            return Err("Tip account and amount must be provided for bribe transactions".into());
        }

        instructions.push(system_instruction::transfer(
            &payer.pubkey(),
            tip_account.unwrap(),
            tip_amount,
        ));
    }

    // let raw_account = client.get_account(&address_lookup_table_key)?;
    // let address_lookup_table = AddressLookupTable::deserialize(&raw_account.data)?;
    // let address_lookup_table_account = AddressLookupTableAccount {
    //     key: address_lookup_table_key,
    //     addresses: address_lookup_table.addresses.to_vec(),
    // };

    // Build v0 message
    let message = match v0::Message::try_compile(
        &payer.pubkey(),
        &instructions,
        &[],
        *recent_blockhash,
    ) {
        Ok(message) => message,
        Err(err) => {
            error!("Error compiling swap in v0 message: {:?}", err);
            return Err(err.into());
        }
    };

    Ok(VersionedMessage::V0(message))
}

/// Builds a message to swap tokens out on the Raydium AMM.
pub async fn create_amm_swap_out(
    payer: &Keypair,
    order: &Order,
    recent_blockhash: &Hash,
    compute_unit_limit: u32,
    compute_unit_price: u64,
    executor: &ExecutorType,
    tip_account: Option<&Pubkey>,
    tip_amount: u64, // lamports
) -> Result<VersionedMessage, Box<dyn std::error::Error + Send + Sync>> {
    let payer_pubkey = payer.pubkey();

    // User token source is the associated token account for the quote currency
    let user_token_source = get_associated_token_address(
        &payer_pubkey,
        &order.in_mint, // Out mint is the base currency
    );

    // Get the associated token account address for the base mint
    let user_token_destination = get_associated_token_address(
        &payer_pubkey,
        &order.out_mint, // In mint is the quote currency
    );

    // Build swapOut instruction
    let swap_instruction = build_swap_in_instruction(
        &payer_pubkey, // Current wallet
        &user_token_source,
        &user_token_destination,
        order.amount,
        order.limit_amount,
        &order.pool_keys,
        4,
    );

    // Build close account instruction
    let close_account_ix = match solana::build_close_spl_token_account_instruction(
        &payer_pubkey,
        &user_token_source,
    ) {
        Ok(ix) => ix,
        Err(err) => {
            return Err(err)
        }
    };

    // Compute Budget Instructions
    let compute_unit_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit);
    let compute_unit_price_ix = ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price);

    // Build the transaction
    let mut instructions = vec![
        compute_unit_limit_ix,
        compute_unit_price_ix,
        swap_instruction,
        close_account_ix,
    ];

    // If order executor isn't RPC add bribe transfer instruction
    if *executor != ExecutorType::RPC {
        // Check if tip account and amount are provided
        if tip_account.is_none() || tip_amount == 0 {
            error!("Tip account and amount must be provided for bribe transactions");
            return Err("Tip account and amount must be provided for bribe transactions".into());
        }

        instructions.push(system_instruction::transfer(
            &payer.pubkey(),
            tip_account.unwrap(),
            tip_amount,
        ));
    }

    // Build v0 message
    let message = match v0::Message::try_compile(
        &payer.pubkey(),
        &instructions,
        &[],
        *recent_blockhash,
    ) {
        Ok(message) => message,
        Err(err) => {
            error!("Error compiling swap out v0 message: {:?}", err);
            return Err(err.into());
        }
    };

    Ok(VersionedMessage::V0(message))
}
