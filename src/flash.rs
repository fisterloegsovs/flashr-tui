//! Flash ISO image to USB device with progress tracking.
//!
//! This module handles the actual flashing operation using `dd`, streams progress updates
//! through an mpsc channel, and optionally labels the USB drive based on the ISO filename.
//!
//! When not running as root, privileged commands (`dd`, `partprobe`, labeling tools)
//! are automatically wrapped with `pkexec` or `sudo` for privilege elevation.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;

use crate::device::{DevicePath, LsblkOutput};
use crate::iso::IsoKind;

/// Check if the current process is running as root (euid == 0).
pub fn is_root() -> bool {
    nix::unistd::geteuid().is_root()
}

/// Find an available privilege elevation tool.
///
/// Checks for `sudo` first (terminal prompt with credential caching),
/// then falls back to `pkexec` (graphical prompt, no caching).
/// `sudo` is preferred because its credential cache (default 15 minutes)
/// avoids repeated password prompts across multiple commands.
///
/// # Returns
///
/// `Some("sudo")` or `Some("pkexec")` if found, `None` if neither is available.
pub fn find_elevator() -> Option<&'static str> {
    ["sudo", "pkexec"]
        .into_iter()
        .find(|tool| which::which(tool).is_ok())
}

/// Check if the `isohybrid` tool (from syslinux) is available.
pub fn has_isohybrid() -> bool {
    which::which("isohybrid").is_ok()
}

/// Convert an ISO to hybrid format using the `isohybrid` tool.
///
/// This modifies the ISO file in-place by adding an MBR partition table,
/// making it suitable for raw-writing to USB drives.
pub fn convert_isohybrid(image: &Path) -> Result<()> {
    let status = Command::new("isohybrid")
        .arg(image)
        .status()
        .context("run isohybrid")?;
    if !status.success() {
        return Err(anyhow::anyhow!("isohybrid conversion failed"));
    }
    Ok(())
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
    user_confirmed_wipe: bool,
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

    // Validate device path (symlink, block device) via DevicePath, then
    // check mount safety separately.
    let device_path = DevicePath::validate(device)?;
    ensure_device_safe(device_path.as_str(), user_confirmed_wipe)?;

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
        // Prime the credential cache so the user only enters their password
        // once. `sudo -v` validates credentials without running a command;
        // subsequent sudo calls within the timeout window (default 15 min)
        // won't re-prompt.
        if elev == "sudo" {
            let _ = progress.send("Requesting sudo access...".to_string());
            let prime = Command::new("sudo")
                .arg("-v")
                .status()
                .context("failed to obtain sudo credentials")?;
            if !prime.success() {
                return Err(anyhow::anyhow!("sudo authentication failed"));
            }
        }
        Some(elev)
    };

    let dev = device_path.as_str();
    wipe_device_if_needed(dev, elevator, &progress)?;

    let mut child = elevated_command("dd", elevator)
        .arg(format!("if={}", image.display()))
        .arg(format!("of={}", dev))
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

    // Verify flash integrity before labeling (labeling modifies the device).
    let _ = progress.send("Verifying flash integrity...".to_string());
    match verify_flash(image, dev, elevator, &progress) {
        Ok(true) => {
            let _ = progress.send("Verification passed: SHA-256 checksums match.".to_string());
        }
        Ok(false) => {
            return Err(anyhow::anyhow!(
                "Verification failed: device content does not match source image"
            ));
        }
        Err(e) => {
            let _ = progress.send(format!("Verification skipped: {e}"));
        }
    }

    // Post-flash privileged operations (partprobe + label).
    let label_result = label_device_post_flash(image, dev, elevator);
    if let Ok(Some(message)) = &label_result {
        let _ = progress.send(message.clone());
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

/// Perform post-flash privileged operations using direct command invocations.
///
/// Runs `partprobe` to refresh the kernel partition table, then attempts to
/// label the USB partition based on the ISO filename. Each tool is invoked
/// directly via `Command` (no shell interpretation) to avoid injection risks.
fn label_device_post_flash(
    image: &Path,
    device: &str,
    elevator: Option<&str>,
) -> Result<Option<String>> {
    let label_base = image
        .file_stem()
        .and_then(|s| s.to_str())
        .map(sanitize_label)
        .unwrap_or_default();

    // Run partprobe directly (no shell).
    let _ = elevated_command("partprobe", elevator)
        .arg(device)
        .status()
        .context("run partprobe")?;

    // Try to discover partitions and apply a label.
    if let Some((label, tool, args)) = resolve_label_command(image, device) {
        let status = elevated_command(&tool, elevator)
            .args(&args)
            .status();
        return match status {
            Ok(s) if s.success() => Ok(Some(format!("Label set to {label}"))),
            Ok(_) => Ok(Some("Labeling failed".to_string())),
            Err(_) => Ok(Some("Labeling tool not available".to_string())),
        };
    }

    // If lsblk didn't show partitions yet, retry now that partprobe has run.
    if !label_base.is_empty() {
        if let Some((label, tool, args)) = resolve_label_command(image, device) {
            let status = elevated_command(&tool, elevator)
                .args(&args)
                .status();
            return match status {
                Ok(s) if s.success() => Ok(Some(format!("Label set to {label}"))),
                Ok(_) => Ok(Some("Labeling failed".to_string())),
                Err(_) => Ok(Some("Labeling tool not available".to_string())),
            };
        }
    }

    Ok(None)
}

/// Resolve the label tool, args, and label string for a device partition.
///
/// Runs an unprivileged `lsblk` to discover the first partition with a
/// supported filesystem and returns the label command to apply.
fn resolve_label_command(image: &Path, device: &str) -> Option<(String, String, Vec<String>)> {
    let label_base = image
        .file_stem()
        .and_then(|s| s.to_str())
        .map(sanitize_label)?;

    if label_base.is_empty() {
        return None;
    }

    let output = Command::new("lsblk")
        .args(["--json", "-o", "NAME,FSTYPE,TYPE", "-p", device])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let parsed: LsblkOutput = serde_json::from_slice(&output.stdout).ok()?;

    for dev in parsed.blockdevices {
        if dev.r#type == "disk" {
            for child in dev.children {
                if let Some(fstype) = child.fstype.clone() {
                    if is_supported_fstype(&fstype) {
                        let (label, tool, args) = label_command(&child.name, &fstype, &label_base);
                        return Some((label, tool.to_string(), args));
                    }
                }
            }
        }
    }

    None
}

/// Information about existing partitions on a device.
///
/// Used to warn the user before wiping a device that already has partitions,
/// filesystems, or a bootable operating system.
#[derive(Debug, Clone)]
pub struct DevicePartitionInfo {
    /// Whether the device has any partitions
    pub has_partitions: bool,
    /// Human-readable list of partition details (e.g., "/dev/sdb1 ext4 50G")
    pub partition_details: Vec<String>,
    /// Whether any partitions are currently mounted
    pub has_mounted: bool,
    /// Mountpoints that are currently in use
    pub mounted_paths: Vec<String>,
}

/// Check whether a device has existing partitions, filesystems, or mounted volumes.
///
/// This is used before flashing to warn the user that the target device already
/// contains data (e.g., a bootable OS) and give them a chance to confirm or cancel.
///
/// # Arguments
///
/// * `device` - Device path (e.g., "/dev/sdb")
///
/// # Returns
///
/// `Ok(DevicePartitionInfo)` with details about what was found, or an error if
/// `lsblk` cannot be run.
pub fn check_device_partitions(device: &str) -> Result<DevicePartitionInfo> {
    let output = Command::new("lsblk")
        .args([
            "--json",
            "-o",
            "NAME,TYPE,FSTYPE,SIZE,MOUNTPOINT,MOUNTPOINTS",
            "-p",
            device,
        ])
        .output()
        .context("run lsblk to check partitions")?;

    if !output.status.success() {
        return Ok(DevicePartitionInfo {
            has_partitions: false,
            partition_details: Vec::new(),
            has_mounted: false,
            mounted_paths: Vec::new(),
        });
    }

    let parsed: LsblkOutput =
        serde_json::from_slice(&output.stdout).context("parse lsblk partition info")?;

    let mut partition_details = Vec::new();
    let mut mounted_paths = Vec::new();

    for dev in &parsed.blockdevices {
        for child in &dev.children {
            if child.r#type == "part" {
                let fstype = child.fstype.as_deref().unwrap_or("unknown");
                let size = child.size.as_deref().unwrap_or("?");
                partition_details.push(format!("{} ({}, {})", child.name, fstype, size));

                // Collect mountpoints
                if let Some(mp) = &child.mountpoint {
                    if !mp.is_empty() && mp != "[SWAP]" {
                        mounted_paths.push(mp.clone());
                    }
                }
                if let Some(list) = &child.mountpoints {
                    for item in list.iter().flatten() {
                        if !item.is_empty() && item != "[SWAP]" && !mounted_paths.contains(item) {
                            mounted_paths.push(item.clone());
                        }
                    }
                }
            }
        }
    }

    let has_partitions = !partition_details.is_empty();
    let has_mounted = !mounted_paths.is_empty();

    Ok(DevicePartitionInfo {
        has_partitions,
        partition_details,
        has_mounted,
        mounted_paths,
    })
}

/// Check mount safety for a device whose path has already been validated
/// by `DevicePath::validate()` (symlink + block device checks).
fn ensure_device_safe(device: &str, user_confirmed_wipe: bool) -> Result<()> {
    let output = Command::new("lsblk")
        .args([
            "--json",
            "-o",
            "NAME,TYPE,MOUNTPOINT,MOUNTPOINTS,FSTYPE",
            "-p",
            device,
        ])
        .output()
        .context("run lsblk for mount safety checks")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if stderr.is_empty() {
            "lsblk failed for mount safety checks".to_string()
        } else {
            format!("lsblk failed for mount safety checks: {stderr}")
        };
        return Err(anyhow::anyhow!(message));
    }

    let parsed: LsblkOutput =
        serde_json::from_slice(&output.stdout).context("parse lsblk mount safety output")?;

    let mut mounts = Vec::new();
    for dev in &parsed.blockdevices {
        collect_mountpoints(dev, &mut mounts);
    }

    if mounts.iter().any(|m| m == "/") {
        return Err(anyhow::anyhow!(
            "Refusing to flash device containing the root filesystem (/)."
        ));
    }

    // If the user confirmed the wipe, mounted partitions are OK -- they will
    // be unmounted by wipe_device_if_needed(). Otherwise, block and ask the
    // user to unmount manually.
    if !user_confirmed_wipe && !mounts.is_empty() {
        let preview = mounts.into_iter().take(4).collect::<Vec<_>>().join(", ");
        return Err(anyhow::anyhow!(
            "Target device has mounted filesystems ({preview}). Unmount all partitions before flashing."
        ));
    }

    Ok(())
}

fn wipe_device_if_needed(
    device: &str,
    elevator: Option<&str>,
    progress: &mpsc::Sender<String>,
) -> Result<()> {
    let output = Command::new("lsblk")
        .args(["--json", "-o", "NAME,TYPE", "-p", device])
        .output()
        .context("run lsblk to check partitions")?;

    if !output.status.success() {
        return Ok(());
    }

    let parsed: LsblkOutput =
        serde_json::from_slice(&output.stdout).context("parse lsblk partition output")?;

    let has_partitions = parsed
        .blockdevices
        .iter()
        .any(|dev| dev.children.iter().any(|c| c.r#type == "part"));

    if !has_partitions {
        return Ok(());
    }

    let _ = progress.send("Device has existing partitions, wiping...".to_string());

    let partitions: Vec<String> = parsed
        .blockdevices
        .iter()
        .flat_map(|dev| {
            dev.children
                .iter()
                .filter(|c| c.r#type == "part")
                .map(|c| format!("/dev/{}", c.name))
                .collect::<Vec<_>>()
        })
        .collect();

    // Unmount each partition directly (failures are OK — partition may not be
    // mounted). Then wipefs and partprobe, each as a direct command invocation
    // with no shell interpretation.
    for partition in &partitions {
        let _ = elevated_command("umount", elevator)
            .arg(partition)
            .stderr(std::process::Stdio::null())
            .status();
    }

    elevated_command("wipefs", elevator)
        .args(["-a", device])
        .status()
        .context("wipefs failed")?;

    elevated_command("partprobe", elevator)
        .arg(device)
        .status()
        .context("partprobe failed after wipe")?;

    let _ = progress.send("Device wiped successfully.".to_string());

    Ok(())
}

/// Verify flash integrity by comparing SHA-256 hashes of the source image
/// and the bytes written to the device.
fn verify_flash(
    image: &Path,
    device: &str,
    elevator: Option<&str>,
    progress: &mpsc::Sender<String>,
) -> Result<bool> {
    let iso_size = std::fs::metadata(image)
        .with_context(|| format!("read image size: {}", image.display()))?
        .len();

    let _ = progress.send("Verifying: hashing source image...".to_string());

    // Hash the source ISO file.
    let mut hasher = Sha256::new();
    let mut file =
        std::fs::File::open(image).with_context(|| format!("open ISO: {}", image.display()))?;
    let mut buf = [0u8; 1024 * 1024]; // 1 MB chunks
    let mut remaining = iso_size;
    while remaining > 0 {
        let to_read = std::cmp::min(remaining as usize, buf.len());
        let n = file.read(&mut buf[..to_read]).context("read ISO for hash")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    let source_hash = hasher.finalize();

    let _ = progress.send("Verifying: reading back from device...".to_string());

    // Read the same number of bytes back from the device and hash them.
    let blocks = iso_size.div_ceil(1024 * 1024);
    let mut child = elevated_command("dd", elevator)
        .arg(format!("if={}", device))
        .arg("bs=1M")
        .arg(format!("count={}", blocks))
        .arg("status=none")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawn dd for verification read")?;

    let mut hasher = Sha256::new();
    if let Some(mut stdout) = child.stdout.take() {
        let mut remaining = iso_size;
        while remaining > 0 {
            let to_read = std::cmp::min(remaining as usize, buf.len());
            let n = stdout
                .read(&mut buf[..to_read])
                .context("read device for hash")?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
    }

    let _ = child.wait();
    let device_hash = hasher.finalize();

    Ok(source_hash == device_hash)
}

fn collect_mountpoints(dev: &crate::device::LsblkDevice, mounts: &mut Vec<String>) {
    if let Some(mp) = &dev.mountpoint {
        if !mp.is_empty() && mp != "[SWAP]" {
            mounts.push(mp.clone());
        }
    }

    if let Some(list) = &dev.mountpoints {
        for item in list.iter().flatten() {
            if !item.is_empty() && item != "[SWAP]" {
                mounts.push(item.clone());
            }
        }
    }

    for child in &dev.children {
        collect_mountpoints(child, mounts);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dd_bytes_reads_leading_number() {
        let line = "123456789 bytes (123 MB) copied, 1 s, 123 MB/s";
        assert_eq!(parse_dd_bytes(line), Some(123_456_789));
    }

    #[test]
    fn parse_dd_bytes_returns_none_without_leading_digits() {
        assert_eq!(parse_dd_bytes("dd: failed to open"), None);
    }

    #[test]
    fn sanitize_label_keeps_supported_chars() {
        let input = "Fedora Linux 40 (Beta)!";
        assert_eq!(sanitize_label(input), "Fedora_Linux_40_Beta");
    }

    #[test]
    fn truncate_label_uses_char_boundaries() {
        assert_eq!(truncate_label("abcdef", 3), "abc");
        assert_eq!(truncate_label("åäö", 2), "åä");
    }

    #[test]
    fn label_command_uses_partition_path_as_is() {
        let (label, tool, args) = label_command("/dev/sdb1", "ext4", "ubuntu_live");
        assert_eq!(label, "ubuntu_live");
        assert_eq!(tool, "e2label");
        assert_eq!(args, vec!["/dev/sdb1", "ubuntu_live"]);
    }

    #[test]
    fn collect_mountpoints_reads_nested_entries() {
        let tree = crate::device::LsblkDevice {
            name: "/dev/sdb".to_string(),
            model: None,
            size: None,
            rm: None,
            r#type: "disk".to_string(),
            fstype: None,
            mountpoint: None,
            mountpoints: None,
            children: vec![crate::device::LsblkDevice {
                name: "/dev/sdb1".to_string(),
                model: None,
                size: None,
                rm: None,
                r#type: "part".to_string(),
                fstype: Some("ext4".to_string()),
                mountpoint: Some("/media/usb".to_string()),
                mountpoints: Some(vec![Some("/media/usb".to_string()), Some("".to_string())]),
                children: Vec::new(),
            }],
        };

        let mut mounts = Vec::new();
        collect_mountpoints(&tree, &mut mounts);
        assert!(mounts.iter().any(|m| m == "/media/usb"));
    }
}
