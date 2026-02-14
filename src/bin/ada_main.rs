//! stonksfish-ada: Unified Lichess bot with game harvesting.
//!
//! This is the production binary that combines:
//! - Stonksfish chess engine (alpha-beta search + evaluation)
//! - Lichess Bot API client (concurrent games)
//! - Game harvester (Cypher + JSON output)
//! - What-if branching (optional deep analysis)
//!
//! # Usage
//!
//! ```bash
//! # Required
//! export RUST_BOT_TOKEN=lip_xxxxx
//!
//! # Optional
//! export BOT_DEPTH=5              # Engine search depth
//! export BOT_MAX_GAMES=4          # Max concurrent games
//! export BOT_WHATIF=false          # Enable what-if branching
//! export BOT_USERNAME=AdaChessBot # Bot username (auto-detected if omitted)
//! export HARVEST_DIR=./harvest    # Output directory for harvested data
//! export HARVEST_FORMAT=both      # cypher, json, or both
//!
//! cargo run --bin stonksfish-ada --release
//! ```

use dotenv::dotenv;
use log::info;
use std::path::PathBuf;

use stonksfish::harvest::collector::{JsonHarvester, MultiHarvester};
use stonksfish::harvest::cypher::CypherHarvester;
use stonksfish::harvest::{HarvestSink, NullHarvester};
use stonksfish::lichess::{BotConfig, LichessBot};

#[tokio::main]
async fn main() {
    dotenv().ok();
    env_logger::init();

    println!("=== stonksfish-ada ===");
    println!("Unified Lichess bot with game harvesting");
    println!();

    // Load configuration
    let mut config = BotConfig::from_env();

    if config.token.is_empty() {
        eprintln!("Error: RUST_BOT_TOKEN environment variable is required.");
        eprintln!("Get a token at: https://lichess.org/account/oauth/token");
        std::process::exit(1);
    }

    // Set bot username from env or default
    config.bot_username = std::env::var("BOT_USERNAME").unwrap_or_else(|_| "AdaChessBot".to_string());

    info!(
        "Config: depth={}, max_games={}, whatif={}, username={}",
        config.depth, config.max_concurrent_games, config.whatif_enabled, config.bot_username
    );

    // Build harvester based on HARVEST_FORMAT
    let harvest_dir = std::env::var("HARVEST_DIR").unwrap_or_else(|_| "./harvest".to_string());
    let harvest_format = std::env::var("HARVEST_FORMAT").unwrap_or_else(|_| "both".to_string());

    let harvester: Box<dyn HarvestSink + Send> = match harvest_format.as_str() {
        "cypher" => {
            info!("Harvest format: Cypher (aiwar-neo4j-harvest compatible)");
            Box::new(CypherHarvester::new(PathBuf::from(&harvest_dir)))
        }
        "json" => {
            info!("Harvest format: JSON (crewai-rust agent compatible)");
            Box::new(JsonHarvester::new(PathBuf::from(&harvest_dir)))
        }
        "both" => {
            info!("Harvest format: Cypher + JSON (dual output)");
            Box::new(MultiHarvester::new(vec![
                Box::new(CypherHarvester::new(PathBuf::from(format!(
                    "{}/cypher",
                    harvest_dir
                )))),
                Box::new(JsonHarvester::new(PathBuf::from(format!(
                    "{}/json",
                    harvest_dir
                )))),
            ]))
        }
        "none" => {
            info!("Harvest format: None (data discarded)");
            Box::new(NullHarvester)
        }
        _ => {
            eprintln!(
                "Unknown HARVEST_FORMAT '{}'. Use: cypher, json, both, or none",
                harvest_format
            );
            std::process::exit(1);
        }
    };

    // Create and run the bot
    let bot = LichessBot::new(config, harvester);

    info!("Connecting to Lichess...");
    match bot.run().await {
        Ok(()) => info!("Bot shut down cleanly."),
        Err(e) => {
            eprintln!("Bot error: {}", e);
            std::process::exit(1);
        }
    }
}
