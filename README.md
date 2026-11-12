# OAK-D Lite Studio — Cross-Platform Rust RGB & Stereo Depth Viewer

A native, portable Rust GUI application (`eframe` / `egui` + `depthai`) that streams **RGB (`CamA`)**, **Stereo Depth (`CamB` + `CamC`)**, **Aligned RGB + Depth Overlay**, and **Raw Mono Pairs** from a Luxonis **OAK-D Lite** camera on **Windows (`x86_64`)**, **Linux (`x86_64`, `aarch64` / `arm64`)**, and **macOS (`x86_64`, `arm64` Apple Silicon)**.

## Features
- **Live Multi-View Dashboard**: Toggleable **RGB (`CamA`)**, **Stereo Depth (`CamB` + `CamC`)**, **RGB + Depth Overlay** (with FOV & 37.5 mm baseline parallax alignment), and **Raw Mono L+R** views in a responsive 1×1, 1×2, or 2×2 card grid.
- **Full-Resolution Stereo Depth (`640×400`)**: Uses `FastDensity` + `Extended Disparity` (measuring down to ~18 cm) without 2× decimation or dark-pixel cutoffs.
- **Host-Side Depth Enhancement (Instant Toggle)**: 3-stage post-processing filter combining Temporal EMA + 4-frame persistence, horizontal scanline gap interpolation, and 5×5 edge-preserving median smoothing.
- **Live VPU Temperature Monitor**: Displays real-time OAK-D Lite Myriad X (RVC2) average chip temperature in the top bar with a hover breakdown of `CSS`, `MSS`, `UPA`, and `DSS` subsystems.
- **Instant Live Controls**: Adjust image orientation (`Normal`, `Rotate 180°`, `Horizontal Mirror`, `Vertical Flip`), depth range (`Min` / `Max` mm), overlay opacity, and alignment without restarting the camera pipeline.
- **Interactive Depth Inspection**: Live **Center Distance** and **Cursor Hover Distance** (`mm` and `m`).
- **Auto-Reconnect & Watchdog**: Automatically detects USB disconnects or stalled streams and retries in the background.

## Supported Platforms
| OS | Architectures |
|---|---|
| **Windows** | `x86_64` |
| **Linux** | `x86_64`, `aarch64` (`arm64`) |
| **macOS** | `x86_64` (Intel), `aarch64` (`arm64` Apple Silicon) |

---

## Portable Software (Zero Install)
Pre-packaged portable bundles include the compiled binary and all required runtime libraries (`depthai-core`, `opencv_world`, `dynamic_calibration`, `libusb-1.0`, and app-local VC++ runtime DLLs on Windows).

1. Extract the portable archive (`oakd-lite-studio-<os>-<arch>-portable.zip`) into any folder.
2. Plug in your **OAK-D Lite** via USB 3.0.
3. Run **`oakd-lite-gui.exe`** (Windows) or **`./oakd-lite-gui`** (Linux/macOS) — no installation required.

### Creating a Portable Bundle Yourself
- **Windows (`x86_64`)**:
  ```powershell
  powershell -ExecutionPolicy Bypass -File .\scripts\package_portable.ps1
  ```
  Outputs `dist/oakd-lite-studio-win64-portable/` and `dist/oakd-lite-studio-win64-portable.zip`.
- **Linux & macOS (`x86_64` / `arm64`)**:
  ```bash
  ./scripts/package_portable.sh
  ```
  Outputs `dist/oakd-lite-studio-<os>-<arch>-portable.zip`.

---

## Building from Source

### 1. Windows (`x86_64`)
1. **Rust Toolchain**: Install via [rustup.rs](https://rustup.rs/)
2. **Visual Studio C++ Build Tools & CMake**
3. **64-bit `libclang.dll`**:
   - Install LLVM (`winget install LLVM.LLVM`) **or** via Python (`pip install libclang`).
   - Copy `.cargo/config.toml.example` to `.cargo/config.toml` and set `LIBCLANG_PATH` (and `BINDGEN_EXTRA_CLANG_ARGS = "-D_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH"` if needed).
4. **USB Driver**:
   - Ensure the **WinUSB** driver is installed for the OAK-D Lite (`Movidius MyriadX` `03E7:2485`, via [Zadig](https://zadig.akeo.ie/)).
   - Ensure an official 64-bit `libusb-1.0.dll` is in `PATH` or set `LIBUSB_DLL_PATH` so `build.rs` stages it automatically.

### 2. Linux (`x86_64` & `aarch64` / `arm64`)
1. **System Dependencies** (Debian/Ubuntu):
   ```bash
   sudo apt update
   sudo apt install -y build-essential cmake pkg-config git clang libclang-dev \
     libusb-1.0-0-dev libudev-dev libdw-dev libelf-dev \
     libgl1-mesa-dev libxkbcommon-dev libwayland-dev libxrandr-dev libxi-dev libxcursor-dev
   ```
2. **USB udev Rules** (required for non-root access to OAK-D Lite):
   ```bash
   echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee /etc/udev/rules.d/80-movidius.rules
   sudo udevadm control --reload-rules && sudo udevadm trigger
   ```

### 3. macOS (`x86_64` Intel & `arm64` Apple Silicon)
1. **Xcode Command Line Tools & Homebrew Dependencies**:
   ```bash
   xcode-select --install
   brew install cmake pkg-config libusb llvm
   ```
2. **Configure `LIBCLANG_PATH`** (or copy `.cargo/config.toml.example` to `.cargo/config.toml`):
   - **Apple Silicon (`arm64`)**: `export LIBCLANG_PATH="/opt/homebrew/opt/llvm/lib"`
   - **Intel (`x86_64`)**: `export LIBCLANG_PATH="/usr/local/opt/llvm/lib"`

### Build & Run
```bash
cargo run --release
```

---

## Repository Structure
- `Cargo.toml` — Rust package dependencies (`depthai`, `eframe`, `anyhow`) and LTO release profile
- `src/main.rs` — Main GUI application, stereo depth post-processor, overlay aligner, and temperature monitor
- `build.rs` — Windows build helper that stages runtime DLLs (`opencv_world4130.dll`, `libusb-1.0.dll`)
- `scripts/package_portable.ps1` — Windows portable folder & `.zip` packager
- `scripts/package_portable.sh` — Linux/macOS portable `.tar.gz` packager
- `examples/test_camera.rs` — Hardware diagnostic & temperature test example
- `.cargo/config.toml.example` — Template for `LIBCLANG_PATH` configuration
- `oakd_reference.py` — Reference Python implementation
