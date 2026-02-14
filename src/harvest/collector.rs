//! JSON harvester for crewai-rust agent consumption.
//!
//! Writes game records as JSONL (one JSON object per line) that can be
//! consumed by crewai-rust agents for training and analysis.

use async_trait::async_trait;
use log::info;
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;

use super::{GameRecord, HarvestSink};
use crate::whatif::BranchTree;

/// Harvester that writes JSONL files for agent consumption.
pub struct JsonHarvester {
    output_dir: PathBuf,
    buffer: Vec<serde_json::Value>,
}

impl JsonHarvester {
    pub fn new(output_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&output_dir).ok();
        Self {
            output_dir,
            buffer: Vec::new(),
        }
    }
}

#[async_trait]
impl HarvestSink for JsonHarvester {
    async fn record_game(
        &mut self,
        game: GameRecord,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let moves: Vec<serde_json::Value> = game
            .moves
            .iter()
            .map(|mr| {
                json!({
                    "move_number": mr.move_number,
                    "side": mr.side,
                    "uci": mr.uci,
                    "fen_before": mr.fen_before,
                    "eval_cp": mr.eval_cp,
                    "phase": mr.phase,
                    "piece_count": mr.piece_count,
                    "think_time_ms": mr.think_time_ms,
                    "is_book": mr.is_book,
                    "alternatives": mr.alternatives,
                })
            })
            .collect();

        self.buffer.push(json!({
            "type": "game",
            "game_id": game.game_id,
            "white": game.white,
            "black": game.black,
            "result": game.result,
            "bot_color": game.bot_color,
            "started_at": game.started_at,
            "total_moves": game.moves.len(),
            "moves": moves,
        }));

        info!(
            "Collected game {} for JSON harvest ({} moves)",
            game.game_id,
            game.moves.len()
        );
        Ok(())
    }

    async fn record_branch_tree(
        &mut self,
        game_id: &str,
        tree: &BranchTree,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.buffer.push(json!({
            "type": "branch_tree",
            "game_id": game_id,
            "root_fen": tree.root_fen,
            "total_nodes": tree.total_nodes,
            "max_depth_reached": tree.max_depth_reached,
            "principal_variation": tree.principal_variation,
        }));
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let path = self.output_dir.join("live_games.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        for entry in &self.buffer {
            writeln!(file, "{}", entry)?;
        }

        info!(
            "Flushed {} JSON records to {}",
            self.buffer.len(),
            path.display()
        );
        self.buffer.clear();

        Ok(())
    }
}

/// Multi-harvester that fans out to multiple sinks.
pub struct MultiHarvester {
    sinks: Vec<Box<dyn HarvestSink + Send>>,
}

impl MultiHarvester {
    pub fn new(sinks: Vec<Box<dyn HarvestSink + Send>>) -> Self {
        Self { sinks }
    }
}

#[async_trait]
impl HarvestSink for MultiHarvester {
    async fn record_game(
        &mut self,
        game: GameRecord,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for sink in &mut self.sinks {
            sink.record_game(game.clone()).await?;
        }
        Ok(())
    }

    async fn record_branch_tree(
        &mut self,
        game_id: &str,
        tree: &BranchTree,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for sink in &mut self.sinks {
            sink.record_branch_tree(game_id, tree).await?;
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for sink in &mut self.sinks {
            sink.flush().await?;
        }
        Ok(())
    }
}
