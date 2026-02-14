//! Game data harvester for the knowledge graph.
//!
//! Records every game, position, and decision made by the bot,
//! then exports them in formats compatible with:
//! - aiwar-neo4j-harvest (Cypher statements)
//! - neo4j-rs (embedded graph)
//! - JSON (for crewai-rust agent consumption)
//!
//! # Data Model
//!
//! ```text
//! (:Game {id, white, black, result, bot_color})
//!     -[:PLAYED_MOVE {move_number}]->
//! (:Position {fen, eval_cp, phase, piece_count})
//!     -[:MOVE {uci, eval_cp, think_time_ms}]->
//! (:Position {fen, ...})
//!     -[:BELONGS_TO]->
//! (:Opening {eco, name})
//! ```
//!
//! This schema is compatible with aiwar-neo4j-harvest's chess model.

pub mod collector;
pub mod cypher;

use async_trait::async_trait;

use crate::whatif::BranchTree;

/// Record of a complete game played on Lichess.
#[derive(Debug, Clone)]
pub struct GameRecord {
    /// Lichess game ID.
    pub game_id: String,
    /// White player username.
    pub white: String,
    /// Black player username.
    pub black: String,
    /// Game result (e.g., "mate", "resign", "draw", "outoftime").
    pub result: String,
    /// Which color the bot played.
    pub bot_color: String,
    /// All moves with position data.
    pub moves: Vec<MoveRecord>,
    /// Unix timestamp when the game started.
    pub started_at: u64,
}

impl GameRecord {
    pub fn new(game_id: String) -> Self {
        Self {
            game_id,
            white: String::new(),
            black: String::new(),
            result: String::new(),
            bot_color: String::new(),
            moves: Vec::new(),
            started_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}

/// Record of a single move/position during a game.
#[derive(Debug, Clone)]
pub struct MoveRecord {
    /// Half-move number (1-based).
    pub move_number: u32,
    /// Side that moved ("white" or "black").
    pub side: String,
    /// UCI move string (e.g., "e2e4").
    pub uci: String,
    /// FEN of the position before the move.
    pub fen_before: String,
    /// Engine evaluation in centipawns (from side-to-move perspective).
    pub eval_cp: i32,
    /// Game phase at this position.
    pub phase: String,
    /// Piece count at this position.
    pub piece_count: u32,
    /// Time spent thinking (milliseconds).
    pub think_time_ms: u64,
    /// Whether this move came from an opening book.
    pub is_book: bool,
    /// Number of legal alternatives at this position.
    pub alternatives: u32,
}

/// Trait for harvest data sinks.
///
/// Implement this to store game data in different backends:
/// - CypherHarvester: writes Cypher statements to files
/// - JsonHarvester: writes JSON for agent consumption
/// - NullHarvester: discards data (for testing)
#[async_trait]
pub trait HarvestSink: Send {
    /// Record a completed game.
    async fn record_game(
        &mut self,
        game: GameRecord,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Record a what-if branch tree for a position.
    async fn record_branch_tree(
        &mut self,
        game_id: &str,
        tree: &BranchTree,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Flush any buffered data.
    async fn flush(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// Null harvester that discards all data (for testing or when harvesting is disabled).
pub struct NullHarvester;

#[async_trait]
impl HarvestSink for NullHarvester {
    async fn record_game(
        &mut self,
        _game: GameRecord,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    async fn record_branch_tree(
        &mut self,
        _game_id: &str,
        _tree: &BranchTree,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
}
