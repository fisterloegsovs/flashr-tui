# Architecture & Implementation Details

This document provides an in-depth look at flashr-tui's design, data flow, and implementation patterns.

## Design Philosophy

**flashr-tui** follows these core principles:

1. **Modularity**: Each concern (device detection, ISO validation, flashing, UI) is in its own module
2. **State Machine**: Application progresses through discrete, well-defined steps
3. **Non-Blocking UI**: Long operations (flashing) happen in background threads; UI remains responsive
4. **Error Transparency**: All errors are surfaced to the user in a non-fatal way when possible
5. **Safety**: Hybrid ISO detection prevents accidental flashing of non-flashable images

## Application Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                      main.rs                                 │
│  • CLI parsing (clap)                                        │
│  • Terminal setup (crossterm)                                │
│  • Event loop (250ms tick rate)                              │
└──────────────────────┬──────────────────────────────────────┘
                       │
        ┌──────────────┴──────────────┐
        │                             │
        ▼                             ▼
┌───────────────────┐        ┌──────────────────┐
│    lib.rs         │        │   ui.rs          │
│  • App struct     │        │ • Event handler  │
│  • State machine  │        │ • Rendering      │
│  • Orchestration  │        │ • Widgets        │
│  • File I/O       │        │ • Styling        │
└────────┬──────────┘        └──────────────────┘
         │
    ┌────┴────────────────────────────┐
    │                                 │
    ▼                                 ▼
┌──────────────────┐          ┌─────────────────┐
│  device.rs       │          │  flash.rs       │
│ • lsblk parsing  │          │ • dd streaming  │
│ • Device listing │◄─────────┤ • Progress read │
│ • Filtering      │ validates │ • Labeling      │
└──────────────────┘          └────────┬────────┘
                                       │
                                       ▼
                              ┌──────────────────┐
                              │  iso.rs          │
                              │ • losetup mount  │
                              │ • Type detection │
                              │ • Partition check│
                              └──────────────────┘
```

## State Machine

The application progresses through a linear state machine:

```
Image Selection
       ↓
Device Selection ◄───┐
       ↓             │
Confirmation    ─────┘ (back button)
       ↓
Flashing (background thread)
       ↓
Result / Error (terminal state)
```

### Step Transitions

| From | To | Trigger | Condition |
|------|----|---------| --------- |
| Image | Device | Enter + valid path | Image file exists |
| Device | Confirm | Enter | Device selected |
| Confirm | Flashing | 'f' key | — |
| Confirm | Device | 'b' key | — |
| Flashing | Result | Flash completes | (automatic) |
| Result | — | 'q' key | Exit |
| Error | — | 'q' key | Exit |

## Core Data Structures

### App State

```rust
pub struct App {
    pub step: Step,
    
    // Image selection state
    pub image_input: String,           // User-entered path or filename search
    pub cwd: PathBuf,                  // Current working directory for file picker
    pub entries: Vec<FileEntry>,       // Files/dirs in cwd
    pub validated_image: Option<PathBuf>, // Validated image path
    pub iso_kind: Option<IsoKind>,     // Detected ISO type (if image selected)
    
    // Device selection state
    pub devices: Vec<Disk>,            // List of available devices
    pub selected_device: Option<usize>, // Index into devices list
    pub show_all_disks: bool,          // Toggle between removable-only vs all disks
    
    // Flashing state
    pub execute: bool,                 // Dry-run vs actual flash mode
    pub flash_progress: String,        // Current progress message
    pub progress_rx: Option<Receiver<String>>, // Channel from flash thread
    pub result_rx: Option<Receiver<FlashResult>>, // Result from flash thread
    
    // Error handling
    pub error_message: Option<String>, // Non-terminal errors
}
```

### Step Enum

```rust
pub enum Step {
    Image,      // User selects ISO file from filesystem
    Device,     // User selects target USB device
    Confirm,    // User reviews selection and confirms
    Flashing,   // Flash is in progress (non-interactive)
    Result,     // Flash completed; show result
    Error,      // Error state; show error message
}
```

### Disk Struct

```rust
pub struct Disk {
    pub name: String,    // Device name: "sda", "sdb", etc.
    pub model: String,   // Model/description: "SanDisk 3.2Gen1"
    pub size: String,    // Case sensitive; human readable: "57.3G", "1.8M"
}
```

### FileEntry Enum

```rust
pub enum FileEntry {
    Dir { name: String },
    File { name: String },
}
```

## Data Flow Diagrams

### 1. Device Discovery Flow

```
User starts app
       │
       ▼
main::main()
       │
       ├─→ clap: Parse --image, --device, --execute args
       │
       ├─→ device::list(show_all=false)
       │   │
       │   ├─→ Execute: lsblk --json
       │   │
       │   ├─→ Deserialize JSON via serde
       │   │
       │   └─→ Filter:
       │       • "type" == "disk"
       │       • "rm" == 1 (removable) if !show_all
       │
       └─→ App::new() with devices
```

**Example `lsblk` JSON:**
```json
{
  "blockdevices": [
    {
      "name": "sdb",
      "type": "disk",
      "model": "SanDisk",
      "size": "57.3G",
      "rm": 1,
      "children": [...]
    }
  ]
}
```

### 2. ISO Detection Flow

```
User selects image file (hits Enter)
       │
       ▼
ui::handle_key() → Image step
       │
       ├─→ Validate file exists
       │
       ├─→ lib::App::refresh_iso_kind()
       │   │
       │   └─→ iso::detect(image_path)
       │       │
       │       ├─→ Execute: losetup --find --show --partscan <image>
       │       │   Returns: "/dev/loop0"
       │       │
       │       ├─→ Execute: lsblk --json /dev/loop0
       │       │   Checks if loop device has "children" (partitions)
       │       │
       │       ├─→ Execute: losetup -d /dev/loop0
       │       │   Unmount loop device
       │       │
       │       └─→ Return:
       │           • Hybrid if children exist
       │           • NonHybrid if no children
       │           • Unknown if losetup fails (no root)
       │
       └─→ Store Result in App::iso_kind
```

**Partition Detection Logic:**
```rust
fn detect(image: &Path) -> Result<IsoKind> {
    // losetup mounts ISO as loop device
    let loop_dev = losetup mount image?;
    
    // Check if loop device has partitions (children)
    let output = lsblk --json <loop_dev>?;
    let has_children = output.blockdevices[0].children.is_some();
    
    losetup unmount <loop_dev>?;
    
    Ok(if has_children { Hybrid } else { NonHybrid })
}
```

### 3. Flashing Flow

```
User presses 'f' on Confirm screen
       │
       ▼
ui::handle_key() → Confirm step, 'f' pressed
       │
       ├─→ App::start_flash()
       │   │
       │   ├─→ Create mpsc channel: (progress_tx, progress_rx)
       │   │
       │   ├─→ Spawn thread:
       │   │   │
       │   │   └─→ flash::flash_image_with_progress(
       │   │       image, device, progress_tx
       │   │   )
       │   │
       │   └─→ Store progress_rx in App
       │       (Continue in main thread; pump messages later)
       │
       └─→ Step = Flashing (non-interactive)
       
       
Meanwhile in background thread:

flash_image_with_progress()
       │
       ├─→ iso::detect(image) → Validate Hybrid
       │   (If NonHybrid, error and return)
       │
       ├─→ Execute: dd if=image of=device status=progress ...
       │
       ├─→ Capture dd's stderr:
       │   "123456789 bytes (123M) copied..."
       │   Parse byte count → Send via progress_tx
       │
       ├─→ Wait for dd to complete
       │
       ├─→ Execute: partprobe device
       │   (Refresh partition table in kernel)
       │
       ├─→ flash::label_device_from_iso(image, device)
       │   │
       │   ├─→ Extract ISO filename → "nixos-24-04"
       │   │
       │   ├─→ lsblk to find first partition with fstype
       │   │
       │   └─→ Choose label tool:
       │       • FAT → fatlabel /dev/sdb1 "nixos-24-04"
       │       • NTFS → ntfslabel /dev/sdb1 "nixos-24-04"
       │       • EXT → e2label /dev/sdb1 "nixos-24-04"
       │
       └─→ Send FlashResult via result_tx
       
       
Main thread event loop (continuous):

While Flashing:
    ├─→ App::poll_flash()
    │   │
    │   ├─→ Try to receive from progress_rx (non-blocking)
    │   │   Update App::flash_progress with latest message
    │   │
    │   └─→ Check if thread finished:
    │       If yes, extract FlashResult, move to Result step
    │
    └─→ ui::draw() with latest progress_percentage
```

### 4. Command Execution Pattern

Flashr-tui spawns external processes for system interaction:

```rust
use std::process::Command;

fn run_command(cmd: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(cmd)
        .args(args)
        .output()?;
    
    if !output.status.success() {
        return Err(anyhow!("Command failed: {} {:?}", cmd, args));
    }
    
    Ok(String::from_utf8(output.stdout)?)
}
```

**Example calls:**
- `lsblk --json` – Device listing
- `losetup --find --show --partscan image.iso` – Mount ISO
- `lsblk --json /dev/loop0` – Check loop device partitions
- `losetup -d /dev/loop0` – Unmount ISO
- `dd if=image of=/dev/sdb status=progress` – Flash to device
- `partprobe /dev/sdb` – Refresh partition table
- `fatlabel /dev/sdb1 label` – Set label

## Module Dependencies

```
main.rs
  │
  ├─→ lib.rs (App, Step, FlashResult, load_entries)
  │   │
  │   ├─→ device.rs (Device::list, Disk)
  │   │
  │   ├─→ iso.rs (IsoKind, detect)
  │   │
  │   └─→ flash.rs (flash_image_with_progress)
  │       │
  │       └─→ iso.rs (iso::detect validation)
  │
  └─→ ui.rs (handle_key, draw)
      │
      └─→ lib.rs (App, Step, etc.)
```

**Dependency Rules:**
- `main.rs` knows about all modules
- `ui.rs` only touches `App` and `Step` (no device/iso/flash direct)
- `lib.rs` orchestrates; it calls device/iso/flash as needed
- `flash.rs` depends on `iso.rs` for validation
- No circular dependencies

## Channel Communication

For non-blocking UI during flashing:

```rust
// In background thread:
let output = Command::new("dd")
    .stderr(Stdio::piped())
    .spawn()?;

for line in BufReader::new(stderr).lines() {
    if let Ok(bytes) = parse_dd_bytes(&line) {
        progress_tx.send(format!("{}%", calculate_percentage(bytes)))?;
    }
}

// In main thread (event loop):
if let Ok(msg) = progress_rx.try_recv() {
    app.flash_progress = msg;  // Non-blocking update
}
```

## Error Handling Strategy

**Layered error approach:**

1. **Domain errors** (unexpected but recoverable):
   - Image file not found → show in Image step, auto-clear on retry
   - Device disconnected → show in Device list, allow rescan
   - ISO detection fails (no root) → show in Confirm step, flash blocked

2. **Flashing errors** (show in Result screen):
   - dd failure → non-fatal; show error message
   - partprobe failure → non-fatal; show message
   - Label command missing → non-fatal; show and skip

3. **Fatal errors** (exit):
   - Invalid CLI arguments → exit with error message
   - Terminal initialization failure → exit

**All errors are `anyhow::Result<T>`:**

```rust
use anyhow::{anyhow, Result, Context};

fn some_operation() -> Result<()> {
    let file = std::fs::read_to_string(path)
        .context("Failed to read image file")?;
    
    // ... processing ...
    
    Ok(())
}
```

## Performance Considerations

### dd Speed Optimization

```bash
# Current settings in flash_image_with_progress:
dd if=<image> of=<device> bs=4M status=progress oflag=sync

# Why:
# • bs=4M: 4MB block size for good throughput
# • status=progress: Output per-second progress
# • oflag=sync: Ensure every write is durable
# • (No conv=fsync to avoid flushing every block)
```

### Progress Update Rate

- **dd output rate**: ~1 line/second
- **UI tick rate**: 250ms (4 updates/sec)
- **Result**: Smooth progress bar without flickering

## Testing & Validation

### Manual Testing

1. **Device discovery**:
   ```bash
   cargo run -- --help
   # Check device list appears
   ```

2. **ISO detection** (requires root):
   ```bash
   sudo cargo run
   # Select Linux ISO, confirm ISO type displays
   ```

3. **Dry-run flash**:
   ```bash
   sudo cargo run -- --image ~/linux.iso --device /dev/sdb
   # Don't pass --execute; verify preview in Confirm step
   ```

4. **Real flash**:
   ```bash
   sudo cargo run -- --image ~/linux.iso --device /dev/sdb --execute
   # Watch progress bar; verify final label
   ```

### Edge Cases

| Scenario | Handling |
|----------|----------|
| ISO file is very small | Still flashes (no size validation) |
| Device is root FS | Allowed (no safety check); user responsibility |
| Device disconnected mid-flash | dd fails; error shown in Result |
| losetup unavailable | IsoKind = Unknown; flash allowed with warning |
| No label tools installed | Flash succeeds; label skipped silently |
| User cancels (Ctrl+C) | Terminal cleanup & restore (crossterm leaves) |

## Future Architecture Improvements

1. **Async I/O**: Replace Command spawning with tokio async runtime
   - Pro: Better responsiveness, multiple processes in parallel
   - Con: More complex state management

2. **Module config**: Load-time settings (default paths, block size, etc.)
   - Pro: Customizable behavior
   - Con: More config complexity

3. **Logging**: Optional structured logging for debugging
   - Pro: Better troubleshooting
   - Con: Output clutter in TUI

4. **Retry logic**: Auto-retry failed operations
   - Pro: Better resilience
   - Con: May mask underlying issues

5. **Multi-device flashing**: Flash same ISO to multiple USB drives simultaneously
   - Pro: Batch operations
   - Con: Much higher complexity

## Code Style & Conventions

- **Naming**: snake_case for functions, UPPER_CASE for constants
- **Errors**: Use `anyhow` for context; always add `.context()` or `.map_err()`
- **Imports**: Organize by std, external crates, internal modules
- **Comments**: Doc comments on public items; inline comments only for non-obvious logic
- **Testing**: Integration tests in `tests/` (currently: manual testing only)

## Security Considerations

1. **Root privilege**: losetup and dd require root; user must run with sudo
2. **Device access**: Direct access to `/dev/*`; no permission isolation
3. **Input validation**: ISO paths validated; no injection vulnerability
4. **Command injection**: All commands use `Command::new()` with array args (safe)
5. **Data integrity**: dd with `oflag=sync` ensures writes are durable

## Deployment & Packaging

Currently, deployed as:
- **Debug**: `./target/debug/flashr-tui`
- **Release**: `cargo build --release` → `./target/release/flashr-tui`
- **System package**: `cargo install --path .` → `~/.cargo/bin/flashr-tui`

Future: Create `.deb` / `.rpm` / AUR packages.
