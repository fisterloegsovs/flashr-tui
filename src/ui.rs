//! Terminal User Interface (TUI) rendering and event handling.
//!
//! This module uses ratatui for rendering UI screens and crossterm for reading keyboard events.
//! It dispatches events to step-specific handlers and renders the appropriate screen based on the current step.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};

use crate::{App, AppExit, Step};

/// ASCII art logo for the title banner, loaded from logo.txt at compile time.
const LOGO: &str = include_str!("logo.txt");

/// Handle a keyboard event for the current step.
///
/// Routes the event to the appropriate step handler.
/// 'q' always quits the application.
///
/// # Arguments
///
/// * `app` - Mutable reference to app state
/// * `key` - The keyboard event to handle
///
/// # Returns
///
/// `Some(AppExit)` to exit the application, `None` to continue running.
pub fn handle_key(app: &mut App, key: KeyEvent) -> Option<AppExit> {
    if key.code == KeyCode::Char('q') {
        return Some(AppExit::Quit);
    }

    match app.step {
        Step::Image => handle_image_step(app, key),
        Step::Device => handle_device_step(app, key),
        Step::Confirm => handle_confirm_step(app, key),
        Step::Flashing => handle_flashing_step(app, key),
        Step::Result => handle_result_step(app, key),
        Step::Error => handle_done_step(app, key),
    }
}

fn handle_image_step(app: &mut App, key: KeyEvent) -> Option<AppExit> {
    match key.code {
        KeyCode::Enter => {
            if !app.image_input.trim().is_empty() {
                if app.validate_image() {
                    app.refresh_iso_kind();
                    app.step = Step::Device;
                }
            } else if let Some(entry) = app.entries.get(app.entry_selected).cloned() {
                if entry.is_dir {
                    app.cwd = entry.path;
                    app.entries = crate::load_entries(&app.cwd);
                    app.entry_selected = 0;
                } else {
                    app.image_input = entry.path.display().to_string();
                    if app.validate_image() {
                        app.refresh_iso_kind();
                        app.step = Step::Device;
                    }
                }
            }
        }
        KeyCode::Backspace => {
            if !app.image_input.is_empty() {
                app.image_input.pop();
            } else if let Some(parent) = app.cwd.parent() {
                app.cwd = parent.to_path_buf();
                app.entries = crate::load_entries(&app.cwd);
                app.entry_selected = 0;
            }
        }
        KeyCode::Up => {
            if app.entry_selected > 0 {
                app.entry_selected -= 1;
            }
        }
        KeyCode::Down => {
            if app.entry_selected + 1 < app.entries.len() {
                app.entry_selected += 1;
            }
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.image_input.clear();
        }
        KeyCode::Char(c) => {
            if !key.modifiers.contains(KeyModifiers::CONTROL) {
                app.image_input.push(c);
            }
        }
        _ => {}
    }

    None
}

fn handle_device_step(app: &mut App, key: KeyEvent) -> Option<AppExit> {
    match key.code {
        KeyCode::Up => {
            if app.selected > 0 {
                app.selected -= 1;
            }
        }
        KeyCode::Down => {
            if app.selected + 1 < app.devices.len() {
                app.selected += 1;
            }
        }
        KeyCode::Char('r') => {
            match crate::device::list(app.show_all_disks) {
                Ok(devices) => {
                    app.devices = devices;
                    app.status = if app.devices.is_empty() {
                        "No devices detected.".to_string()
                    } else {
                        "Devices re-scanned.".to_string()
                    };
                }
                Err(err) => {
                    app.devices = Vec::new();
                    app.status = format!("Rescan failed: {err}");
                }
            }
            app.selected = 0;
        }
        KeyCode::Char('a') => {
            app.show_all_disks = !app.show_all_disks;
            match crate::device::list(app.show_all_disks) {
                Ok(devices) => {
                    app.devices = devices;
                    app.status = if app.show_all_disks {
                        "Showing all disks (be careful).".to_string()
                    } else {
                        "Showing removable disks only.".to_string()
                    };
                    if app.devices.is_empty() {
                        app.status = "No devices detected.".to_string();
                    }
                }
                Err(err) => {
                    app.devices = Vec::new();
                    app.status = format!("Disk list failed: {err}");
                }
            }
            app.selected = 0;
        }
        KeyCode::Enter => {
            if let Some(disk) = app.devices.get(app.selected).cloned() {
                app.selected_device = Some(disk);
                if app.iso_kind == crate::iso::IsoKind::Unknown {
                    app.refresh_iso_kind();
                }
                app.step = Step::Confirm;
            } else {
                app.status = "No removable devices found.".to_string();
                app.step = Step::Error;
            }
        }
        KeyCode::Char('b') => {
            app.step = Step::Image;
        }
        _ => {}
    }

    None
}

fn handle_confirm_step(app: &mut App, key: KeyEvent) -> Option<AppExit> {
    match key.code {
        KeyCode::Char('f') => {
            if app.iso_kind == crate::iso::IsoKind::NonHybrid {
                app.status = "ISO has no partition table; hybrid ISO required.".to_string();
                app.step = Step::Error;
            } else if let (Some(image), Some(device)) =
                (app.image_path(), app.selected_device.clone())
            {
                if app.execute {
                    app.start_flash(image, device.device_path());
                } else {
                    app.flash_result = Some(crate::FlashResult {
                        ok: true,
                        message: format!(
                            "Dry run: would flash {} to {}",
                            image.display(),
                            device.device_path()
                        ),
                    });
                    app.step = Step::Result;
                }
            }
        }
        KeyCode::Char('b') => {
            app.step = Step::Device;
        }
        _ => {}
    }

    None
}

fn handle_flashing_step(_app: &mut App, _key: KeyEvent) -> Option<AppExit> {
    None
}

fn handle_result_step(_app: &mut App, _key: KeyEvent) -> Option<AppExit> {
    None
}

fn handle_done_step(_app: &mut App, _key: KeyEvent) -> Option<AppExit> {
    None
}

/// Render the entire TUI screen.
///
/// Renders a 3-section layout:
/// 1. **Top** - Title bar
/// 2. **Middle** - Content specific to current step
/// 3. **Bottom** - Footer with status message and key bindings
///
/// Delegates to step-specific draw functions for the middle section.
///
/// # Arguments
///
/// * `frame` - ratatui Frame to render to
/// * `app` - Current application state (immutable)
pub fn draw(frame: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let title = Paragraph::new(LOGO)
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, chunks[0]);

    match app.step {
        Step::Image => draw_image_step(frame, app, chunks[1]),
        Step::Device => draw_device_step(frame, app, chunks[1]),
        Step::Confirm => draw_confirm_step(frame, app, chunks[1]),
        Step::Flashing => draw_flashing_step(frame, app, chunks[1]),
        Step::Result => draw_result_step(frame, app, chunks[1]),
        Step::Error => draw_error_step(frame, app, chunks[1]),
    }

    let footer = Paragraph::new(status_line(app))
        .style(Style::default().fg(Color::Gray))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);
}

fn draw_image_step(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(5)])
        .split(area);

    let header = Text::from(vec![
        Line::from("Step 1: Choose image file"),
        Line::from(format!("Current dir: {}", app.cwd.display())),
        Line::from(Span::styled(
            format!("Input: {}", app.image_input),
            Style::default().fg(Color::Yellow),
        )),
    ]);

    let block = Block::default().borders(Borders::ALL).title("Image");
    let paragraph = Paragraph::new(header)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, sections[0]);

    let items: Vec<ListItem> = app
        .entries
        .iter()
        .map(|entry| {
            let label = if entry.is_dir {
                format!("{}/", entry.name)
            } else {
                entry.name.clone()
            };
            ListItem::new(Line::from(label))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Files"))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("> ");

    let mut state = ratatui::widgets::ListState::default();
    if !app.entries.is_empty() {
        state.select(Some(app.entry_selected));
    }

    frame.render_stateful_widget(list, sections[1], &mut state);
}

fn draw_device_step(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    if app.devices.is_empty() {
        let text = Text::from(vec![
            Line::from("No devices detected."),
            Line::from("Press 'r' to rescan or 'a' to show all disks."),
        ]);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Select Device");
        let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
        return;
    }

    let items: Vec<ListItem> = app
        .devices
        .iter()
        .map(|disk| {
            let label = format!(
                "{}  {}  {}",
                disk.device_path(),
                disk.size,
                if disk.model.is_empty() {
                    "(unknown)"
                } else {
                    disk.model.as_str()
                }
            );
            ListItem::new(Line::from(label))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Select Device"),
        )
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("> ");

    let mut state = ratatui::widgets::ListState::default();
    if !app.devices.is_empty() {
        state.select(Some(app.selected));
    }

    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_confirm_step(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let image = app.image_input.trim();
    let device = app
        .selected_device
        .as_ref()
        .map(|d| d.device_path())
        .unwrap_or_else(|| "<none>".to_string());

    let mode = if app.execute { "EXECUTE" } else { "DRY RUN" };

    let text = Text::from(vec![
        Line::from("Step 3: Confirm"),
        Line::from(format!("Image : {image}")),
        Line::from(format!("Device: {device}")),
        Line::from(format!("Mode  : {mode}")),
        Line::from(format!("ISO   : {}", iso_info_line(app))),
        Line::from("Press 'f' to flash, 'b' to go back."),
    ]);

    let block = Block::default().borders(Borders::ALL).title("Confirm");
    let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_flashing_step(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let (percent, label) = if let Some(total) = app.flash_total {
        let percent = if total == 0 {
            0
        } else {
            ((app.flash_done.saturating_mul(100)) / total) as u16
        };
        let label = format!("{} / {} bytes", app.flash_done, total);
        (percent, label)
    } else {
        (0, "Working...".to_string())
    };

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(3)])
        .split(area);

    let header = Text::from(vec![
        Line::from("Flashing in progress"),
        Line::from(app.flash_progress.as_str()),
    ]);

    let block = Block::default().borders(Borders::ALL).title("Flashing");
    let paragraph = Paragraph::new(header)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, sections[0]);

    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title("Progress"))
        .gauge_style(Style::default().fg(Color::Green))
        .label(label)
        .percent(percent);
    frame.render_widget(gauge, sections[1]);
}

fn draw_result_step(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let result = app.flash_result.as_ref();
    let (title, style, message) = match result {
        Some(result) if result.ok => (
            "Success",
            Style::default().fg(Color::Green),
            result.message.as_str(),
        ),
        Some(result) => (
            "Failed",
            Style::default().fg(Color::Red),
            result.message.as_str(),
        ),
        None => ("Result", Style::default().fg(Color::Gray), "No result."),
    };

    let text = Text::from(vec![
        Line::from(Span::styled(title, style.add_modifier(Modifier::BOLD))),
        Line::from(message),
        Line::from("Press 'q' to quit."),
    ]);
    let block = Block::default().borders(Borders::ALL).title("Result");
    let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_error_step(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let text = Text::from(vec![
        Line::from(Span::styled(
            "Error",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(app.status.as_str()),
        Line::from("Press 'q' to quit."),
    ]);
    let block = Block::default().borders(Borders::ALL).title("Error");
    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, area);
}

fn status_line(app: &App) -> Line<'static> {
    let keys = match app.step {
        Step::Image => "Up/Down=select  Enter=open/select  Backspace=up  Ctrl+U=clear  q=quit",
        Step::Device => "Up/Down=select  Enter=next  r=rescan  a=all  b=back  q=quit",
        Step::Confirm => "f=flash  b=back  q=quit",
        Step::Flashing => "Flashing...  q=quit",
        Step::Result | Step::Error => "q=quit",
    };

    let mut spans = vec![Span::raw(keys)];
    if !app.status.is_empty() {
        spans.push(Span::raw("  |  "));
        spans.push(Span::styled(
            app.status.clone(),
            Style::default().fg(Color::Red),
        ));
    }

    Line::from(spans)
}

fn iso_info_line(app: &App) -> String {
    if app.iso_info.is_empty() {
        match app.iso_kind {
            crate::iso::IsoKind::Hybrid => "Hybrid ISO detected (raw write).".to_string(),
            crate::iso::IsoKind::NonHybrid => "Non-hybrid ISO (unsupported).".to_string(),
            crate::iso::IsoKind::Unknown => "Unknown ISO type.".to_string(),
        }
    } else {
        app.iso_info.clone()
    }
}
