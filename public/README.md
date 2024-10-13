# **Step-by-Step Guide to Running the Supersonic Sniper Bot**

Welcome to the Supersonic Sniper Bot! This guide will help you, as a non-technical user, to run the Rust binary on **macOS/Unix** and **Windows** systems. The bot is designed to interact with the Raydium AMM on the Solana blockchain, allowing you to monitor and execute trades efficiently.

---

## **Prerequisites**

Before we begin, please ensure you have the following:

1. **Supersonic Sniper Bot Binary**: The compiled executable file for your operating system.
2. **`config/default.yml` File**: A configuration file that contains necessary settings for the bot.
3. **`data/active_trades.json` File**: A data file that stores active trades.
4. **`data/trade_history.json` File**: A data file that stores trade history.
5. **`data/orders.json` File**: A data file that stores pending orders.

---

## **Contents**

- [Step 1: Download the Binary](#step-1-download-the-binary)
- [Step 2: Prepare the Required Folders and Files](#step-2-prepare-the-required-folders-and-files)
- [Step 3: Running the Bot](#step-3-running-the-bot)
    - [On macOS/Unix](#on-macosunix)
    - [On Windows](#on-windows)
- [Step 4: Using the Bot Commands](#step-4-using-the-bot-commands)
- [Appendix: Sample `default.yml` Configuration](#appendix-sample-defaultyml-configuration)

---

## **Step 1: Download the Binary**

Obtain the Supersonic Sniper Bot binary for your operating system:

- **macOS/Unix**: `supersonic_sniper_bot_macos`
- **Windows**: `supersonic_sniper_bot_windows.exe`

> **Note**: Ensure that you download the binary from a trusted source provided by the developers.

---

## **Step 2: Prepare the Required Files**

### **Create the `config/` Folder and `default.yml` File**

- **macOS/Unix and Windows**:
    - In the same directory, create a folder named `config`.
    - Inside the `config/` folder, create a file named `default.yml`.

  ```bash
  mkdir config
  touch config/default.yml
  ```

- **Edit the `default.yml` File**:
    - Open `default.yml` with a text editor.
    - Add your configuration settings (see the [Appendix](#appendix-sample-defaultyml-configuration) for a sample configuration).

> **Important**: Ensure that the `default.yml` file contains all the necessary configurations required by the bot.

---

## **Step 3: Running the Bot**

### **On macOS/Unix**

#### **1. Open Terminal**

- Navigate to the directory containing the bot binary.

  ```bash
  cd /path/to/your/bot/directory
  ```

#### **2. Grant Execution Permissions**

- Ensure the binary has execution permissions.

  ```bash
  chmod +x supersonic_sniper_bot_macos
  ```

#### **3. Run the Bot**

- To run the bot with the default configuration:

  ```bash
  ./supersonic_sniper_bot_macos run
  ```

- If you need to specify a custom configuration file or wallet secret key:

  ```bash
  ./supersonic_sniper_bot_macos run -c config/default.yml -w YOUR_WALLET_SECRET_KEY
  ```

### **On Windows**

#### **1. Open Command Prompt**

- Navigate to the directory containing the bot executable.

  ```batch
  cd \path\to\your\bot\directory
  ```

#### **2. Run the Bot**

- To run the bot with the default configuration:

  ```batch
  supersonic_sniper_bot_windows.exe run
  ```

> **Tip**: You can also double-click the executable to run it, but running via the command line allows you to see logs and outputs.

---

## **Step 4: Using the Bot Commands**

The Supersonic Sniper Bot provides several commands and options to interact with. Below is a breakdown of the available commands:

### **General Syntax**

```bash
./supersonic_sniper_bot [OPTIONS] <SUBCOMMAND>
```

### **Global Options**

- `-c`, `--config <FILE>`: Specify a custom configuration file.
- `-w`, `--wallet-secret-key <SECRET_KEY>`: Set the wallet secret key directly.

### All Commands
- Use the `--help` flag to see available commands and options:
    ```bash
    ./supersonic_sniper_bot --help
    ```

### **Subcommands**

1. **`run`**

    - **Description**: Runs the bot with both the detector and trader modules.
    - **Usage**:

      ```bash
      ./supersonic_sniper_bot run
      ```

2. **`detector`**

    - **Description**: Runs only the detector module.
    - **Usage**:

      ```bash
      ./supersonic_sniper_bot detector
      ```

3. **`info`**

    - **Description**: Prints information about trades, orders, history, and stats.
    - **Options**:

        - `-i`, `--trade <ID>`: Show detailed information about a trade. Provide the trade ID, pool, or base token address.
        - `-a`, `--active`: Show a table with active trades.
        - `-p`, `--pending`: Show a list of pending orders.
        - `-t`, `--history`: Show the trade history table.
        - `-s`, `--stats`: Print total stats based on trade history.

    - **Usage Examples**:

        - Show detailed information about a trade:

          ```bash
          ./supersonic_sniper_bot info -i TRADE_ID_OR_POOL_OR_BASE_TOKEN
          ```

        - Show active trades:

          ```bash
          ./supersonic_sniper_bot info -a
          ```

        - Show pending orders:

          ```bash
          ./supersonic_sniper_bot info -p
          ```

        - Show trade history:

          ```bash
          ./supersonic_sniper_bot info -t
          ```

        - Show stats based on trade history:

          ```bash
          ./supersonic_sniper_bot info -s
          ```

        - Combine options:

          ```bash
          ./supersonic_sniper_bot info -a -p -t -s
          ```

---

## **Appendix: Sample `default.yml` Configuration**

Below is an example of what your `config/default.yml` file might look like. Adjust the settings according to your needs.

```yaml
rpc:
  # Solana RPC URL is required. It can be a public or private RPC node.
  url: "https://api.mainnet-beta.solana.com"

  # WebSocket URL is required for account updates.
  ws_url: "wss://api.mainnet-beta.solana.com"

bloxroute:
  # If enabled, the bot will use Bloxroute to broadcast
  # transactions to speed up the transaction propagation.
  enabled: false

  # Bloxroute URL and WebSocket URL.
  url: "https://uk.solana.dex.blxrbdn.com"
  ws_url: "wss://uk.solana.dex.blxrbdn.com/ws"

  # Auth token is required when using Bloxroute.
  auth_token: ""

# Wallet configuration
wallet:
  # The bot will use the first account in the wallet to trade.
  # If you want to use a different account, you can specify the
  # account index here.
  account_index: 0

  # Path to the wallet file. If you are using a wallet file, you can
  # specify the path to the file here.
  file_path: "/path/to/your/solana/wallet.json"

  # Wallet secret key. If you are using a wallet secret key, you can
  # specify the key here.
  # secret_key: ""

# Detector module configuration
detector:
  # Watchlist is a list of tokens that the bot will focus on.
  # If the watchlist is empty, the bot will focus on all tokens.
  # Otherwise, the bot will process only tokens from the watchlist.
  watchlist: []
#   watchlist:
#     - "Dyg9HthwdkenToccZQM76TjWFoqSPfQd5d8iR8vspump"
#     - "7tGAa9otm4Qmmd9Di1PQJCyyZz1Wh78drpH2TiaTEb5G"
#     - "Bk6btMamZ2UiUdgxApy5w9iBRU9JY1Kr3T1Zgbae5jZw"
#     - "7D7BRcBYepfi77vxySapmeqRNN1wsBBxnFPJGbH5pump"

# Trader module configuration
trader:
  # List of conditions that can be used to choose the strategy.
  # Any condition with a value of 0 or commented out will be ignored.
  strategies:
    # Base strategy suitable for most trading pairs.
    base:
      freezable: false # If the token can be frozen set to true.
      mint_renounced: true # If the owner can mint new tokens set to false.
      # meta_mutable: false # If the token metadata can be changed set to true.

      quote_symbol: "WSOL" # The quote symbol of the trading pair can be USDC or WSOL.
      # base_symbol: "XYZ" # The base symbol of the trading pair (token).

      # If the pool detected time is older than the max_delay_since_detect,
      # the bot will not trade. A checking will be performed before trade execution.
      max_delay_since_detect: 8000 # milliseconds

      # Some base conditions can lead to additional data being fetched.
      # This will increase the time it takes to make a trading decision,
      # as the bot needs to fetch the data from different sources.
      # has_website: true
      # has_social_networks: true
      # has_community: true
      # has_team: true
      # has_audit: true
      # has_whitepaper: true
      # has_partnership: true
      # min_audits_passed: 0
      # min_social_networks: 0

      # Price conditions. A price in the quote currency.
      price_condition: "gt" # Price operator can be "eq", "gt", "gte", "lt", "lte", "in".
      price: 0.0 # Price value.
      # price_range: [0.0, 0.0] # Price range required when using "in" operator.

      # Reserves conditions in the base and quote currencies.
      reserves_base_condition: "gt" # Reserves operator can be "eq", "gt", "gte", "lt", "lte", "in".
      reserves_base: 0.0
      # reserves_base_range: [0.0, 0.0]
      reserves_quote_condition: "gt"
      reserves_quote: 10.0 # WSOL reserves.
      # reserves_quote_range: [0.0, 0.0]

      # Total supply conditions.
      total_supply_condition: "gt" # Total supply operator can be "eq", "gt", "gte", "lt", "lte", "in".
      total_supply: 0.0 # Total supply value.

      # Market data conditions. Enable these conditions to filter tokens
      # based on volume, transactions, makers, buyers, sellers, buys, sells.
      # This data is fetched from the Solana Serum DEX API leads to additional
      # time to make a trading decision and recommended to be disabled.
      # volume_condition: "gt"
      # volume: 0.0
      # volume_range: [0.0, 0.0]

      # buy_condition: "gt"
      # buy: 0.0
      # buy_range: [0.0, 0.0]

      # sell_condition: "gt"
      # sell: 0.0
      # sell_range: [0.0, 0.0]

      # txns_condition: "gt"
      # txns: 0.0
      # txns_range: [0.0, 0.0]

      # makers_condition: "gt"
      # makers: 0.0
      # makers_range: [0.0, 0.0]

      # byers_condition: "gt"
      # byers: 0.0
      # byers_range: [0.0, 0.0]

      # sellers_condition: "gt"
      # sellers: 0.0
      # sellers_range: [0.0, 0.0]

      # buys_condition: "gt"
      # buys: 0.0
      # buys_range: [0.0, 0.0]

      # sells_condition: "gt"
      # sells: 0.0
      # sells_range: [0.0, 0.0]

      # Configuration for executing orders.
      buy_delay: 0 # Delay in milliseconds before executing the buy order.
      sell_delay: 0
      buy_slippage: 10.0 # Maximum slippage allowed for the buy order.
      sell_slippage: 50.0
      buy_executor: "bloxroute" # Tx executor, can be "rpc" or "bloxroute".
      max_retries: 5 # Number of retries if the buy order fails.
      quote_amount: 0.21 # Amount of quote currency to spend.
      sell_percent: 100.0 # Percentage of the base currency to sell.

      # After holding the token for the hold_time, the bot will
      # execute the sell order automatically by current price.
      hold_time: 300000 # Hold time in milliseconds.
      take_profit: 25.0 # Take profit percentage.
      stop_loss: 10.0 # Stop loss percentage.
      take_profit_executor: "rpc"
      stop_loss_executor: "bloxroute"

      # The bribes are used to incentivize the validators to include the transaction
      # in the block. It will be used only if Bloxroute is enabled and auth token is provided.
      buy_bribe: 0.003 # The bribe amount in SOL.
      sell_bribe: 0.003
```

> **Important**: Replace `"YOUR_WALLET_SECRET_KEY"` with your actual wallet secret key. Keep this information secure and do not share it with others.

---

## **Additional Tips**

- **Operation Not Permitted**: If you encounter an "Operation not permitted" error on macOS, try running the binary with: `xattr -d com.apple.quarantine ./supersonic-sniper`.
- **Ensure Network Connectivity**: The bot requires internet access to interact with the Solana blockchain.
- **Check Permissions**: On macOS/Unix, you may need to grant execution permissions to the binary using `chmod +x`.
- **Monitor Logs**: Keep an eye on the console output or log files to monitor the bot's activity.
- **Security**: Keep your wallet secret key secure. Do not expose it in public or share it.
