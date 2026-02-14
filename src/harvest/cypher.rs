//! Cypher statement generator for aiwar-neo4j-harvest compatibility.
//!
//! Generates Cypher CREATE/MERGE statements that match the schema used
//! by aiwar-neo4j-harvest's chess harvesting pipeline:
//!
//! - Position nodes with multi-label faceting (:Position:Middlegame)
//! - MOVE relationships with evaluation metadata
//! - Game nodes linking to position chains
//! - Opening identification via ECO codes

use async_trait::async_trait;
use log::info;
use std::io::Write;
use std::path::PathBuf;

use super::{GameRecord, HarvestSink, MoveRecord};
use crate::whatif::BranchTree;

/// Harvester that writes Cypher statements to files.
///
/// Compatible with aiwar-neo4j-harvest's cypher ingestion pipeline.
/// Generated files can be loaded with `cypher-shell` or neo4j-rs.
pub struct CypherHarvester {
    /// Output directory for .cypher files.
    output_dir: PathBuf,
    /// Buffered Cypher statements.
    buffer: Vec<String>,
    /// Number of games recorded.
    game_count: u32,
}

impl CypherHarvester {
    pub fn new(output_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&output_dir).ok();
        Self {
            output_dir,
            buffer: Vec::new(),
            game_count: 0,
        }
    }

    /// Generate Cypher for a Game node.
    fn game_cypher(game: &GameRecord) -> String {
        format!(
            "MERGE (g:Game:LiveGame {{id: '{game_id}'}}) \
             SET g.white = '{white}', g.black = '{black}', \
             g.result = '{result}', g.bot_color = '{bot_color}', \
             g.started_at = {started_at}, g.total_moves = {total_moves};\n",
            game_id = escape_cypher(&game.game_id),
            white = escape_cypher(&game.white),
            black = escape_cypher(&game.black),
            result = escape_cypher(&game.result),
            bot_color = escape_cypher(&game.bot_color),
            started_at = game.started_at,
            total_moves = game.moves.len(),
        )
    }

    /// Generate Cypher for a Position node with phase-based multi-label.
    fn position_cypher(mr: &MoveRecord) -> String {
        let phase_label = match mr.phase.as_str() {
            "opening" => ":Opening",
            "middlegame" => ":Middlegame",
            "endgame" => ":Endgame",
            _ => "",
        };

        format!(
            "MERGE (p:Position{phase_label} {{fen: '{fen}'}}) \
             SET p.eval_cp = {eval_cp}, p.phase = '{phase}', \
             p.piece_count = {piece_count};\n",
            phase_label = phase_label,
            fen = escape_cypher(&mr.fen_before),
            eval_cp = mr.eval_cp,
            phase = escape_cypher(&mr.phase),
            piece_count = mr.piece_count,
        )
    }

    /// Generate Cypher for a MOVE relationship between positions.
    fn move_cypher(from: &MoveRecord, to_fen: &str, game_id: &str) -> String {
        format!(
            "MATCH (from:Position {{fen: '{from_fen}'}}), \
             (to:Position {{fen: '{to_fen}'}}) \
             MERGE (from)-[:MOVE {{uci: '{uci}', eval_cp: {eval_cp}, \
             think_time_ms: {think_ms}, move_number: {move_num}, \
             game_id: '{game_id}', side: '{side}', \
             alternatives: {alts}, is_book: {is_book}}}]->(to);\n",
            from_fen = escape_cypher(&from.fen_before),
            to_fen = escape_cypher(to_fen),
            uci = escape_cypher(&from.uci),
            eval_cp = from.eval_cp,
            think_ms = from.think_time_ms,
            move_num = from.move_number,
            game_id = escape_cypher(game_id),
            side = escape_cypher(&from.side),
            alts = from.alternatives,
            is_book = from.is_book,
        )
    }

    /// Generate Cypher for linking a Game to its positions.
    fn game_position_cypher(game_id: &str, fen: &str, move_number: u32) -> String {
        format!(
            "MATCH (g:Game {{id: '{game_id}'}}), \
             (p:Position {{fen: '{fen}'}}) \
             MERGE (g)-[:PLAYED_MOVE {{move_number: {move_number}}}]->(p);\n",
            game_id = escape_cypher(game_id),
            fen = escape_cypher(fen),
            move_number = move_number,
        )
    }

    /// Generate Cypher for a BranchTree (what-if analysis).
    fn branch_tree_cypher(game_id: &str, tree: &BranchTree) -> Vec<String> {
        let mut stmts = Vec::new();

        for node in &tree.nodes {
            let phase_label = match node.phase.as_str() {
                "opening" => ":Opening",
                "middlegame" => ":Middlegame",
                "endgame" => ":Endgame",
                _ => "",
            };

            // Create position node for each branch position
            stmts.push(format!(
                "MERGE (p:Position{phase_label} {{fen: '{fen}'}}) \
                 SET p.eval_cp = {eval_cp}, p.phase = '{phase}', \
                 p.piece_count = {piece_count};\n",
                phase_label = phase_label,
                fen = escape_cypher(&node.fen),
                eval_cp = node.eval_cp,
                phase = escape_cypher(&node.phase),
                piece_count = node.piece_count,
            ));

            // Create branch relationship
            if let (Some(ref parent_id), Some(ref move_uci)) =
                (&node.parent_id, &node.move_uci)
            {
                // Find parent FEN
                if let Some(parent) = tree.nodes.iter().find(|n| &n.branch_id == parent_id) {
                    stmts.push(format!(
                        "MATCH (from:Position {{fen: '{from_fen}'}}), \
                         (to:Position {{fen: '{to_fen}'}}) \
                         MERGE (from)-[:WHATIF_MOVE {{uci: '{uci}', \
                         game_id: '{game_id}', branch_id: '{branch_id}', \
                         depth: {depth}, eval_cp: {eval_cp}}}]->(to);\n",
                        from_fen = escape_cypher(&parent.fen),
                        to_fen = escape_cypher(&node.fen),
                        uci = escape_cypher(move_uci),
                        game_id = escape_cypher(game_id),
                        branch_id = escape_cypher(&node.branch_id),
                        depth = node.depth,
                        eval_cp = node.eval_cp,
                    ));
                }
            }
        }

        stmts
    }
}

#[async_trait]
impl HarvestSink for CypherHarvester {
    async fn record_game(
        &mut self,
        game: GameRecord,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Game node
        self.buffer.push(Self::game_cypher(&game));

        // Position nodes and MOVE relationships
        for (i, mr) in game.moves.iter().enumerate() {
            self.buffer.push(Self::position_cypher(mr));
            self.buffer
                .push(Self::game_position_cypher(&game.game_id, &mr.fen_before, mr.move_number));

            // MOVE edge to the next position
            if i + 1 < game.moves.len() {
                let next_fen = &game.moves[i + 1].fen_before;
                self.buffer
                    .push(Self::move_cypher(mr, next_fen, &game.game_id));
            }
        }

        self.game_count += 1;
        info!(
            "Harvested game {} ({} moves, {} positions)",
            game.game_id,
            game.moves.len(),
            game.moves.len()
        );

        Ok(())
    }

    async fn record_branch_tree(
        &mut self,
        game_id: &str,
        tree: &BranchTree,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let stmts = Self::branch_tree_cypher(game_id, tree);
        self.buffer.extend(stmts);
        info!(
            "Harvested branch tree for game {} ({} nodes)",
            game_id, tree.total_nodes
        );
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let filename = format!("live_games_{:04}.cypher", self.game_count);
        let path = self.output_dir.join(&filename);

        let mut file = std::fs::File::create(&path)?;

        // Write header
        writeln!(
            file,
            "// Auto-generated by stonksfish-ada live game harvester"
        )?;
        writeln!(
            file,
            "// Compatible with aiwar-neo4j-harvest chess schema"
        )?;
        writeln!(file, "// Games harvested: {}\n", self.game_count)?;

        // Write constraints (idempotent)
        writeln!(
            file,
            "CREATE CONSTRAINT IF NOT EXISTS FOR (g:Game) REQUIRE g.id IS UNIQUE;"
        )?;
        writeln!(
            file,
            "CREATE CONSTRAINT IF NOT EXISTS FOR (p:Position) REQUIRE p.fen IS UNIQUE;\n"
        )?;

        // Write all buffered statements
        for stmt in &self.buffer {
            write!(file, "{}", stmt)?;
        }

        info!("Flushed {} Cypher statements to {}", self.buffer.len(), path.display());
        self.buffer.clear();

        Ok(())
    }
}

/// Escape single quotes for Cypher string literals.
fn escape_cypher(s: &str) -> String {
    s.replace('\'', "\\'").replace('\\', "\\\\")
}
