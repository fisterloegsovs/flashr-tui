//! Core application state machine and types for flashr-tui.
//!
//! This module defines the `App` struct which represents the entire application state,
//! the `Step` enum for the state machine, and helper types for file picking and flash results.

pub mod device;
pub mod flash;
pub mod iso;
pub mod ui;

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

pub use device::Disk;
pub use iso::IsoKind;

/// Represents a file or directory entry in the file picker.
///
/// # Fields
///
/// * `name` - Display name of the file or directory (includes ".." for parent)
/// * `path` - Full path to the file or directory
/// * `is_dir` - `true` if this entry is a directory, `false` if it's a file
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Application step/state in the state machine.
///
/// The application flows through these states in order:
/// 1. `Image` - User selects an ISO file via file picker
/// 2. `Device` - User selects a target USB device from device list
/// 3. `Confirm` - User reviews selection and confirms before flashing
/// 4. `Flashing` - Flash operation in progress (non-interactive)
/// 5. `Result` - Flash operation completed; displays result
/// 6. `Error` - An error occurred during operation
///
/// User can go back from `Device` → `Image` or from `Confirm` → `Device`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// User is selecting ISO image file from filesystem
    Image,
    /// User is selecting target USB device
    Device,
    /// User is reviewing selection before flashing
    Confirm,
    /// Flashing is in progress; non-interactive
    Flashing,
    /// Flash operation completed; showing result (success or failure)
    Result,
    /// An error occurred; showing error message
    Error,
}

/// Signals the application to exit.
#[derive(Debug)]
pub enum AppExit {
    /// Exit the application cleanly
    Quit,
}

/// Result of a flash operation (success or failure).
///
/// # Fields
///
/// * `ok` - `true` if flash succeeded, `false` if it failed
/// * `message` - User-friendly message describing the result
#[derive(Debug, Clone)]
pub struct FlashResult {
    pub ok: bool,
    pub message: String,
}

/// Main application state struct.
///
/// This struct holds all the mutable state needed by the TUI application, including
/// the current step in the state machine, file picker state, device list, flash progress,
/// and channels for receiving updates from background threads.
///
/// # Fields
///
/// * `step` - Current step in the state machine (Image/Device/Confirm/Flashing/Result/Error)
/// * `image_input` - User-entered path or filename search string for ISO file
/// * `cwd` - Current working directory for file picker navigation
/// * `entries` - Files and directories in the current working directory
/// * `entry_selected` - Index of selected entry in file picker
/// * `iso_kind` - Detected ISO type (Hybrid/NonHybrid/Unknown)
/// * `iso_info` - Human-readable string describing ISO detection result
/// * `devices` - List of available USB devices
/// * `selected` - Index of selected device in device list
/// * `selected_device` - Full `Disk` struct of selected device (or None)
/// * `status` - Status message displayed in UI (empty if no message)
/// * `execute` - `true` to actually flash, `false` for dry-run
/// * `show_all_disks` - `true` to show all disks, `false` for removable only
/// * `flash_progress` - Current flashing progress message (updated from background thread)
/// * `flash_result` - Result of flash operation when complete (success/failure)
/// * `flash_total` - Total bytes to flash (estimated from file size)
/// * `flash_done` - Bytes flashed so far (updated in real-time)
/// * `progress_rx` - Channel receiver for progress updates from flash thread
/// * `result_rx` - Channel receiver for final result from flash thread
pub struct App {
    pub step: Step,
    pub image_input: String,
    pub cwd: PathBuf,
    pub entries: Vec<FileEntry>,
    pub entry_selected: usize,
    pub iso_kind: IsoKind,
    pub iso_info: String,
    pub devices: Vec<Disk>,
    pub selected: usize,
    pub selected_device: Option<Disk>,
    pub status: String,
    pub execute: bool,
    pub show_all_disks: bool,
    pub flash_progress: String,
    pub flash_result: Option<FlashResult>,
    pub flash_total: Option<u64>,
    pub flash_done: u64,
    pub progress_rx: Option<Receiver<String>>,
    pub result_rx: Option<Receiver<Result<(), String>>>,
}

impl App {
    /// Create a new App instance with initial state.
    ///
    /// Initializes the application with the provided CLI arguments and device list.
    ///
    /// # Arguments
    ///
    /// * `image` - Optional path to ISO file (pre-fills image input)
    /// * `device` - Optional device name like "/dev/sdb" (pre-selects device)
    /// * `execute` - Whether to actually flash (true) or dry-run (false)
    /// * `devices` - List of available USB devices
    ///
    /// # Returns
    ///
    /// A new App with initial step either at Image (if no image given) or Device (if image provided).
    pub fn new(
        image: Option<PathBuf>,
        device: Option<String>,
        execute: bool,
        devices: Vec<Disk>,
    ) -> Self {
        let mut selected_device = None;
        let mut selected = 0;

        if let Some(ref device) = device {
            if let Some((idx, disk)) = devices
                .iter()
                .enumerate()
                .find(|(_, d)| d.device_path() == *device)
            {
                selected = idx;
                selected_device = Some(disk.clone());
            }
        }

        let image_input = image
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let image_valid = image.as_ref().map(|p| p.is_file()).unwrap_or(false);

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let entries = load_entries(&cwd);

        let step = if image_valid {
            Step::Device
        } else {
            Step::Image
        };

        let mut status = String::new();
        if image.is_some() && !image_valid {
            status.push_str("Provided --image path must point to an existing file.");
        }
        if devices.is_empty() {
            if !status.is_empty() {
                status.push_str("  ");
            }
            status.push_str("No devices detected. Press r to rescan or a to show all.");
        }

        Self {
            step,
            image_input,
            cwd,
            entries,
            entry_selected: 0,
            iso_kind: IsoKind::Unknown,
            iso_info: String::new(),
            devices,
            selected,
            selected_device,
            status,
            execute,
            show_all_disks: false,
            flash_progress: String::new(),
            flash_result: None,
            flash_total: None,
            flash_done: 0,
            progress_rx: None,
            result_rx: None,
        }
    }

    /// Get the image file path from user input string.
    ///
    /// Trims whitespace and returns the path, or `None` if input is empty.
    ///
    /// # Returns
    ///
    /// `Some(PathBuf)` if input is non-empty, `None` otherwise.
    pub fn image_path(&self) -> Option<PathBuf> {
        let trimmed = self.image_input.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    }

    /// Validate that the user-entered image path points to an existing file.
    ///
    /// If valid, clears the status message. If invalid, sets an error message
    /// and resets ISO type to Unknown.
    ///
    /// # Returns
    ///
    /// `true` if image path is valid (file exists), `false` otherwise.
    pub fn validate_image(&mut self) -> bool {
        match self.image_path() {
            Some(path) if path.is_file() => {
                self.status.clear();
                true
            }
            _ => {
                self.status = "Image path must point to a file.".to_string();
                self.iso_kind = IsoKind::Unknown;
                self.iso_info.clear();
                false
            }
        }
    }

    /// Detect the ISO type (Hybrid/NonHybrid) of the selected image.
    ///
    /// Reads the MBR header of the ISO file to check for a partition table.
    /// Updates `iso_kind` and `iso_info` with the result or error message.
    ///
    /// # Note
    ///
    /// This operation requires only read access to the file — no root privileges needed.
    /// If `iso_kind` is `NonHybrid`, the flash operation will be blocked in the `Confirm` step.
    pub fn refresh_iso_kind(&mut self) {
        let Some(path) = self.image_path() else {
            self.iso_kind = IsoKind::Unknown;
            self.iso_info.clear();
            return;
        };

        match iso::detect(&path) {
            Ok(kind) => {
                self.iso_kind = kind;
                self.iso_info = match kind {
                    IsoKind::Hybrid => "Hybrid ISO detected (raw write).".to_string(),
                    IsoKind::NonHybrid => "Non-hybrid ISO (unsupported).".to_string(),
                    IsoKind::Unknown => "ISO type unknown.".to_string(),
                };
            }
            Err(err) => {
                self.iso_kind = IsoKind::Unknown;
                self.iso_info = format!("ISO check failed: {err}");
            }
        }
    }

    /// Poll for updates from the background flash thread.
    ///
    /// Non-blocking: receives any pending progress messages and checks if flash is complete.
    /// Updates:
    /// - `flash_progress` with latest message
    /// - `flash_done` with bytes flashed so far
    /// - `step` to `Result` when flash thread completes
    /// - `flash_result` with final success/failure message
    ///
    /// Called once per event loop iteration (every 250ms in main loop).
    pub fn poll_flash(&mut self) {
        if let Some(rx) = &self.progress_rx {
            while let Ok(line) = rx.try_recv() {
                if let Some(bytes) = flash::parse_dd_bytes(&line) {
                    self.flash_done = bytes;
                }
                self.flash_progress = line;
            }
        }

        if let Some(rx) = &self.result_rx {
            if let Ok(result) = rx.try_recv() {
                self.progress_rx = None;
                self.result_rx = None;
                self.flash_result = Some(match result {
                    Ok(()) => FlashResult {
                        ok: true,
                        message: "Flash completed successfully.".to_string(),
                    },
                    Err(err) => FlashResult {
                        ok: false,
                        message: err,
                    },
                });
                self.step = Step::Result;
            }
        }
    }

    /// Start the flash operation in a background thread.
    ///
    /// Creates progress and result channels, spawns a background thread to perform the flash,
    /// and transitions to the `Flashing` step.
    ///
    /// # Arguments
    ///
    /// * `image` - Path to the ISO image file
    /// * `device` - Device name (e.g., "/dev/sdb")
    ///
    /// # Note
    ///
    /// The background thread sends progress updates through `progress_rx` and final result
    /// through `result_rx`. Call `poll_flash()` regularly to receive these updates.
    pub fn start_flash(&mut self, image: PathBuf, device: String) {
        let (progress_tx, progress_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();

        self.flash_progress = "Starting...".to_string();
        self.flash_done = 0;
        self.flash_total = std::fs::metadata(&image).map(|m| m.len()).ok();
        self.progress_rx = Some(progress_rx);
        self.result_rx = Some(result_rx);
        self.step = Step::Flashing;

        std::thread::spawn(move || {
            let _ = progress_tx.send(format!("Flashing {} -> {}", image.display(), device));
            let result = flash::flash_image_with_progress(&image, &device, progress_tx);
            let result = result.map_err(|err| err.to_string());
            let _ = result_tx.send(result);
        });
    }
}

/// Load directory contents for the file picker.
///
/// Reads all entries in a directory, filters out hidden files (starting with '.'),
/// and sorts them with directories first, then by name.
/// Always includes a ".." entry for navigating to parent directory (unless already at root).
///
/// # Arguments
///
/// * `cwd` - Current working directory path to list
///
/// # Returns
///
/// Vector of `FileEntry` structs sorted by directory-first, then alphabetically.
/// If directory cannot be read, returns an empty vector.
pub fn load_entries(cwd: &std::path::Path) -> Vec<FileEntry> {
    let mut entries: Vec<FileEntry> = std::fs::read_dir(cwd)
        .ok()
        .into_iter()
        .flat_map(|iter| iter.filter_map(|entry| entry.ok()))
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                return None;
            }
            let is_dir = path.is_dir();
            Some(FileEntry { name, path, is_dir })
        })
        .collect();

    if let Some(parent) = cwd.parent() {
        entries.push(FileEntry {
            name: "..".to_string(),
            path: parent.to_path_buf(),
            is_dir: true,
        });
    }

    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        _ if a.name == ".." => std::cmp::Ordering::Less,
        _ if b.name == ".." => std::cmp::Ordering::Greater,
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    entries
}
