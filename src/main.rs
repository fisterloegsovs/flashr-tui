//! Flashr TUI - A terminal user interface for flashing Linux ISO images to USB drives.
//!
//! This is the entry point for the application. It handles:
//! - Command-line argument parsing
//! - Terminal setup and cleanup
//! - Main event loop

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use flashr_tui::{App, AppExit, Step};

/// Command-line arguments.
#[derive(Parser, Debug)]
#[command(version, about = "Flash images to USB drives (TUI MVP)")]
struct Cli {
    /// Pre-fill image path, skip to device selection
    #[arg(long)]
    image: Option<std::path::PathBuf>,
    /// Pre-select device (e.g. /dev/sdb)
    #[arg(long)]
    device: Option<String>,
    /// Actually execute dd (default is dry-run)
    #[arg(long)]
    execute: bool,
}

/// Main entry point.
fn main() -> Result<()> {
    let cli = Cli::parse();
    let devices = flashr_tui::device::list(false).unwrap_or_else(|err| {
        eprintln!("Warning: failed to list devices: {err}");
        Vec::new()
    });

    let mut app = App::new(cli.image, cli.device, cli.execute, devices);
    run_tui(&mut app)?;

    Ok(())
}

/// Set up the terminal in raw mode and render the TUI.
///
/// Enables raw mode, enters alternate screen, creates a ratatui Terminal,
/// runs the event loop, and restores normal terminal state on exit.
///
/// # Arguments
///
/// * `app` - Mutable reference to app state
///
/// # Returns
///
/// `Ok(())` if successful, `Err` if terminal setup or event loop failed.
fn run_tui(app: &mut App) -> Result<()> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen).context("enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let result = run_loop(&mut terminal, app);

    disable_raw_mode().ok();
    let mut stdout = io::stdout();
    stdout.execute(LeaveAlternateScreen).ok();

    result
}

/// Main event loop for the TUI.
///
/// Continuously:
/// 1. Polls the background flash thread for updates (if flashing)
/// 2. Draws the current frame
/// 3. Waits for keyboard events with a 250ms timeout
/// 4. Dispatches key events to the UI handler
/// 5. Exits on 'q' key or window close
///
/// # Arguments
///
/// * `terminal` - Mutable reference to ratatui Terminal
/// * `app` - Mutable reference to app state
///
/// # Returns
///
/// `Ok(())` when user exits normally, `Err` if an error occurs.
fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    let mut last_tick = Instant::now();
    loop {
        if app.step == Step::Flashing {
            app.poll_flash();
        }
        terminal.draw(|frame| flashr_tui::ui::draw(frame, app))?;

        let timeout = Duration::from_millis(250).saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if let Some(exit) = flashr_tui::ui::handle_key(app, key) {
                    let AppExit::Quit = exit;
                    return Ok(());
                }
            }
        }

        if last_tick.elapsed() >= Duration::from_millis(250) {
            last_tick = Instant::now();
        }
    }
}
