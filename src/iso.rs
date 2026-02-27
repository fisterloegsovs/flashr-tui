//! ISO type detection.
//!
//! Detects whether an ISO image file is a hybrid ISO (with MBR partition table)
//! or non-hybrid (raw ISO 9660 data). Only hybrid ISOs can be safely flashed
//! to USB with raw block writes.
//!
//! Detection is done by reading the first 512 bytes of the file and inspecting
//! the MBR boot signature and partition table entries. This requires no special
//! privileges — only read access to the ISO file.

use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;

/// Categorizes an ISO image based on whether it has a partition table.
///
/// - `Unknown` - Could not determine type
/// - `Hybrid` - Has MBR partition table; safe to raw-write to USB
/// - `NonHybrid` - No partition table; cannot be flashed with raw write
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoKind {
    /// ISO type could not be determined
    Unknown,
    /// Hybrid ISO with MBR partition table (safe to raw write)
    Hybrid,
    /// Non-hybrid ISO without partition table (unsafe to raw write)
    NonHybrid,
}

/// MBR boot signature bytes at offset 510-511.
const MBR_SIGNATURE: [u8; 2] = [0x55, 0xAA];

/// Offset where the four MBR partition entries begin (bytes 446-509).
const PARTITION_TABLE_OFFSET: usize = 446;

/// Each MBR partition entry is 16 bytes; there are 4 entries.
const PARTITION_ENTRY_SIZE: usize = 16;
const PARTITION_ENTRY_COUNT: usize = 4;

/// Offset of the MBR boot signature (bytes 510-511).
const MBR_SIGNATURE_OFFSET: usize = 510;

/// GPT header magic string at byte offset 512.
const GPT_MAGIC: &[u8; 8] = b"EFI PART";

/// Detect the type of an ISO image file.
///
/// Works by reading the first 520 bytes and checking for:
/// 1. MBR boot signature (`0x55 0xAA`) at bytes 510-511
/// 2. At least one non-zero MBR partition entry in bytes 446-509
/// 3. Optionally, a GPT header (`EFI PART`) at bytes 512-519
///
/// If the MBR signature is present and at least one partition entry is non-zero,
/// the ISO is considered `Hybrid`. Otherwise it is `NonHybrid`.
///
/// # Arguments
///
/// * `image` - Path to the ISO file to analyze
///
/// # Returns
///
/// - `Ok(IsoKind::Hybrid)` if ISO has an MBR partition table (or GPT)
/// - `Ok(IsoKind::NonHybrid)` if ISO has no partition table
/// - `Err` if the file cannot be read
///
/// # Note
///
/// This function requires only read access to the file — no root privileges needed.
pub fn detect(image: &Path) -> Result<IsoKind> {
    let mut file = std::fs::File::open(image)
        .with_context(|| format!("open ISO image: {}", image.display()))?;

    // Read enough for MBR (512 bytes) + potential GPT header (8 more bytes)
    let mut buf = [0u8; 520];
    let bytes_read = file.read(&mut buf).context("read ISO header")?;

    // Need at least 512 bytes to inspect MBR
    if bytes_read < 512 {
        return Ok(IsoKind::Unknown);
    }

    // Check MBR boot signature at bytes 510-511
    let has_mbr_signature = buf[MBR_SIGNATURE_OFFSET] == MBR_SIGNATURE[0]
        && buf[MBR_SIGNATURE_OFFSET + 1] == MBR_SIGNATURE[1];

    // Check for at least one non-zero partition entry in the MBR partition table
    let has_partition_entry = (0..PARTITION_ENTRY_COUNT).any(|i| {
        let start = PARTITION_TABLE_OFFSET + i * PARTITION_ENTRY_SIZE;
        let end = start + PARTITION_ENTRY_SIZE;
        buf[start..end].iter().any(|&b| b != 0)
    });

    // Check for GPT header at byte 512 (present in some hybrid ISOs)
    let has_gpt = bytes_read >= 520 && buf[512..520] == *GPT_MAGIC;

    if (has_mbr_signature && has_partition_entry) || has_gpt {
        Ok(IsoKind::Hybrid)
    } else {
        Ok(IsoKind::NonHybrid)
    }
}
