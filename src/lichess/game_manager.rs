//! Concurrent game handler.
//!
//! Each active game runs in its own tokio task. The game manager:
//! - Streams game state from Lichess
//! - Applies opponent moves
//! - Computes engine responses via Bot::choose_move()
//! - Collects positions and decisions for the harvester
//! - Optionally runs what-if branching on critical positions

use chess::{Board, ChessMove, Color, Game, MoveGen};
use licheszter::client::Licheszter;
use licheszter::models::board::{BoardState, Challenger};
use log::{debug, error, info, warn};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;

use crate::engine::evaluation::simple::evaluate_board;
use crate::engine::player::{Bot, Player};
use crate::harvest::{GameRecord, HarvestSink, MoveRecord};
use crate::uci::{classify_phase, count_pieces};
use crate::whatif::{generate_branch_tree, BranchConfig};

/// Play a single game on Lichess.
///
/// This function runs in its own tokio task and handles the complete
/// game lifecycle: determining color, making moves, recording positions,
/// and optionally running what-if analysis.
pub async fn play_game(
    client: Licheszter,
    game_id: &str,
    depth: u8,
    whatif_enabled: bool,
    bot_username: &str,
    harvester: Arc<Mutex<Box<dyn HarvestSink + Send>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bot = Bot { depth };
    let mut game = Game::new();
    let mut bot_color = Color::White;
    let mut game_record = GameRecord::new(game_id.to_string());
    let mut move_number: u32 = 0;

    let mut stream = client
        .stream_game_state(game_id)
        .await
        .map_err(|e| format!("Stream error: {:?}", e))?;

    while let Ok(Some(state)) = stream.try_next().await {
        match state {
            BoardState::GameFull(game_full) => {
                // Determine our color
                bot_color = match &game_full.white {
                    Challenger::LightUser(white_user) => {
                        if white_user.username.to_lowercase() == bot_username.to_lowercase() {
                            Color::White
                        } else {
                            Color::Black
                        }
                    }
                    _ => Color::Black,
                };

                // Record game metadata
                let (white_name, black_name) = match &game_full.white {
                    Challenger::LightUser(w) => {
                        let b_name = match &game_full.black {
                            Challenger::LightUser(b) => b.username.clone(),
                            _ => "unknown".to_string(),
                        };
                        (w.username.clone(), b_name)
                    }
                    _ => ("unknown".to_string(), "unknown".to_string()),
                };

                game_record.white = white_name;
                game_record.black = black_name;
                game_record.bot_color = format!("{:?}", bot_color);

                info!(
                    "[{}] Playing as {:?}. {} vs {}",
                    game_id, bot_color, game_record.white, game_record.black
                );

                // If we're white, make the first move
                if bot_color == Color::White {
                    let board = game.current_position();
                    let start = Instant::now();
                    let chosen_move = bot.choose_move(&board);
                    let think_time = start.elapsed();

                    let uci_move = format!("{}", chosen_move);
                    let eval = evaluate_board(&board);

                    // Record the move
                    game_record.moves.push(MoveRecord {
                        move_number: 1,
                        side: "white".to_string(),
                        uci: uci_move.clone(),
                        fen_before: format!("{}", board),
                        eval_cp: eval,
                        phase: classify_phase(&board).to_string(),
                        piece_count: count_pieces(&board),
                        think_time_ms: think_time.as_millis() as u64,
                        is_book: false,
                        alternatives: count_legal_moves(&board),
                    });

                    client
                        .make_move(game_id, &uci_move, false)
                        .await
                        .map_err(|e| format!("Move error: {:?}", e))?;
                }
            }

            BoardState::GameState(game_state) => {
                if game_state.status != "started" {
                    // Game ended
                    game_record.result = game_state.status.clone();
                    info!("[{}] Game ended: {}", game_id, game_state.status);

                    // Send completed game to harvester
                    if let Err(e) = harvester.lock().await.record_game(game_record.clone()).await
                    {
                        warn!("[{}] Harvest error: {:?}", game_id, e);
                    }
                    break;
                }

                // Parse the latest move from the move string
                let moves_str = &game_state.moves;
                if moves_str.is_empty() {
                    continue;
                }

                // Rebuild game state from full move list
                let move_list: Vec<&str> = moves_str.split_whitespace().collect();
                move_number = move_list.len() as u32;

                // Apply the last move if it's new
                let last_move_str = move_list.last().unwrap_or(&"");
                if let Ok(chess_move) = ChessMove::from_str(last_move_str) {
                    let move_result = game.make_move(chess_move);
                    if !move_result {
                        // Game state diverged - rebuild from scratch
                        game = Game::new();
                        for ms in &move_list {
                            if let Ok(m) = ChessMove::from_str(ms) {
                                game.make_move(m);
                            }
                        }
                    }

                    // Check if it's our turn
                    if game.side_to_move() == bot_color {
                        let board = game.current_position();

                        // Check for game-over positions
                        if MoveGen::new_legal(&board).len() == 0 {
                            debug!("[{}] No legal moves, game should end", game_id);
                            continue;
                        }

                        // Optional: what-if branching on critical positions
                        if whatif_enabled && is_critical_position(&board) {
                            let branch_config = BranchConfig::quick();
                            let fen = format!("{}", board);
                            if let Some(tree) = generate_branch_tree(&fen, &branch_config) {
                                if let Err(e) = harvester
                                    .lock()
                                    .await
                                    .record_branch_tree(game_id, &tree)
                                    .await
                                {
                                    debug!("[{}] Branch harvest error: {:?}", game_id, e);
                                }
                            }
                        }

                        // Compute our move
                        let start = Instant::now();
                        let chosen_move = bot.choose_move(&board);
                        let think_time = start.elapsed();

                        let uci_move = format!("{}", chosen_move);
                        let eval = evaluate_board(&board);
                        let side = if bot_color == Color::White {
                            "white"
                        } else {
                            "black"
                        };

                        // Record the move
                        game_record.moves.push(MoveRecord {
                            move_number,
                            side: side.to_string(),
                            uci: uci_move.clone(),
                            fen_before: format!("{}", board),
                            eval_cp: eval,
                            phase: classify_phase(&board).to_string(),
                            piece_count: count_pieces(&board),
                            think_time_ms: think_time.as_millis() as u64,
                            is_book: false,
                            alternatives: count_legal_moves(&board),
                        });

                        // Send move to Lichess
                        if let Err(e) = client.make_move(game_id, &uci_move, false).await {
                            error!("[{}] Failed to send move {}: {:?}", game_id, uci_move, e);
                        }
                    }
                } else {
                    warn!("[{}] Could not parse move: '{}'", game_id, last_move_str);
                }
            }

            other => {
                debug!("[{}] Other state: {:?}", game_id, other);
            }
        }
    }

    Ok(())
}

/// Count legal moves in a position (for recording decision breadth).
fn count_legal_moves(board: &Board) -> u32 {
    MoveGen::new_legal(board).len() as u32
}

/// Determine if a position is "critical" and warrants what-if analysis.
///
/// Critical positions are those where the evaluation is close to 0
/// (unclear) or where there's a significant material imbalance that
/// could lead to complex tactics.
fn is_critical_position(board: &Board) -> bool {
    let eval = evaluate_board(board).abs();
    let pieces = count_pieces(board);

    // Critical if eval is close to equal and in middlegame
    (eval < 100 && pieces > 10 && pieces < 28)
        // Or if there's a big swing potential (complex tactics)
        || (eval > 200 && eval < 500 && pieces > 14)
}
