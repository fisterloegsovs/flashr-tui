//! Device detection and listing using `lsblk`.
//!
//! This module queries the Linux block device (lsblk) command to enumerate
//! USB and removable storage devices, then presents them as a list of `Disk` structs.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::Command;

/// Represents a block storage device (USB drive, hard disk, etc.).
///
/// # Fields
///
/// * `name` - Device name without path prefix (e.g., "sdb", "sdc1")
/// * `model` - Human-readable model string (e.g., "SanDisk Cruzer")
/// * `size` - Human-readable size string (e.g., "57.3G", "1.8M")
#[derive(Debug, Clone)]
pub struct Disk {
    pub name: String,
    pub model: String,
    pub size: String,
}

impl Disk {
    /// Get the full device path for this disk.
    ///
    /// # Returns
    ///
    /// Full path like "/dev/sdb"
    pub fn device_path(&self) -> String {
        format!("/dev/{}", self.name)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct LsblkOutput {
    pub blockdevices: Vec<LsblkDevice>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LsblkDevice {
    pub name: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub rm: Option<bool>,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub fstype: Option<String>,
    #[serde(default)]
    pub children: Vec<LsblkDevice>,
}

/// List available block devices on the system.
///
/// Runs `lsblk --json` and filters for block devices (`type == "disk"`).
/// If `show_all` is false, further filters to only removable devices (`rm == 1`).
///
/// # Arguments
///
/// * `show_all` - If `true`, list all disk devices; if `false`, list only removable devices
///
/// # Returns
///
/// `Ok(Vec<Disk>)` with the list of devices, or an error if `lsblk` fails or output cannot be parsed.
///
/// # Errors
///
/// Returns an error if:
/// - `lsblk` command is not available or fails to execute
/// - `lsblk` output cannot be parsed as JSON
pub fn list(show_all: bool) -> Result<Vec<Disk>> {
    let output = Command::new("lsblk")
        .args(["--json", "-o", "NAME,MODEL,SIZE,RM,TYPE"])
        .output()
        .context("run lsblk")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err(anyhow::anyhow!("lsblk failed"));
        }
        return Err(anyhow::anyhow!("lsblk failed: {stderr}"));
    }

    let parsed: LsblkOutput = serde_json::from_slice(&output.stdout).context("parse lsblk output")?;

    let disks = parsed
        .blockdevices
        .into_iter()
        .filter(|dev| dev.r#type == "disk")
        .filter(|dev| show_all || dev.rm.unwrap_or(false))
        .map(|dev| Disk {
            name: dev.name,
            model: dev.model.unwrap_or_default(),
            size: dev.size.unwrap_or_default(),
        })
        .collect();

    Ok(disks)
}
