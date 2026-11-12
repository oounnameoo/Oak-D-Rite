# OAK-D Lite — Rust RGB & Stereo Depth GUI Viewer

A native Rust GUI application (`eframe` / `egui` + `depthai`) that streams both **RGB (`CamA`)** and **Stereo Depth (`CamB` + `CamC`)** from a Luxonis **OAK-D Lite** camera.

## Features
- **Live Dual Stream**: Side-by-side **RGB (`CamA`, 640×400 @ 30 FPS)** and **Stereo Depth (`CamB` + `CamC`, 8-bit `GRAY8` input -> 16-bit millimeter depth output)**.
- **Auto-Reconnect & Watchdog**: Automatically detects USB disconnects or stalled streams, retries connection in the background, and provides a manual **Reconnect Now** button.
- **Interactive Depth Inspection**:
  - Live **Center Distance** readout (`mm` and `m`).
  - **Cursor Hover Distance**: Move your cursor over any pixel on the depth map to inspect its exact distance in millimeters and meters.
  - **Adjustable Depth Range Slider**: Dynamically scale the Jet heatmap range (`500 mm` – `8000 mm`).
- **Windows GUI Subsystem**: Launches directly as a native GUI window without an extra console window (`#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]`).
- **Python Reference Included**: Includes [`oakd_reference.py`](oakd_reference.py) showing the 1-to-1 equivalent Python (`depthai` + `opencv`) script.

## Repository Structure
- `Cargo.toml` — Rust package dependencies (`depthai`, `eframe`, `egui`, `anyhow`)
- `src/main.rs` — Main GUI application and OAK-D Lite pipeline worker thread
- `build.rs` — Windows build helper that stages runtime DLLs (`opencv_world4130.dll` and `libusb-1.0.dll`) into the target profile directory
- `.cargo/config.toml.example` — Template for configuring `LIBCLANG_PATH` and `BINDGEN_EXTRA_CLANG_ARGS` on Windows
- `oakd_reference.py` — Reference Python implementation

## Prerequisites (Windows)
1. **Rust Toolchain**: Install via [rustup.rs](https://rustup.rs/)
2. **Visual Studio C++ Build Tools & CMake**
3. **64-bit `libclang.dll`**:
   - Install LLVM (`winget install LLVM.LLVM`) **or** via Python (`pip install libclang`).
   - Copy `.cargo/config.toml.example` to `.cargo/config.toml` and set `LIBCLANG_PATH` to the folder containing `libclang.dll`.
4. **USB Driver & Runtime**:
   - Ensure the **WinUSB** driver is installed for the OAK-D Lite (`Movidius MyriadX` `03E7:2485`, via [Zadig](https://zadig.akeo.ie/)).
   - Ensure a 64-bit `libusb-1.0.dll` is available in your `PATH` (or set `LIBUSB_DLL_PATH` before building so `build.rs` stages it automatically).

## Build & Run
```powershell
cargo run --release
```
