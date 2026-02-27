//! Flash ISO image to USB device with progress tracking.
//!
//! This module handles the actual flashing operation using `dd`, streams progress updates
//! through an mpsc channel, and optionally labels the USB drive based on the ISO filename.
//!
//! When not running as root, privileged commands (`dd`, `partprobe`, labeling tools)
//! are automatically wrapped with `pkexec` or `sudo` for privilege elevation.

use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;

use crate::device::LsblkOutput;
use crate::iso::IsoKind;

/// Check if the current process is running as root (euid == 0).
pub fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Find an available privilege elevation tool.
///
/// Checks for `pkexec` first (graphical prompt on desktop systems),
/// then falls back to `sudo` (terminal prompt).
///
/// # Returns
///
/// `Some("pkexec")` or `Some("sudo")` if found, `None` if neither is available.
pub fn find_elevator() -> Option<&'static str> {
    for tool in &["pkexec", "sudo"] {
        if Command::new("which")
            .arg(tool)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(tool);
        }
    }
    None
}

/// Build a `Command` that runs a program with privilege elevation if needed.
///
/// If already root, returns `Command::new(program)` directly.
/// If not root, wraps the command with the given elevator (e.g., `pkexec` or `sudo`).
///
/// # Arguments
///
/// * `program` - The program to run (e.g., "dd", "partprobe")
/// * `elevator` - Optional elevator tool name (e.g., "pkexec", "sudo")
///
/// # Returns
///
/// A `Command` ready for argument addition and execution.
fn elevated_command(program: &str, elevator: Option<&str>) -> Command {
    match elevator {
        Some(elev) if !is_root() => {
            let mut cmd = Command::new(elev);
            cmd.arg(program);
            cmd
        }
        _ => Command::new(program),
    }
}

/// Flash an ISO image to a USB device with live progress streaming.
///
/// This function:
/// 1. Validates that the ISO is hybrid (safe to raw-write)
/// 2. Spawns a `dd` process to copy the image to the device
/// 3. Reads progress lines from `dd` stderr and sends them via the progress channel
/// 4. Waits for `dd` to complete and validates success
/// 5. Refreshes the kernel's partition table with `partprobe`
/// 6. Attempts to label the device based on ISO filename
///
/// When not running as root, `dd` and post-flash commands are automatically
/// elevated via `pkexec` or `sudo`.
///
/// # Arguments
///
/// * `image` - Path to the ISO file
/// * `device` - Device path (e.g., "/dev/sdb")
/// * `progress` - Channel to send progress messages to
///
/// # Returns
///
/// `Ok(())` if flash succeeded, `Err` if any step failed.
///
/// # Errors
///
/// Returns an error if:
/// - ISO is NonHybrid or type cannot be determined
/// - No privilege elevation tool is available when not running as root
/// - `dd` command fails to execute or returns non-zero
/// - Reading progress from `dd` fails
///
/// # Note
///
/// Labeling failures are non-fatal and reported in progress messages but don't cause the function to fail.
pub fn flash_image_with_progress(
    image: &Path,
    device: &str,
    progress: mpsc::Sender<String>,
) -> Result<()> {
    match crate::iso::detect(image)? {
        IsoKind::Hybrid => {}
        IsoKind::NonHybrid => {
            return Err(anyhow::anyhow!(
                "ISO has no partition table; hybrid ISO required"
            ));
        }
        IsoKind::Unknown => {
            return Err(anyhow::anyhow!("Unable to determine ISO type"));
        }
    }

    // Find an elevator if we're not root
    let elevator = if is_root() {
        None
    } else {
        let elev = find_elevator().ok_or_else(|| {
            anyhow::anyhow!(
                "Root privileges required for flashing. \
                 Install pkexec or sudo, or run with: sudo flashr-tui --execute"
            )
        })?;
        let _ = progress.send(format!(
            "Not running as root; using '{}' for privilege elevation",
            elev
        ));
        Some(elev)
    };

    let mut child = elevated_command("dd", elevator)
        .arg(format!("if={}", image.display()))
        .arg(format!("of={}", device))
        .arg("bs=4M")
        .arg("status=progress")
        .arg("oflag=sync")
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .context("run dd (do you have permission?)")?;

    if let Some(mut stderr) = child.stderr.take() {
        let mut buf = [0u8; 4096];
        let mut pending = String::new();
        loop {
            let read = stderr.read(&mut buf).context("read dd output")?;
            if read == 0 {
                break;
            }
            let chunk = String::from_utf8_lossy(&buf[..read]);
            for ch in chunk.chars() {
                if ch == '\n' || ch == '\r' {
                    let line = pending.trim();
                    if !line.is_empty() {
                        let _ = progress.send(line.to_string());
                    }
                    pending.clear();
                } else {
                    pending.push(ch);
                }
            }
        }

        let line = pending.trim();
        if !line.is_empty() {
            let _ = progress.send(line.to_string());
        }
    }

    let status = child.wait().context("wait for dd")?;
    if !status.success() {
        return Err(anyhow::anyhow!("dd failed"));
    }

    Command::new("sync").status().ok();

    let _ = elevated_command("partprobe", elevator).arg(device).status();

    if let Ok(Some(message)) = label_device_from_iso(image, device, elevator) {
        let _ = progress.send(message);
    }

    Ok(())
}

/// Parse byte count from a dd progress line.
///
/// Extracts the leading digits from a line of `dd` output, which typically looks like:
/// `"1234567890 bytes (1.2G) copied..."`
///
/// # Arguments
///
/// * `line` - A line from dd's progress output
///
/// # Returns
///
/// `Some(bytes)` if line starts with digits, `None` otherwise.
pub fn parse_dd_bytes(line: &str) -> Option<u64> {
    let mut digits = String::new();
    for ch in line.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            break;
        }
    }
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// Label the USB device based on the ISO filename.
///
/// Extracts the ISO filename (without extension), sanitizes it for use as a partition label,
/// queries the device's filesystems with `lsblk`, and uses the appropriate labeling tool
/// (fatlabel, ntfslabel, or e2label) to set the label on the first writable partition.
///
/// # Arguments
///
/// * `image` - Path to the ISO file
/// * `device` - Device path (e.g., "/dev/sdb")
/// * `elevator` - Optional privilege elevation tool (e.g., "pkexec", "sudo")
///
/// # Returns
///
/// `Ok(Some(message))` with a success or error message, or `Ok(None)` if no suitable partition found.
/// Errors are non-fatal and reported as messages.
fn label_device_from_iso(
    image: &Path,
    device: &str,
    elevator: Option<&str>,
) -> Result<Option<String>> {
    let Some(label_base) = image.file_stem().and_then(|s| s.to_str()) else {
        return Ok(None);
    };

    let label_base = sanitize_label(label_base);
    if label_base.is_empty() {
        return Ok(None);
    }

    let output = Command::new("lsblk")
        .args(["--json", "-o", "NAME,FSTYPE", "-p", device])
        .output()
        .context("run lsblk for fstype")?;

    if !output.status.success() {
        return Err(anyhow::anyhow!("lsblk failed for fstype"));
    }

    let parsed: LsblkOutput =
        serde_json::from_slice(&output.stdout).context("parse lsblk fstype output")?;

    let mut target: Option<(String, String)> = None;
    for dev in parsed.blockdevices {
        if dev.r#type == "disk" {
            for child in dev.children {
                if let Some(fstype) = child.fstype.clone() {
                    if is_supported_fstype(&fstype) {
                        target = Some((format!("/dev/{}", child.name), fstype));
                        break;
                    }
                }
            }
        }
    }

    let Some((partition, fstype)) = target else {
        return Ok(None);
    };

    let (label, tool, extra_args) = label_command(&partition, &fstype, &label_base);
    let status = elevated_command(tool, elevator).args(extra_args).status();
    match status {
        Ok(status) if status.success() => Ok(Some(format!("Label set to {label}"))),
        Ok(_) => Ok(Some("Labeling failed".to_string())),
        Err(_) => Ok(Some("Labeling tool not available".to_string())),
    }
}

/// Sanitize a string for use as a filesystem label.
///
/// Keeps alphanumeric characters, hyphens, and underscores.
/// Replaces spaces with underscores.
/// Removes all other characters.
///
/// # Arguments
///
/// * `input` - String to sanitize (typically an ISO filename)
///
/// # Returns
///
/// Sanitized label string safe for filesystem labels.
fn sanitize_label(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else if ch.is_ascii_whitespace() {
            out.push('_');
        }
    }
    out
}

/// Check if a filesystem type supports labeling.
///
/// Returns `true` for FAT, NTFS, and EXT filesystems which can be labeled
/// with `fatlabel`, `ntfslabel`, or `e2label` respectively.
fn is_supported_fstype(fstype: &str) -> bool {
    matches!(
        fstype,
        "vfat" | "fat" | "fat16" | "fat32" | "ntfs" | "ext2" | "ext3" | "ext4"
    )
}

/// Get the appropriate label command and arguments for a filesystem type.
///
/// Returns the tool name and arguments to execute for labeling the partition.
/// Different filesystems have different tools and max label lengths:
/// - FAT: `fatlabel`, max 11 characters
/// - NTFS: `ntfslabel`, max 32 characters
/// - EXT: `e2label`, max 16 characters
fn label_command<'a>(
    partition: &'a str,
    fstype: &'a str,
    base: &'a str,
) -> (String, &'a str, Vec<String>) {
    match fstype {
        "vfat" | "fat" | "fat16" | "fat32" => {
            let label = truncate_label(base, 11);
            (
                label.clone(),
                "fatlabel",
                vec![partition.to_string(), label],
            )
        }
        "ntfs" => {
            let label = truncate_label(base, 32);
            (
                label.clone(),
                "ntfslabel",
                vec![partition.to_string(), label],
            )
        }
        "ext2" | "ext3" | "ext4" => {
            let label = truncate_label(base, 16);
            (label.clone(), "e2label", vec![partition.to_string(), label])
        }
        _ => (base.to_string(), "false", Vec::new()),
    }
}

/// Truncate a label to a maximum length.
///
/// Safely truncates UTF-8 strings by character count (not byte count).
fn truncate_label(input: &str, max_len: usize) -> String {
    input.chars().take(max_len).collect()
}
