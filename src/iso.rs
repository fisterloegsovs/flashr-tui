//! ISO type detection.
//!
//! Detects whether an ISO image file is a hybrid ISO (with partition table)
//! or non-hybrid (raw data). Only hybrid ISOs can be safely flashed to USB with `dd`.
//!
//! # Important
//!
//! ISO detection requires root privileges due to `losetup` requirements.

use anyhow::{Context, Result};
use std::process::Command;

use crate::device::LsblkOutput;

/// Categorizes an ISO image based on whether it has a partition table.
///
/// - `Unknown` - Could not determine type (usually due to missing root privileges)
/// - `Hybrid` - Has partition table; safe to raw-write to USB with `dd`
/// - `NonHybrid` - No partition table; cannot be flashed with raw `dd` write
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoKind {
    /// ISO type could not be determined
    Unknown,
    /// Hybrid ISO with partition table (safe to raw write)
    Hybrid,
    /// Non-hybrid ISO without partition table (unsafe to raw write)
    NonHybrid,
}

/// Detect the type of an ISO image file.
///
/// Works by:
/// 1. Using `losetup --partscan` to mount the ISO as a loop device
/// 2. Running `lsblk` on the loop device to check if it has partitions
/// 3. Unmounting the loop device
///
/// If the loop device has child devices (partitions), the ISO is `Hybrid`.
/// Otherwise, it's `NonHybrid`.
///
/// # Arguments
///
/// * `image` - Path to the ISO file to analyze
///
/// # Returns
///
/// - `Ok(IsoKind::Hybrid)` if ISO has partition table
/// - `Ok(IsoKind::NonHybrid)` if ISO has no partition table  
/// - `Err` if ISO detection fails (e.g., no root privileges for losetup)
///
/// # Requirements
///
/// Requires root privileges to execute `losetup`. Non-root users will get an error.
pub fn detect(image: &std::path::Path) -> Result<IsoKind> {
    let image_str = image
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid image path"))?;

    let loopdev_output = Command::new("losetup")
        .args(["--find", "--show", "--read-only", "--partscan", image_str])
        .output()
        .context("run losetup")?;

    if !loopdev_output.status.success() {
        return Err(anyhow::anyhow!(
            "losetup failed; need root for ISO inspection"
        ));
    }

    let loopdev = String::from_utf8_lossy(&loopdev_output.stdout)
        .trim()
        .to_string();

    let lsblk_output = Command::new("lsblk")
        .args(["--json", "-o", "NAME,TYPE", "-p", &loopdev])
        .output()
        .context("run lsblk for loop device")?;

    let _ = Command::new("losetup").args(["-d", &loopdev]).status();

    if !lsblk_output.status.success() {
        return Err(anyhow::anyhow!("lsblk failed for loop device"));
    }

    let parsed: LsblkOutput =
        serde_json::from_slice(&lsblk_output.stdout).context("parse lsblk loop output")?;

    let has_partitions = parsed.blockdevices.iter().any(|dev| {
        dev.r#type == "loop" && !dev.children.is_empty()
    });

    Ok(if has_partitions {
        IsoKind::Hybrid
    } else {
        IsoKind::NonHybrid
    })
}
