# Flashr TUI

A fast, safe, and interactive TUI (Terminal User Interface) for flashing Linux OS ISO images to USB drives. Built in Rust with [ratatui](https://ratatui.rs/) for the user interface.

## Features

- **Interactive TUI** – Navigate and select images and devices with keyboard controls
- **File picker** – Browse your entire filesystem to select ISO images
- **Auto-detection** – Detects ISO type (hybrid/non-hybrid) without root privileges
- **Progress tracking** – Real-time progress bar during flashing with byte count
- **Device management** – Filter removable disks or show all disks
- **Device labeling** – Auto-rename USB drive labels after flashing (FAT/NTFS/EXT)
- **Dry-run mode** – Safe preview of what would flash (default)
- **Auto-elevation** – Automatically prompts for password via `pkexec`/`sudo` when flashing
- **Linux ISOs** – Optimized for hybrid Linux ISOs (raw write with `dd`)

## Requirements

### System
- **Linux** (tested on modern distributions)
- **Rust** (1.70+) for building from source
- **lsblk** – for device listing (no root required)
- **dd** – for flashing (root required; auto-elevated via `pkexec` or `sudo`)

### Optional tools
- `pkexec` or `sudo` – for automatic privilege elevation when flashing (if not running as root)
- `fatlabel` – for FAT/VFAT labels
- `ntfslabel` – for NTFS labels
- `e2label` – for EXT2/3/4 labels

## Installation

### From Source

**1. Clone the repository:**
```bash
git clone https://github.com/fisterloegsovs/flashr-tui.git
cd flashr-tui
```

**2. Build the project:**
```bash
cargo build --release
```

The binary will be at `target/release/flashr-tui`.

**3. (Optional) Install globally:**
```bash
cargo install --path .
```

### Download Pre-built Binary

[Add future binary releases here once available]

## Usage

### Basic Usage

Run the TUI:
```bash
./target/release/flashr-tui
```

Or if installed globally:
```bash
flashr-tui
```

### Command-line Options

- `--image <PATH>` – Pre-fill the image path (skip file picker)
- `--device <DEVICE>` – Pre-select device (e.g., `/dev/sdb`)
- `--execute` – Actually flash the device (default is dry-run)

### Examples

**Dry-run (safe preview, no root needed):**
```bash
flashr-tui --image ~/Downloads/linux.iso --device /dev/sdb
```

**Flash for real (auto-elevates via pkexec/sudo):**
```bash
flashr-tui --image ~/Downloads/linux.iso --device /dev/sdb --execute
```

**Or run with sudo directly:**
```bash
sudo flashr-tui --execute
```

**Pre-fill both image and device:**
```bash
flashr-tui --image ~/Downloads/nixos.iso --device /dev/sdb --execute
```

### TUI Controls

#### Step 1: Choose Image File
- **Up/Down** – Move selection in file list
- **Enter** – Open directory or select file
- **Backspace** – Go up one directory (when input is empty)
- **Ctrl+U** – Clear typed input
- **Type** – Filter or enter custom path

#### Step 2: Select Device
- **Up/Down** – Move selection in device list
- **Enter** – Select device and move to confirmation
- **r** – Rescan devices
- **a** – Toggle between removable disks only / all disks
- **b** – Back to image selection

#### Step 3: Confirm
- **f** – Flash (or dry-run if not `--execute`)
- **b** – Back to device selection

#### Flashing
- Watch real-time progress with byte count and percentage
- Estimated time remaining shown when available

#### Result
- **q** – Exit after flashing completes

## Project Structure

```
flashr-tui/
├── Cargo.toml              # Package manifest and dependencies
├── src/
│   ├── main.rs             # Entry point, CLI parsing, event loop
│   ├── lib.rs              # Core app state and types
│   ├── device.rs           # Device detection and listing (lsblk)
│   ├── iso.rs              # ISO type detection (MBR/GPT byte reading)
│   ├── flash.rs            # Flashing logic, privilege elevation, progress streaming, labeling
│   ├── ui.rs               # All ratatui rendering and event handling
│   └── logo.txt            # ASCII art logo (embedded at compile time)
└── README.md               # This file
```

## Comprehensive Documentation

### [src/main.rs](src/main.rs) – Entry Point (80 lines)

**Purpose:** CLI parsing, TUI initialization, and main event loop.

**Key Functions:**
- `main()` – Parses arguments, lists removable disks, initializes app, and starts TUI
- `run_tui()` – Sets up terminal (raw mode, alternate screen) and runs the event loop
- `run_loop()` – Main event loop that polls for key events, renders frames, and checks flash progress

**Key Structs:**
- `Cli` – Command-line arguments using `clap`

### [src/lib.rs](src/lib.rs) – Core App State (251 lines)

**Purpose:** Application state machine, types, and initialization.

**Key Types:**

- **`Step`** – Current TUI step enum:
  - `Image` – Picking ISO file
  - `Device` – Selecting USB device
  - `Confirm` – Review before flash
  - `Flashing` – Flash in progress
  - `Result` – Flash completed (success/fail)
  - `Error` – Error state

- **`App`** – Main application state struct:
  - `step` – Current screen
  - `image_input` – User-entered ISO path
  - `cwd` – Current working directory for file picker
  - `entries` – Files/dirs in current directory
  - `iso_kind` – Detected ISO type (Hybrid/NonHybrid/Unknown)
  - `devices` – List of available USB devices
  - `selected_device` – Currently selected device
  - `execute` – Whether to actually flash or dry-run
  - `flash_progress` – Current flashing progress message
  - `progress_rx` / `result_rx` – Channels from background flash thread

**Key Methods:**
- `App::new()` – Initialize app from CLI args and device list
- `validate_image()` – Check image file exists
- `refresh_iso_kind()` – Detect ISO type (calls `iso::detect`)
- `start_flash()` – Spawn background thread to flash
- `poll_flash()` – Check progress from background thread
- `load_entries()` – Load files/dirs from filesystem for file picker

**Key Enums:**
- `AppExit` – Exit signals (currently just `Quit`)
- `FlashResult` – Result of flash operation (ok: bool, message: String)
- `FileEntry` – Represents a file or directory in picker

### [src/device.rs](src/device.rs) – Device Detection (70 lines)

**Purpose:** Query system for connected USB/removable devices.

**Key Structures:**
- **`Disk`** – Represents a block device:
  - `name` – Device name (e.g., "sdb")
  - `model` – Model string (e.g., "SanDisk 3.2Gen1")
  - `size` – Size string (e.g., "57.3G")
  - Method: `device_path()` – Returns full path "/dev/sdb"

- **`LsblkOutput` / `LsblkDevice`** – Deserialization structures for `lsblk --json` output

**Key Functions:**
- `list(show_all: bool) -> Result<Vec<Disk>>` – Main function:
  - Runs `lsblk --json` to query block devices
  - Filters to disk-type devices
  - If `show_all` is false, filters to removable devices only (`rm==1`)
  - Returns list of `Disk` structs or error

**How it Works:**
```
lsblk --json ─→ Parse JSON ─→ Filter disk type ─→ Filter by removability ─→ Return Disk list
```

### [src/iso.rs](src/iso.rs) – ISO Type Detection (57 lines)

**Purpose:** Determine if an ISO file has a partition table (hybrid) for safe flashing.

**Key Types:**
- **`IsoKind`** – Enum representing ISO type:
  - `Unknown` – Unable to detect (e.g., file too small)
  - `Hybrid` – Has MBR/GPT partition table; safe to raw write with `dd`
  - `NonHybrid` – No partition table; cannot be flashed with raw write

**Key Functions:**
- `detect(image: &Path) -> Result<IsoKind>` – Main function:
  - Reads the first 520 bytes of the ISO file
  - Checks for MBR boot signature (`0x55 0xAA`) at bytes 510-511
  - Checks for non-zero MBR partition entries at bytes 446-509
  - Checks for GPT header (`EFI PART`) at bytes 512-519
  - Returns `Hybrid` if partition table found, else `NonHybrid`
  - **No root privileges required** — only needs read access to the file

**How it Works:**
```
Read first 520 bytes of ISO file
  │
  ├─→ Check bytes 510-511 for MBR signature (0x55 0xAA)
  ├─→ Check bytes 446-509 for non-zero partition entries
  ├─→ Check bytes 512-519 for GPT header ("EFI PART")
  │
  └─→ Return Hybrid (if MBR+partitions or GPT) / NonHybrid
```

### [src/flash.rs](src/flash.rs) – Flashing Logic (150+ lines)

**Purpose:** Actually flash ISO to device, stream progress, and label the drive. Automatically elevates privileges via `pkexec` or `sudo` when not running as root.

**Key Functions:**

- **`is_root() -> bool`**
  - Checks if the current process is running as root (euid == 0)

- **`find_elevator() -> Option<&'static str>`**
  - Finds an available privilege elevation tool (`pkexec` first, then `sudo`)

- **`flash_image_with_progress(image: &Path, device: &str, progress: Sender<String>) -> Result<()>`**
  - Main flashing function (called in background thread)
  - Validates ISO type
  - If not root, finds an elevator and wraps privileged commands with it
  - Spawns `dd` process with pipes for streaming progress
  - Reads `stderr` line-by-line (dd outputs progress to stderr)
  - Sends progress updates through channel
  - Calls `partprobe` to refresh partition table
  - Attempts to label device via `label_device_from_iso()`

- **`parse_dd_bytes(line: &str) -> Option<u64>`**
  - Extracts byte count from dd output
  - Parses lines like "1234567 bytes" to get progress number

- **`label_device_from_iso(image: &Path, device: &str, elevator: Option<&str>) -> Result<Option<String>>`**
  - Extracts ISO filename (e.g., "nixos-24.04.iso" → "nixos-24-04")
  - Queries `lsblk` to find first writable filesystem on device
  - Calls appropriate label tool (elevated if needed) based on filesystem type:
    - FAT → `fatlabel` (11 char max)
    - NTFS → `ntfslabel` (32 char max)
    - EXT → `e2label` (16 char max)

- **`sanitize_label(input: &str) -> String`**
  - Converts ISO name to valid filesystem label
  - Keeps alphanumerics, `-`, `_`
  - Replaces spaces with `_`

- **`is_supported_fstype(fstype: &str) -> bool`**
  - Checks if filesystem type is writable (FAT, NTFS, EXT)

- **`label_command()` →  `truncate_label()`**
  - Maps filesystem type to label tool and arguments
  - Truncates label to filesystem-specific max length

**How it Works:**
```
[pkexec/sudo] dd if=image.iso of=/dev/sdb bs=4M status=progress oflag=sync
  │
  ├─→ Capture stderr ─→ Parse progress bytes ─→ Send via channel
  │
  └─→ Wait for completion ─→ [pkexec/sudo] partprobe ─→ [pkexec/sudo] Label device
```

### [src/ui.rs](src/ui.rs) – User Interface (15K+ lines)

**Purpose:** Render TUI screens and handle keyboard input.

**Key Functions:**

- **Event Handlers:**
  - `handle_key(app: &mut App, key: KeyEvent) -> Option<AppExit>` – Main key dispatcher
  - `handle_image_step()` – Navigate file picker, type paths
  - `handle_device_step()` – Select device, rescan, toggle all disks
  - `handle_confirm_step()` – Confirm flash or go back
  - `handle_flashing_step()` – No input during flash (blocking on progress)
  - `handle_result_step()` – Show result, wait for quit
  - `handle_done_step()` / error handler

- **Rendering:**
  - `draw(frame: &mut ratatui::Frame, app: &App)` – Main render dispatcher
  - `draw_image_step()` – File picker screen with directory browser
  - `draw_device_step()` – Device list (or empty state with hints)
  - `draw_confirm_step()` – Confirmation screen with ISO type info
  - `draw_flashing_step()` – Progress bar + current status message
  - `draw_result_step()` – Success/fail message
  - `draw_error_step()` – Error state display

- **Utilities:**
  - `status_line()` – Bottom footer with key bindings for current step
  - `iso_info_line()` – Formats ISO detection result for display

**Key Rendering Details:**
- Uses `ratatui::widgets` for Layout, List, Paragraph, Gauge, Block, Borders
- 3-panel layout: title (top), content (middle), status/footer (bottom)
- Color-coded text: Cyan for titles, Yellow for input, Green for progress, Red for errors

## Configuration & Customization

Currently, all features are built-in with no config file. Future enhancements could include:
- Config file for default ISO search paths
- Output verbosity settings
- Custom labeling strategies

## Troubleshooting

### Flash fails: "Root privileges required for flashing"
**Cause:** Neither `pkexec` nor `sudo` was found on the system.
**Solution:** Install one of them, or run directly with sudo:
```bash
sudo flashr-tui --execute
```

### No USB devices appear
**Causes:**
1. Devices not plugged in
2. Devices not removable (`rm` attribute not set)
3. **Solution:** Press `r` to rescan, or `a` to show all disks (with warning)

### Labeling fails: "Labeling tool not available"
**Solution:** Install the appropriate tool:
- FAT: `sudo pacman -S dosfstools` (includes `fatlabel`)
- NTFS: `sudo pacman -S ntfs-3g`
- EXT: Usually built-in; check `e2fsprogs` package

### Flash takes too long / seems stuck
**Normal behavior:** Flashing large ISOs can take 1-5 minutes depending on USB speed. Watch the progress bar; if it's not advancing, press `Ctrl+C` to abort and retry.

## Development

### Building
```bash
cargo build           # Debug build
cargo build --release # Optimized build
```

### Running Tests
```bash
cargo test
```

### Code Quality
```bash
cargo clippy          # Lint suggestions
cargo fmt             # Format code
```

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `ratatui` | 0.29 | TUI rendering |
| `crossterm` | 0.28 | Terminal I/O |
| `clap` | 4.5 | CLI argument parsing |
| `serde` | 1.0 | JSON deserialization |
| `serde_json` | 1.0 | JSON parsing |
| `anyhow` | 1.0 | Error handling |
| `libc` | 0.2 | Root detection (geteuid) |

## License

[Add license here - e.g., MIT, GPL, etc.]

## Contributing

Contributions are welcome! Please:
1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Run `cargo fmt` and `cargo clippy`
5. Submit a pull request

