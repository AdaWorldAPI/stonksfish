//! Stonksfish UCI engine binary.
//!
//! Run this binary to start Stonksfish in UCI mode, compatible with
//! any UCI-compliant chess GUI or the lichess-bot bridge.
//!
//! ```sh
//! cargo run --bin stonksfish-uci --release
//! ```

fn main() {
    stonksfish::uci::run_uci_loop();
}
