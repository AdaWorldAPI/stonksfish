//! What-If branching engine for 32-move look-ahead testing.
//!
//! Generates a tree of candidate move sequences (up to 32 half-moves deep)
//! for each position. Each branch is a speculative future that can be
//! evaluated by crewai-rust agents and stored in the neo4j-rs knowledge graph.
//!
//! # Architecture
//!
//! ```text
//! Current Position (FEN)
//!     ├── Move A ──→ Position A₁
//!     │   ├── Response A₁₁ ──→ Position A₁₁
//!     │   │   ├── Move A₁₁₁ ──→ ...  (up to 32 plies deep)
//!     │   │   └── Move A₁₁₂ ──→ ...
//!     │   └── Response A₁₂ ──→ Position A₁₂
//!     ├── Move B ──→ Position B₁
//!     │   └── ...
//!     └── Move C ──→ Position C₁
//!         └── ...
//! ```
//!
//! The branching factor is controlled by `BranchConfig::width` (how many
//! candidate moves to explore at each depth). With width=3 and depth=32,
//! the full tree would have 3^32 nodes — far too many. So we use
//! **selective deepening**: only the top-K moves (by engine evaluation)
//! are explored at each level, and the depth is reduced for lower-ranked
//! moves.

use chess::{Board, ChessMove, Color, MoveGen, EMPTY};
use std::fmt;
use std::str::FromStr;

use crate::engine::evaluation::simple::evaluate_board;
use crate::engine::search::find_move;
use crate::uci::{analyze_position, classify_phase, count_pieces, format_move};

/// Maximum look-ahead depth (32 half-moves = 16 full moves).
pub const MAX_BRANCH_DEPTH: u8 = 32;

/// Default branching width at each level.
pub const DEFAULT_WIDTH: usize = 3;

/// Configuration for what-if branching.
#[derive(Debug, Clone)]
pub struct BranchConfig {
    /// Maximum depth in half-moves (plies). Default: 32.
    pub max_depth: u8,
    /// Number of candidate moves to explore at each depth. Default: 3.
    pub width: usize,
    /// Minimum search depth for move ordering. Default: 3.
    pub ordering_depth: u8,
    /// Whether to use selective deepening (reduce depth for lower-ranked moves).
    pub selective_deepening: bool,
    /// Maximum total nodes to generate (budget). Default: 10_000.
    pub node_budget: usize,
    /// Minimum evaluation change to keep exploring a branch (centipawns).
    pub prune_threshold: i32,
}

impl Default for BranchConfig {
    fn default() -> Self {
        Self {
            max_depth: MAX_BRANCH_DEPTH,
            width: DEFAULT_WIDTH,
            ordering_depth: 3,
            selective_deepening: true,
            node_budget: 10_000,
            prune_threshold: 500, // Prune if position swings > 5 pawns
        }
    }
}

impl BranchConfig {
    /// Create a config for quick analysis (shallow, narrow).
    pub fn quick() -> Self {
        Self {
            max_depth: 8,
            width: 2,
            ordering_depth: 2,
            selective_deepening: true,
            node_budget: 500,
            prune_threshold: 300,
        }
    }

    /// Create a config for deep analysis (full 32-move lookahead).
    pub fn deep() -> Self {
        Self {
            max_depth: MAX_BRANCH_DEPTH,
            width: 3,
            ordering_depth: 4,
            selective_deepening: true,
            node_budget: 50_000,
            prune_threshold: 800,
        }
    }
}

/// A node in the what-if branching tree.
#[derive(Debug, Clone)]
pub struct BranchNode {
    /// Unique identifier for this branch (path from root).
    pub branch_id: String,
    /// FEN of the position at this node.
    pub fen: String,
    /// The move that led to this position (None for root).
    pub move_uci: Option<String>,
    /// Depth from root (0 = current position).
    pub depth: u8,
    /// Static evaluation in centipawns (from side to move).
    pub eval_cp: i32,
    /// Game phase at this node.
    pub phase: String,
    /// Piece count.
    pub piece_count: u32,
    /// Whether the game is over at this node.
    pub is_terminal: bool,
    /// Terminal reason (if applicable).
    pub terminal_reason: Option<String>,
    /// Parent branch_id (None for root).
    pub parent_id: Option<String>,
    /// Child branch_ids.
    pub children: Vec<String>,
    /// Fork ID for what-if execution tracking.
    pub fork_id: String,
}

/// Result of what-if branching from a position.
#[derive(Debug, Clone)]
pub struct BranchTree {
    /// Root position FEN.
    pub root_fen: String,
    /// All nodes in the tree (breadth-first order).
    pub nodes: Vec<BranchNode>,
    /// Configuration used.
    pub config: BranchConfig,
    /// Total nodes generated.
    pub total_nodes: usize,
    /// Maximum depth reached.
    pub max_depth_reached: u8,
    /// Principal variation (best line).
    pub principal_variation: Vec<String>,
}

/// Generate a what-if branching tree from the given position.
///
/// This is the main entry point for what-if testing. It builds a tree
/// of candidate move sequences up to `config.max_depth` half-moves deep,
/// exploring the top `config.width` moves at each level.
pub fn generate_branch_tree(fen: &str, config: &BranchConfig) -> Option<BranchTree> {
    let root_board = Board::from_str(fen).ok()?;
    let root_eval = evaluate_board(&root_board);

    let mut tree = BranchTree {
        root_fen: fen.to_string(),
        nodes: Vec::new(),
        config: config.clone(),
        total_nodes: 0,
        max_depth_reached: 0,
        principal_variation: Vec::new(),
    };

    let root_node = BranchNode {
        branch_id: "root".to_string(),
        fen: fen.to_string(),
        move_uci: None,
        depth: 0,
        eval_cp: root_eval,
        phase: classify_phase(&root_board).to_string(),
        piece_count: count_pieces(&root_board),
        is_terminal: MoveGen::new_legal(&root_board).len() == 0,
        terminal_reason: terminal_reason(&root_board),
        parent_id: None,
        children: Vec::new(),
        fork_id: format!("fork-root"),
    };

    tree.nodes.push(root_node);
    tree.total_nodes = 1;

    // Recursive branching
    expand_node(&mut tree, 0, &root_board, config, &mut 1);

    // Extract principal variation
    tree.principal_variation = extract_pv(&tree);
    tree.max_depth_reached = tree.nodes.iter().map(|n| n.depth).max().unwrap_or(0);

    Some(tree)
}

/// Expand a node by generating child branches.
fn expand_node(
    tree: &mut BranchTree,
    node_idx: usize,
    board: &Board,
    config: &BranchConfig,
    node_counter: &mut usize,
) {
    let current_depth = tree.nodes[node_idx].depth;

    // Check stopping conditions
    if current_depth >= config.max_depth {
        return;
    }
    if tree.total_nodes >= config.node_budget {
        return;
    }
    if tree.nodes[node_idx].is_terminal {
        return;
    }

    // Generate and rank candidate moves
    let candidates = rank_moves(board, config);
    let width = candidates.len().min(config.width);

    let parent_id = tree.nodes[node_idx].branch_id.clone();
    let parent_eval = tree.nodes[node_idx].eval_cp;

    let mut child_indices = Vec::new();

    for (rank, (chess_move, move_eval)) in candidates.iter().take(width).enumerate() {
        if tree.total_nodes >= config.node_budget {
            break;
        }

        let mut new_board = Board::default();
        board.make_move(*chess_move, &mut new_board);

        let move_str = format_move(*chess_move);
        let branch_id = format!("{}-{}", parent_id, move_str);
        let child_eval = -evaluate_board(&new_board);

        // Pruning: skip if evaluation swings too much (likely losing)
        if config.selective_deepening && (child_eval - parent_eval).abs() > config.prune_threshold {
            if rank > 0 {
                continue; // Keep exploring the best move even if it swings
            }
        }

        let child_node = BranchNode {
            branch_id: branch_id.clone(),
            fen: format!("{}", new_board),
            move_uci: Some(move_str),
            depth: current_depth + 1,
            eval_cp: child_eval,
            phase: classify_phase(&new_board).to_string(),
            piece_count: count_pieces(&new_board),
            is_terminal: MoveGen::new_legal(&new_board).len() == 0,
            terminal_reason: terminal_reason(&new_board),
            parent_id: Some(parent_id.clone()),
            children: Vec::new(),
            fork_id: format!("fork-{}", *node_counter),
        };

        tree.nodes.push(child_node);
        let child_idx = tree.nodes.len() - 1;
        child_indices.push((child_idx, new_board));
        tree.total_nodes += 1;
        *node_counter += 1;
    }

    // Update parent's children list
    let child_branch_ids: Vec<String> = child_indices
        .iter()
        .map(|(idx, _)| tree.nodes[*idx].branch_id.clone())
        .collect();
    tree.nodes[node_idx].children = child_branch_ids;

    // Recursively expand children (selective deepening: reduce width for lower-ranked)
    for (rank, (child_idx, child_board)) in child_indices.into_iter().enumerate() {
        let mut child_config = config.clone();
        if config.selective_deepening && rank > 0 {
            // Reduce depth for non-best moves
            child_config.max_depth = child_config.max_depth.saturating_sub(rank as u8 * 2);
            child_config.width = (child_config.width).max(1);
        }
        expand_node(tree, child_idx, &child_board, &child_config, node_counter);
    }
}

/// Rank candidate moves by evaluation (using shallow search).
fn rank_moves(board: &Board, config: &BranchConfig) -> Vec<(ChessMove, i32)> {
    let mut moves: Vec<(ChessMove, i32)> = Vec::new();
    let movegen = MoveGen::new_legal(board);
    let mut new_board = Board::default();

    for chess_move in movegen {
        board.make_move(chess_move, &mut new_board);
        let eval = -evaluate_board(&new_board);
        moves.push((chess_move, eval));
    }

    // Sort by evaluation (best moves first)
    moves.sort_by(|a, b| b.1.cmp(&a.1));
    moves
}

/// Determine if a position is terminal and why.
fn terminal_reason(board: &Board) -> Option<String> {
    let legal_moves = MoveGen::new_legal(board).len();
    if legal_moves == 0 {
        if board.checkers().popcnt() > 0 {
            Some("checkmate".to_string())
        } else {
            Some("stalemate".to_string())
        }
    } else {
        None
    }
}

/// Extract the principal variation (best line) from the tree.
fn extract_pv(tree: &BranchTree) -> Vec<String> {
    let mut pv = Vec::new();
    let mut current_idx = 0; // Start from root

    loop {
        let node = &tree.nodes[current_idx];
        if node.children.is_empty() {
            break;
        }

        // Find the best child (highest absolute evaluation)
        let best_child_id = &node.children[0]; // First child is the best (sorted)
        if let Some(child_idx) = tree.nodes.iter().position(|n| &n.branch_id == best_child_id) {
            if let Some(ref m) = tree.nodes[child_idx].move_uci {
                pv.push(m.clone());
            }
            current_idx = child_idx;
        } else {
            break;
        }
    }

    pv
}

/// Get a summary of the branching tree for display.
pub fn tree_summary(tree: &BranchTree) -> TreeSummary {
    let mut depth_counts = vec![0u32; (tree.max_depth_reached + 1) as usize];
    let mut terminal_count = 0u32;
    let mut checkmate_count = 0u32;
    let mut stalemate_count = 0u32;
    let mut min_eval = i32::MAX;
    let mut max_eval = i32::MIN;

    for node in &tree.nodes {
        if (node.depth as usize) < depth_counts.len() {
            depth_counts[node.depth as usize] += 1;
        }
        if node.is_terminal {
            terminal_count += 1;
            match node.terminal_reason.as_deref() {
                Some("checkmate") => checkmate_count += 1,
                Some("stalemate") => stalemate_count += 1,
                _ => {}
            }
        }
        min_eval = min_eval.min(node.eval_cp);
        max_eval = max_eval.max(node.eval_cp);
    }

    TreeSummary {
        total_nodes: tree.total_nodes,
        max_depth: tree.max_depth_reached,
        depth_distribution: depth_counts,
        terminal_nodes: terminal_count,
        checkmates: checkmate_count,
        stalemates: stalemate_count,
        eval_range: (min_eval, max_eval),
        principal_variation: tree.principal_variation.clone(),
        branching_factor: if tree.total_nodes > 1 {
            (tree.total_nodes as f64 - 1.0) / tree.nodes.iter().filter(|n| !n.children.is_empty()).count().max(1) as f64
        } else {
            0.0
        },
    }
}

/// Summary statistics for a what-if branching tree.
#[derive(Debug, Clone)]
pub struct TreeSummary {
    pub total_nodes: usize,
    pub max_depth: u8,
    pub depth_distribution: Vec<u32>,
    pub terminal_nodes: u32,
    pub checkmates: u32,
    pub stalemates: u32,
    pub eval_range: (i32, i32),
    pub principal_variation: Vec<String>,
    pub branching_factor: f64,
}

impl fmt::Display for TreeSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "What-If Branch Tree Summary")?;
        writeln!(f, "  Total nodes: {}", self.total_nodes)?;
        writeln!(f, "  Max depth: {} plies ({} full moves)", self.max_depth, self.max_depth / 2)?;
        writeln!(f, "  Terminal nodes: {} ({} checkmates, {} stalemates)", self.terminal_nodes, self.checkmates, self.stalemates)?;
        writeln!(f, "  Eval range: [{}, {}] cp", self.eval_range.0, self.eval_range.1)?;
        writeln!(f, "  Avg branching factor: {:.1}", self.branching_factor)?;
        writeln!(f, "  Principal variation: {}", self.principal_variation.join(" "))?;
        write!(f, "  Depth distribution: ")?;
        for (d, count) in self.depth_distribution.iter().enumerate() {
            if *count > 0 {
                write!(f, "d{}={} ", d, count)?;
            }
        }
        Ok(())
    }
}

/// Serialize a BranchTree to JSON for storage in neo4j-rs or transmission
/// to crewai-rust agents.
pub fn tree_to_json(tree: &BranchTree) -> serde_json::Value {
    serde_json::json!({
        "root_fen": tree.root_fen,
        "total_nodes": tree.total_nodes,
        "max_depth_reached": tree.max_depth_reached,
        "principal_variation": tree.principal_variation,
        "config": {
            "max_depth": tree.config.max_depth,
            "width": tree.config.width,
            "node_budget": tree.config.node_budget,
            "selective_deepening": tree.config.selective_deepening,
        },
        "nodes": tree.nodes.iter().map(|n| {
            serde_json::json!({
                "branch_id": n.branch_id,
                "fen": n.fen,
                "move_uci": n.move_uci,
                "depth": n.depth,
                "eval_cp": n.eval_cp,
                "phase": n.phase,
                "piece_count": n.piece_count,
                "is_terminal": n.is_terminal,
                "terminal_reason": n.terminal_reason,
                "parent_id": n.parent_id,
                "children": n.children,
                "fork_id": n.fork_id,
            })
        }).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STARTPOS: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

    #[test]
    fn test_branch_config_default() {
        let config = BranchConfig::default();
        assert_eq!(config.max_depth, 32);
        assert_eq!(config.width, 3);
        assert_eq!(config.node_budget, 10_000);
    }

    #[test]
    fn test_generate_branch_tree_startpos() {
        let config = BranchConfig::quick();
        let tree = generate_branch_tree(STARTPOS, &config).unwrap();

        assert_eq!(tree.root_fen, STARTPOS);
        assert!(tree.total_nodes > 1, "Should have expanded at least one level");
        assert!(!tree.nodes.is_empty());
        assert_eq!(tree.nodes[0].depth, 0); // Root is depth 0
        assert!(tree.nodes[0].children.len() <= config.width);
    }

    #[test]
    fn test_branch_tree_depth() {
        let config = BranchConfig {
            max_depth: 4,
            width: 2,
            ordering_depth: 1,
            selective_deepening: false,
            node_budget: 100,
            prune_threshold: 10_000,
        };
        let tree = generate_branch_tree(STARTPOS, &config).unwrap();
        assert!(tree.max_depth_reached <= 4);
    }

    #[test]
    fn test_branch_tree_budget() {
        let config = BranchConfig {
            max_depth: 32,
            width: 3,
            ordering_depth: 1,
            selective_deepening: false,
            node_budget: 50,
            prune_threshold: 10_000,
        };
        let tree = generate_branch_tree(STARTPOS, &config).unwrap();
        assert!(tree.total_nodes <= 50, "Should respect node budget, got {}", tree.total_nodes);
    }

    #[test]
    fn test_principal_variation() {
        let config = BranchConfig::quick();
        let tree = generate_branch_tree(STARTPOS, &config).unwrap();
        assert!(!tree.principal_variation.is_empty(), "PV should not be empty");
    }

    #[test]
    fn test_tree_summary() {
        let config = BranchConfig::quick();
        let tree = generate_branch_tree(STARTPOS, &config).unwrap();
        let summary = tree_summary(&tree);
        assert!(summary.total_nodes > 0);
        assert!(!summary.depth_distribution.is_empty());
        let display = format!("{}", summary);
        assert!(display.contains("What-If Branch Tree Summary"));
    }

    #[test]
    fn test_tree_to_json() {
        let config = BranchConfig {
            max_depth: 2,
            width: 2,
            ordering_depth: 1,
            selective_deepening: false,
            node_budget: 10,
            prune_threshold: 10_000,
        };
        let tree = generate_branch_tree(STARTPOS, &config).unwrap();
        let json = tree_to_json(&tree);
        assert_eq!(json["root_fen"], STARTPOS);
        assert!(json["nodes"].is_array());
    }

    #[test]
    fn test_terminal_detection() {
        // Scholar's mate position (checkmate)
        let checkmate_fen = "rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3";
        let config = BranchConfig::quick();
        let tree = generate_branch_tree(checkmate_fen, &config);
        if let Some(tree) = tree {
            // Root should be terminal (checkmate)
            assert!(tree.nodes[0].is_terminal);
        }
    }

    #[test]
    fn test_branch_node_parent_child_links() {
        let config = BranchConfig {
            max_depth: 2,
            width: 2,
            ordering_depth: 1,
            selective_deepening: false,
            node_budget: 20,
            prune_threshold: 10_000,
        };
        let tree = generate_branch_tree(STARTPOS, &config).unwrap();

        // Root should have no parent
        assert!(tree.nodes[0].parent_id.is_none());

        // All non-root nodes should have a parent
        for node in &tree.nodes[1..] {
            assert!(node.parent_id.is_some(), "Node {} has no parent", node.branch_id);
        }

        // Parent-child links should be consistent
        for node in &tree.nodes {
            for child_id in &node.children {
                let child = tree.nodes.iter().find(|n| &n.branch_id == child_id);
                assert!(child.is_some(), "Child {} not found in tree", child_id);
                assert_eq!(child.unwrap().parent_id.as_ref().unwrap(), &node.branch_id);
            }
        }
    }

    #[test]
    fn test_selective_deepening() {
        let config_selective = BranchConfig {
            max_depth: 6,
            width: 3,
            ordering_depth: 1,
            selective_deepening: true,
            node_budget: 200,
            prune_threshold: 500,
        };
        let config_flat = BranchConfig {
            max_depth: 6,
            width: 3,
            ordering_depth: 1,
            selective_deepening: false,
            node_budget: 200,
            prune_threshold: 500,
        };

        let tree_selective = generate_branch_tree(STARTPOS, &config_selective).unwrap();
        let tree_flat = generate_branch_tree(STARTPOS, &config_flat).unwrap();

        // Selective deepening should explore the best line deeper
        // but use fewer total nodes (or reach deeper along the PV)
        assert!(tree_selective.principal_variation.len() >= tree_flat.principal_variation.len()
            || tree_selective.total_nodes <= tree_flat.total_nodes,
            "Selective deepening should either reach deeper PV or use fewer nodes");
    }
}
