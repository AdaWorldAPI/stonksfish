//! Lichess Bot integration module.
//!
//! Provides a production-grade Lichess bot that:
//! - Handles concurrent games via tokio tasks
//! - Accepts/declines challenges based on configurable rules
//! - Harvests every position and decision for the knowledge graph
//! - Integrates with crewai-rust agents for multi-agent analysis
//!
//! # Architecture
//!
//! ```text
//! Lichess API (NDJSON stream)
//!     ↕ licheszter client
//! LichessBot::run()
//!     ├── Challenge → accept/decline (challenge.rs rules)
//!     ├── GameStart → spawn game_manager::play_game()
//!     │       ├── Bot::choose_move()  (engine)
//!     │       ├── harvest::Collector  (records positions)
//!     │       └── whatif::branch      (optional deep analysis)
//!     └── GameFinish → harvest::flush()
//! ```

pub mod challenge;
pub mod game_manager;

use licheszter::client::Licheszter;
use licheszter::models::board::Event;
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;

use crate::harvest::HarvestSink;
use challenge::ChallengeConfig;

/// Configuration for the Lichess bot.
#[derive(Debug, Clone)]
pub struct BotConfig {
    /// Lichess API token.
    pub token: String,
    /// Engine search depth (plies).
    pub depth: u8,
    /// Maximum concurrent games.
    pub max_concurrent_games: usize,
    /// Challenge acceptance rules.
    pub challenge: ChallengeConfig,
    /// Whether to run what-if branching on critical positions.
    pub whatif_enabled: bool,
    /// Bot's username on Lichess (determined at startup).
    pub bot_username: String,
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            token: String::new(),
            depth: 5,
            max_concurrent_games: 4,
            challenge: ChallengeConfig::default(),
            whatif_enabled: false,
            bot_username: String::new(),
        }
    }
}

impl BotConfig {
    /// Create config from environment variables.
    pub fn from_env() -> Self {
        Self {
            token: std::env::var("RUST_BOT_TOKEN").unwrap_or_default(),
            depth: std::env::var("BOT_DEPTH")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5),
            max_concurrent_games: std::env::var("BOT_MAX_GAMES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(4),
            challenge: ChallengeConfig::from_env(),
            whatif_enabled: std::env::var("BOT_WHATIF")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            bot_username: String::new(),
        }
    }
}

/// The main Lichess bot.
///
/// Owns the API client, manages concurrent games, and routes
/// harvested data to the configured sink.
pub struct LichessBot {
    client: Licheszter,
    config: BotConfig,
    harvester: Arc<Mutex<Box<dyn HarvestSink + Send>>>,
    active_games: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
}

impl LichessBot {
    /// Create a new bot with the given config and harvest sink.
    pub fn new(config: BotConfig, harvester: Box<dyn HarvestSink + Send>) -> Self {
        let client = Licheszter::new(config.token.clone());
        Self {
            client,
            config,
            harvester: Arc::new(Mutex::new(harvester)),
            active_games: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Run the bot event loop. This is the main entry point.
    ///
    /// Streams events from Lichess and dispatches them:
    /// - Challenge → accept or decline
    /// - GameStart → spawn concurrent game handler
    /// - GameFinish → clean up and flush harvest data
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!(
            "Starting Lichess bot (depth={}, max_games={}, whatif={})",
            self.config.depth, self.config.max_concurrent_games, self.config.whatif_enabled
        );

        let mut stream = self
            .client
            .stream_events()
            .await
            .map_err(|e| format!("Failed to stream events: {:?}", e))?;

        info!("Event stream connected. Waiting for events...");

        while let Ok(Some(event)) = stream.try_next().await {
            match event {
                Event::Challenge {
                    challenge,
                    compat: _,
                } => {
                    let challenger_name = challenge
                        .challenger
                        .as_ref()
                        .map(|u| u.username.as_str())
                        .unwrap_or("unknown");

                    let time_control = challenge
                        .time_control
                        .show
                        .as_deref()
                        .unwrap_or("n/a");

                    info!(
                        "[{}] Challenge from {} ({})",
                        challenge.id, challenger_name, time_control
                    );

                    // Check concurrent game limit
                    let active_count = self.active_games.lock().await.len();
                    if active_count >= self.config.max_concurrent_games {
                        info!(
                            "[{}] Declining: at max concurrent games ({}/{})",
                            challenge.id, active_count, self.config.max_concurrent_games
                        );
                        if let Err(e) = self.client.challenge_decline(&challenge.id, None).await {
                            warn!("[{}] Failed to decline: {:?}", challenge.id, e);
                        }
                        continue;
                    }

                    // Apply challenge rules
                    if challenge::should_accept(&challenge, &self.config.challenge) {
                        info!("[{}] Accepting challenge", challenge.id);
                        if let Err(e) = self.client.challenge_accept(&challenge.id).await {
                            error!("[{}] Failed to accept: {:?}", challenge.id, e);
                        }
                    } else {
                        info!("[{}] Declining: does not match rules", challenge.id);
                        if let Err(e) = self.client.challenge_decline(&challenge.id, None).await {
                            warn!("[{}] Failed to decline: {:?}", challenge.id, e);
                        }
                    }
                }

                Event::GameStart { game: game_id } => {
                    let game_id_str = game_id.id.clone();
                    info!("[{}] Game started", game_id_str);

                    let client = Licheszter::new(self.config.token.clone());
                    let depth = self.config.depth;
                    let whatif = self.config.whatif_enabled;
                    let harvester = Arc::clone(&self.harvester);
                    let bot_username = self.config.bot_username.clone();

                    let handle = tokio::spawn(async move {
                        if let Err(e) = game_manager::play_game(
                            client,
                            &game_id_str,
                            depth,
                            whatif,
                            &bot_username,
                            harvester,
                        )
                        .await
                        {
                            error!("[{}] Game error: {:?}", game_id_str, e);
                        }
                    });

                    self.active_games
                        .lock()
                        .await
                        .insert(game_id.id.clone(), handle);
                }

                Event::GameFinish { game: game_id } => {
                    info!("[{}] Game finished", game_id.id);
                    if let Some(handle) = self.active_games.lock().await.remove(&game_id.id) {
                        handle.abort();
                    }
                    // Flush harvest data
                    if let Err(e) = self.harvester.lock().await.flush().await {
                        warn!("Harvest flush error: {:?}", e);
                    }
                }

                Event::ChallengeCanceled { challenge } => {
                    debug!("[{}] Challenge cancelled", challenge.id);
                }

                event => {
                    debug!("Other event: {:?}", event);
                }
            }
        }

        info!("Event stream ended. Shutting down...");

        // Final harvest flush
        if let Err(e) = self.harvester.lock().await.flush().await {
            warn!("Final harvest flush error: {:?}", e);
        }

        Ok(())
    }
}
