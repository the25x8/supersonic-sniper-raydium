use solana_program::instruction::Instruction;
use solana_program::program_pack::Pack;
use solana_sdk::{
    signature::{Keypair, Signer},
    transaction::Transaction,
    system_instruction,
    pubkey::Pubkey,
};
use spl_token::instruction as token_instruction;

pub async fn build_spl_token_account_instruction(
    payer: &Keypair,
    token_mint: &Pubkey,
    rpc_client: &solana_client::rpc_client::RpcClient,
) -> Result<Keypair, Box<dyn std::error::Error>> {
    // Generate a new Keypair for the token account
    let token_account = Keypair::new();

    // Build instructions
    let instructions = match build_spl_token_account_instructions(
        payer,
        &token_account,
        token_mint,
        rpc_client,
    ) {
        Ok(instructions) => instructions,
        Err(err) => return Err(err),
    };

    // Create and send transaction
    let recent_blockhash = rpc_client.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(
        instructions.as_slice(),
        Some(&payer.pubkey()),
        &[payer, &token_account],
        recent_blockhash,
    );

    rpc_client.send_and_confirm_transaction(&tx)?;

    Ok(token_account)
}

pub fn build_spl_token_account_instructions(
    payer: &Keypair,
    token_account: &Keypair,
    token_mint: &Pubkey,
    rpc_client: &solana_client::rpc_client::RpcClient,
) -> Result<Vec<Instruction>, Box<dyn std::error::Error>> {
    // Calculate rent-exempt balance
    let token_account_rent = rpc_client
        .get_minimum_balance_for_rent_exemption(spl_token::state::Account::LEN)?;

    // Build instructions
    let create_account_ix = system_instruction::create_account(
        &payer.pubkey(),
        &token_account.pubkey(),
        token_account_rent,
        spl_token::state::Account::LEN as u64,
        &spl_token::id(),
    );

    let init_account_ix = token_instruction::initialize_account(
        &spl_token::id(),
        &token_account.pubkey(),
        token_mint,
        &payer.pubkey(),
    )?;

    // Build instructions vector
    Ok(vec![create_account_ix, init_account_ix])
}

pub fn build_close_spl_token_account_instruction(
    payer_pubkey: &Pubkey,
    token_account_pubkey: &Pubkey,
) -> Result<Instruction, Box<dyn std::error::Error>> {
    use spl_token::instruction::close_account;

    let close_account_ix = close_account(
        &spl_token::id(),
        token_account_pubkey,
        payer_pubkey,   // Destination of the reclaimed lamports
        payer_pubkey,   // Authority to close the account (must be the payer)
        &[],
    )?;

    Ok(close_account_ix)
}

pub async fn close_spl_token_account(
    payer: &Keypair,
    token_mint: &Pubkey,
    rpc_client: &solana_client::rpc_client::RpcClient,
) -> Result<(), Box<dyn std::error::Error>> {
    let close_account_ix = build_close_spl_token_account_instruction(
        &payer.pubkey(),
        token_mint,
    )?;

    // Build and send the transaction
    let recent_blockhash = rpc_client.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(
        &[close_account_ix],
        Some(&payer.pubkey()),
        &[payer],
        recent_blockhash,
    );

    rpc_client.send_and_confirm_transaction(&tx)?;

    Ok(())
}