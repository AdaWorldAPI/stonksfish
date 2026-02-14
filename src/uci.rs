//! UCI (Universal Chess Interface) protocol handler for Stonksfish.
//!
//! This module implements the UCI protocol using the `ruci` crate, allowing
//! Stonksfish to be used with any UCI-compatible GUI or bridge (like lichess-bot).
//!
//! # Architecture
//!
//! ```text
//! GUI / lichess-bot
//!     ↕ stdin/stdout
//! uci::run_uci_loop()
//!     ↕ function calls
//! engine::search::find_move()
//! engine::evaluation::evaluate_board()
//! ```

use chess::{Board, ChessMove, Color, MoveGen, Square};
use std::io::{self, BufRead, Write};
use std::str::FromStr;

use crate::engine::search::find_move;
use crate::engine::evaluation::simple::evaluate_board;

/// Engine identity constants.
const ENGINE_NAME: &str = "Stonksfish";
const ENGINE_AUTHOR: &str = "Claus Martinsen + Ada Chess AI";
const DEFAULT_DEPTH: u8 = 5;
const MAX_DEPTH: u8 = 20;

/// Run the UCI protocol loop on stdin/stdout.
///
/// This is the main entry point when running Stonksfish as a UCI engine.
/// It reads UCI commands from stdin, processes them, and writes responses
/// to stdout.
pub fn run_uci_loop() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut reader = stdin.lock();

    let mut board = Board::default();
    let mut depth = DEFAULT_DEPTH;
    let mut debug_mode = false;
    let mut line = String::new();

    loop {
        line.clear();
        if reader.read_line(&mut line).is_err() {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        match parts[0] {
            "uci" => {
                writeln!(stdout, "id name {}", ENGINE_NAME).ok();
                writeln!(stdout, "id author {}", ENGINE_AUTHOR).ok();
                writeln!(stdout, "option name Depth type spin default {} min 1 max {}", DEFAULT_DEPTH, MAX_DEPTH).ok();
                writeln!(stdout, "option name CrewAI type check default false").ok();
                writeln!(stdout, "uciok").ok();
                stdout.flush().ok();
            }

            "isready" => {
                writeln!(stdout, "readyok").ok();
                stdout.flush().ok();
            }

            "ucinewgame" => {
                board = Board::default();
            }

            "debug" => {
                if parts.len() > 1 {
                    debug_mode = parts[1] == "on";
                }
            }

            "setoption" => {
                // Parse: setoption name <name> [value <value>]
                if let Some(option) = parse_setoption(trimmed) {
                    match option.name.to_lowercase().as_str() {
                        "depth" => {
                            if let Ok(d) = option.value.parse::<u8>() {
                                depth = d.clamp(1, MAX_DEPTH);
                            }
                        }
                        _ => {
                            if debug_mode {
                                writeln!(stdout, "info string unknown option: {}", option.name).ok();
                            }
                        }
                    }
                }
            }

            "position" => {
                board = parse_position(&parts);
                if debug_mode {
                    writeln!(stdout, "info string position set: {}", board).ok();
                    stdout.flush().ok();
                }
            }

            "go" => {
                let go_depth = parse_go_depth(&parts).unwrap_or(depth);

                // Run the search
                let best_move = find_move(&board, go_depth);
                let eval = evaluate_board(&board);

                // Send info about the search
                writeln!(stdout, "info depth {} score cp {}", go_depth, eval).ok();

                // Send the best move
                let move_str = format_move(best_move);
                writeln!(stdout, "bestmove {}", move_str).ok();
                stdout.flush().ok();
            }

            "stop" => {
                // We don't have async search yet, so stop is a no-op
            }

            "quit" => {
                break;
            }

            "eval" => {
                // Non-standard: evaluate current position
                let eval = evaluate_board(&board);
                let piece_count = count_pieces(&board);
                writeln!(stdout, "info string eval={} pieces={} side={:?}", eval, piece_count, board.side_to_move()).ok();
                stdout.flush().ok();
            }

            "perft" => {
                // Non-standard: run perft for move generation testing
                let perft_depth = parts.get(1).and_then(|s| s.parse::<u8>().ok()).unwrap_or(1);
                let count = perft(&board, perft_depth);
                writeln!(stdout, "info string perft({})={}", perft_depth, count).ok();
                stdout.flush().ok();
            }

            _ => {
                if debug_mode {
                    writeln!(stdout, "info string unknown command: {}", trimmed).ok();
                    stdout.flush().ok();
                }
            }
        }
    }
}

/// Parse a UCI `position` command.
///
/// Supports:
/// - `position startpos [moves e2e4 e7e5 ...]`
/// - `position fen <fen_string> [moves e2e4 e7e5 ...]`
fn parse_position(parts: &[&str]) -> Board {
    if parts.len() < 2 {
        return Board::default();
    }

    let (mut board, moves_start) = if parts[1] == "startpos" {
        let idx = parts.iter().position(|&p| p == "moves").unwrap_or(parts.len());
        (Board::default(), idx + 1)
    } else if parts[1] == "fen" {
        // Collect FEN components (up to 6 parts after "fen")
        let moves_idx = parts.iter().position(|&p| p == "moves").unwrap_or(parts.len());
        let fen_parts: Vec<&str> = parts[2..moves_idx].to_vec();
        let fen_str = fen_parts.join(" ");
        let board = Board::from_str(&fen_str).unwrap_or_default();
        (board, moves_idx + 1)
    } else {
        return Board::default();
    };

    // Apply moves
    if moves_start < parts.len() {
        for move_str in &parts[moves_start..] {
            if let Some(chess_move) = parse_uci_move(&board, move_str) {
                let mut new_board = Board::default();
                board.make_move(chess_move, &mut new_board);
                board = new_board;
            }
        }
    }

    board
}

/// Parse a UCI move string (e.g., "e2e4", "e7e8q") into a ChessMove.
fn parse_uci_move(board: &Board, move_str: &str) -> Option<ChessMove> {
    let move_str = move_str.trim();
    if move_str.len() < 4 {
        return None;
    }

    let from = Square::from_str(&move_str[0..2]).ok()?;
    let to = Square::from_str(&move_str[2..4]).ok()?;

    // Check for promotion piece
    let promotion = if move_str.len() > 4 {
        match move_str.as_bytes()[4] {
            b'q' | b'Q' => Some(chess::Piece::Queen),
            b'r' | b'R' => Some(chess::Piece::Rook),
            b'b' | b'B' => Some(chess::Piece::Bishop),
            b'n' | b'N' => Some(chess::Piece::Knight),
            _ => None,
        }
    } else {
        None
    };

    let chess_move = ChessMove::new(from, to, promotion);

    // Verify the move is legal
    if board.legal(chess_move) {
        Some(chess_move)
    } else {
        None
    }
}

/// Format a ChessMove as a UCI string (e.g., "e2e4", "e7e8q").
fn format_move(m: ChessMove) -> String {
    let from = m.get_source();
    let to = m.get_dest();
    let promo = m.get_promotion().map(|p| match p {
        chess::Piece::Queen => "q",
        chess::Piece::Rook => "r",
        chess::Piece::Bishop => "b",
        chess::Piece::Knight => "n",
        _ => "",
    }).unwrap_or("");

    format!("{}{}{}", from, to, promo)
}

/// Parse depth from `go` command arguments.
///
/// Supports: `go depth 8`, `go movetime 5000` (returns None for time-based).
fn parse_go_depth(parts: &[&str]) -> Option<u8> {
    for (i, &part) in parts.iter().enumerate() {
        if part == "depth" {
            return parts.get(i + 1).and_then(|s| s.parse::<u8>().ok());
        }
    }
    None
}

/// Represents a parsed UCI option.
struct UciOption {
    name: String,
    value: String,
}

/// Parse a `setoption name <name> value <value>` command.
fn parse_setoption(cmd: &str) -> Option<UciOption> {
    let name_start = cmd.find("name ")? + 5;
    let (name_end, value) = if let Some(val_idx) = cmd.find(" value ") {
        (val_idx, cmd[val_idx + 7..].to_string())
    } else {
        (cmd.len(), String::new())
    };
    let name = cmd[name_start..name_end].trim().to_string();

    Some(UciOption { name, value })
}

/// Count total pieces on the board.
pub fn count_pieces(board: &Board) -> u32 {
    board.combined().popcnt()
}

/// Simple perft (performance test) for move generation verification.
fn perft(board: &Board, depth: u8) -> u64 {
    if depth == 0 {
        return 1;
    }

    let mut count = 0u64;
    let movegen = MoveGen::new_legal(board);
    let mut new_board = Board::default();

    for chess_move in movegen {
        board.make_move(chess_move, &mut new_board);
        count += perft(&new_board, depth - 1);
    }

    count
}

/// Classify the game phase based on piece count.
pub fn classify_phase(board: &Board) -> &'static str {
    let pieces = count_pieces(board);
    if pieces <= 10 {
        "endgame"
    } else if pieces <= 24 {
        "middlegame"
    } else {
        "opening"
    }
}

/// Get evaluation and all legal moves with their evaluations.
///
/// This is the main interface for crewai-rust agents to use Stonksfish
/// as a tool. Returns structured data about the position.
pub fn analyze_position(board: &Board, depth: u8) -> PositionAnalysis {
    let eval = evaluate_board(board);
    let phase = classify_phase(board);
    let piece_count = count_pieces(board);

    let mut legal_moves = Vec::new();
    let movegen = MoveGen::new_legal(board);
    let mut new_board = Board::default();

    for chess_move in movegen {
        board.make_move(chess_move, &mut new_board);
        let move_eval = -evaluate_board(&new_board);
        legal_moves.push(MoveEvaluation {
            uci: format_move(chess_move),
            eval_cp: move_eval,
            is_capture: board.piece_on(chess_move.get_dest()).is_some(),
            is_check: new_board.checkers().popcnt() > 0,
        });
    }

    // Sort by evaluation (best moves first)
    legal_moves.sort_by(|a, b| b.eval_cp.cmp(&a.eval_cp));

    PositionAnalysis {
        fen: format!("{}", board),
        eval_cp: eval,
        phase: phase.to_string(),
        piece_count,
        side_to_move: format!("{:?}", board.side_to_move()),
        legal_moves,
        is_check: board.checkers().popcnt() > 0,
        is_checkmate: MoveGen::new_legal(board).len() == 0 && board.checkers().popcnt() > 0,
        is_stalemate: MoveGen::new_legal(board).len() == 0 && board.checkers().popcnt() == 0,
    }
}

/// Result of analyzing a chess position.
#[derive(Debug, Clone)]
pub struct PositionAnalysis {
    /// FEN string of the position.
    pub fen: String,
    /// Evaluation in centipawns from side-to-move's perspective.
    pub eval_cp: i32,
    /// Game phase: "opening", "middlegame", or "endgame".
    pub phase: String,
    /// Total piece count.
    pub piece_count: u32,
    /// Side to move: "White" or "Black".
    pub side_to_move: String,
    /// All legal moves with evaluations, sorted best-first.
    pub legal_moves: Vec<MoveEvaluation>,
    /// Whether the side to move is in check.
    pub is_check: bool,
    /// Whether the position is checkmate.
    pub is_checkmate: bool,
    /// Whether the position is stalemate.
    pub is_stalemate: bool,
}

/// Evaluation of a single move.
#[derive(Debug, Clone)]
pub struct MoveEvaluation {
    /// UCI format move string (e.g., "e2e4").
    pub uci: String,
    /// Evaluation after making this move, in centipawns.
    pub eval_cp: i32,
    /// Whether this move captures a piece.
    pub is_capture: bool,
    /// Whether this move gives check.
    pub is_check: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_position_startpos() {
        let parts = vec!["position", "startpos"];
        let board = parse_position(&parts);
        assert_eq!(board, Board::default());
    }

    #[test]
    fn test_parse_position_startpos_with_moves() {
        let parts = vec!["position", "startpos", "moves", "e2e4", "e7e5"];
        let board = parse_position(&parts);
        assert_ne!(board, Board::default());
    }

    #[test]
    fn test_parse_position_fen() {
        let parts = vec!["position", "fen", "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR", "b", "KQkq", "e3", "0", "1"];
        let board = parse_position(&parts);
        assert_eq!(board.side_to_move(), Color::Black);
    }

    #[test]
    fn test_format_move() {
        let m = ChessMove::new(
            Square::from_str("e2").unwrap(),
            Square::from_str("e4").unwrap(),
            None,
        );
        assert_eq!(format_move(m), "e2e4");
    }

    #[test]
    fn test_format_move_promotion() {
        let m = ChessMove::new(
            Square::from_str("e7").unwrap(),
            Square::from_str("e8").unwrap(),
            Some(chess::Piece::Queen),
        );
        assert_eq!(format_move(m), "e7e8q");
    }

    #[test]
    fn test_analyze_position_startpos() {
        let board = Board::default();
        let analysis = analyze_position(&board, 1);
        assert_eq!(analysis.phase, "opening");
        assert_eq!(analysis.piece_count, 32);
        assert_eq!(analysis.side_to_move, "White");
        assert_eq!(analysis.legal_moves.len(), 20); // 20 legal moves from starting position
        assert!(!analysis.is_check);
        assert!(!analysis.is_checkmate);
        assert!(!analysis.is_stalemate);
    }

    #[test]
    fn test_classify_phase() {
        let board = Board::default();
        assert_eq!(classify_phase(&board), "opening");
    }

    #[test]
    fn test_parse_go_depth() {
        let parts = vec!["go", "depth", "8"];
        assert_eq!(parse_go_depth(&parts), Some(8));

        let parts = vec!["go", "infinite"];
        assert_eq!(parse_go_depth(&parts), None);
    }

    #[test]
    fn test_perft_initial_position() {
        let board = Board::default();
        assert_eq!(perft(&board, 1), 20);
        assert_eq!(perft(&board, 2), 400);
    }

    #[test]
    fn test_parse_setoption() {
        let option = parse_setoption("setoption name Depth value 8").unwrap();
        assert_eq!(option.name, "Depth");
        assert_eq!(option.value, "8");

        let option = parse_setoption("setoption name CrewAI value true").unwrap();
        assert_eq!(option.name, "CrewAI");
        assert_eq!(option.value, "true");
    }
}
