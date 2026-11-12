#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::ffi::{c_void, CString};
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use depthai::camera::{CameraNode, CameraOutputConfig};
use depthai::common::{CameraBoardSocket, CameraImageOrientation, ImageFrameType, ResizeMode};
use depthai::pipeline::Pipeline;
use depthai::stereo_depth::StereoDepthNode;
use depthai::{Device, StereoPresetMode};
use eframe::egui::{
    self, Align, Color32, ColorImage, Frame, Grid, Layout, Margin, RichText, Rounding, Stroke,
    TextureHandle, TextureOptions, Vec2,
};

#[cfg(target_os = "windows")]
extern "C" {
    fn _putenv_s(varname: *const c_char, value_string: *const c_char) -> c_int;
}

#[cfg(unix)]
extern "C" {
    fn setenv(name: *const c_char, value: *const c_char, overwrite: c_int) -> c_int;
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
extern "C" {
    #[link_name = "?getChipTemperature@DeviceBase@dai@@QEAA?AUChipTemperature@2@XZ"]
    fn dai_device_base_get_chip_temperature(
        this_device_base: *mut c_void,
        out_temp: *mut ChipTemperature,
    ) -> *mut ChipTemperature;
}

#[cfg(all(unix, any(target_arch = "x86_64", target_arch = "aarch64")))]
extern "C" {
    #[link_name = "_ZN3dai10DeviceBase18getChipTemperatureEv"]
    fn dai_device_base_get_chip_temperature_unix(
        this_device_base: *mut c_void,
    ) -> ChipTemperature;
}

/// On-chip thermal sensor readings (°C) from the OAK-D Lite RVC2 (Myriad X) VPU.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct ChipTemperature {
    css: f32,     // CPU Subsystem (°C)
    mss: f32,     // Media Subsystem (°C)
    upa: f32,     // SHAVE Vector Array (°C)
    dss: f32,     // DRAM Subsystem (°C)
    average: f32, // Average Chip Temperature (°C)
}

fn query_chip_temperature(device: &Device) -> Option<ChipTemperature> {
    if !device.is_connected() {
        return None;
    }
    unsafe {
        let shared_ptr_addr = *(device as *const Device as *const *const *mut c_void);
        if shared_ptr_addr.is_null() {
            return None;
        }
        let device_base_ptr = *shared_ptr_addr;
        if device_base_ptr.is_null() {
            return None;
        }

        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        let temp = {
            let mut t = ChipTemperature::default();
            dai_device_base_get_chip_temperature(device_base_ptr, &mut t);
            t
        };

        #[cfg(all(unix, any(target_arch = "x86_64", target_arch = "aarch64")))]
        let temp = dai_device_base_get_chip_temperature_unix(device_base_ptr);

        #[cfg(not(any(
            all(target_os = "windows", target_arch = "x86_64"),
            all(unix, any(target_arch = "x86_64", target_arch = "aarch64"))
        )))]
        return None;

        if temp.average.is_finite() && temp.average > -40.0 && temp.average < 150.0 {
            Some(temp)
        } else {
            None
        }
    }
}

fn set_c_env(key: &str, value: &str) {
    std::env::set_var(key, value);
    if let (Ok(k), Ok(v)) = (CString::new(key), CString::new(value)) {
        unsafe {
            #[cfg(target_os = "windows")]
            {
                _putenv_s(k.as_ptr(), v.as_ptr());
            }
            #[cfg(unix)]
            {
                setenv(k.as_ptr(), v.as_ptr(), 1);
            }
        }
    }
}

const FRAME_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(4);
const RECONNECT_DELAY: Duration = Duration::from_millis(2500);

/// Precomputed 256-entry JET colormap lookup table to avoid per-pixel floating-point math.
const JET_LUT: [Color32; 256] = build_jet_lut();

const fn build_jet_lut() -> [Color32; 256] {
    let mut lut = [Color32::BLACK; 256];
    let mut i = 0;
    while i < 256 {
        let t4 = (i as i32) * 4; // 0..=1020 (scaled by 255)
        let r_num = 382 - (t4 - 765).abs();
        let g_num = 382 - (t4 - 510).abs();
        let b_num = 382 - (t4 - 255).abs();

        let r = if r_num <= 0 {
            0
        } else if r_num >= 255 {
            255
        } else {
            r_num as u8
        };
        let g = if g_num <= 0 {
            0
        } else if g_num >= 255 {
            255
        } else {
            g_num as u8
        };
        let b = if b_num <= 0 {
            0
        } else if b_num >= 255 {
            255
        } else {
            b_num as u8
        };

        lut[i] = Color32::from_rgb(r, g, b);
        i += 1;
    }
    lut
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolutionPreset {
    R640x400,
    R640x480,
    R1280x720,
    R1280x800,
    R1920x1080,
}

impl ResolutionPreset {
    const ALL: [Self; 5] = [
        Self::R640x400,
        Self::R640x480,
        Self::R1280x720,
        Self::R1280x800,
        Self::R1920x1080,
    ];

    fn dimensions(self) -> (u32, u32) {
        match self {
            Self::R640x400 => (640, 400),
            Self::R640x480 => (640, 480),
            Self::R1280x720 => (1280, 720),
            Self::R1280x800 => (1280, 800),
            Self::R1920x1080 => (1920, 1080),
        }
    }

    /// OAK-D Lite mono stereo sensors (CamB/CamC OV7251) natively support 640x480 and 640x400.
    fn mono_dimensions(self) -> (u32, u32) {
        match self {
            Self::R640x480 => (640, 480),
            Self::R640x400 | Self::R1280x720 | Self::R1280x800 | Self::R1920x1080 => (640, 400),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::R640x400 => "640 x 400",
            Self::R640x480 => "640 x 480",
            Self::R1280x720 => "1280 x 720",
            Self::R1280x800 => "1280 x 800",
            Self::R1920x1080 => "1920 x 1080",
        }
    }
}

/// Hardware-valid disparity modes for OAK-D Lite (RVC2 does not allow Subpixel + Extended simultaneously).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisparityMode {
    Extended,
    Subpixel,
    Standard,
}

impl DisparityMode {
    const ALL: [Self; 3] = [Self::Extended, Self::Subpixel, Self::Standard];

    fn label(self) -> &'static str {
        match self {
            Self::Extended => "Extended (18 cm - 4 m, Close Range)",
            Self::Subpixel => "Subpixel (40 cm - 10 m, High Precision)",
            Self::Standard => "Standard (40 cm - 6 m, 96 px)",
        }
    }
}

/// Configurable camera and stereo depth pipeline parameters (requires pipeline restart on change).
#[derive(Debug, Clone, PartialEq)]
struct PipelineConfig {
    resolution: ResolutionPreset,
    fps: f32,
    resize_mode: ResizeMode,
    stereo_preset: StereoPresetMode,
    disparity_mode: DisparityMode,
    left_right_check: bool,
    distortion_correction: bool,
    stream_mono_pair: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            resolution: ResolutionPreset::R640x400,
            fps: 30.0,
            resize_mode: ResizeMode::Crop,
            // FastDensity preserves full 640x400 resolution without depthai-core's 2x decimation
            // or brightness cutoff that blanks out dark monitors/surfaces.
            stereo_preset: StereoPresetMode::FastDensity,
            disparity_mode: DisparityMode::Extended,
            left_right_check: true,
            distortion_correction: true,
            stream_mono_pair: false,
        }
    }
}

/// Shared live view & rendering flags read by the worker thread without locking or restarting.
struct LiveViewFlags {
    min_depth_mm: AtomicU32,
    max_depth_mm: AtomicU32,
    overlay_alpha_u8: AtomicU32,
    orientation_code: AtomicU32,
    enhance_depth: AtomicBool,
    align_depth_to_rgb: AtomicBool,
    show_rgb: AtomicBool,
    show_depth: AtomicBool,
    show_overlay: AtomicBool,
    show_mono_pair: AtomicBool,
    force_reconnect: AtomicBool,
}

impl Default for LiveViewFlags {
    fn default() -> Self {
        Self {
            min_depth_mm: AtomicU32::new(150),
            max_depth_mm: AtomicU32::new(3000),
            overlay_alpha_u8: AtomicU32::new(140), // ~55%
            orientation_code: AtomicU32::new(0),   // Normal
            enhance_depth: AtomicBool::new(true),
            align_depth_to_rgb: AtomicBool::new(true),
            show_rgb: AtomicBool::new(true),
            show_depth: AtomicBool::new(true),
            show_overlay: AtomicBool::new(false),
            show_mono_pair: AtomicBool::new(false),
            force_reconnect: AtomicBool::new(false),
        }
    }
}

/// Packet sent from the camera worker thread to the GUI thread.
struct CameraUpdate {
    rgb_image: Option<ColorImage>,
    depth_image: Option<ColorImage>,
    overlay_image: Option<ColorImage>,
    mono_pair_image: Option<ColorImage>,
    raw_depth_mm: Option<(usize, usize, Vec<u16>)>,
    center_distance_mm: Option<u16>,
    chip_temp: Option<ChipTemperature>,
    fps: f32,
    connected: bool,
    status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewPanel {
    Rgb,
    Depth,
    Overlay,
    MonoPair,
}

struct OakdGuiApp {
    rx: Receiver<CameraUpdate>,
    rgb_texture: Option<TextureHandle>,
    depth_texture: Option<TextureHandle>,
    overlay_texture: Option<TextureHandle>,
    mono_pair_texture: Option<TextureHandle>,
    raw_depth_mm: Option<(usize, usize, Vec<u16>)>,
    center_distance_mm: u16,
    chip_temp: Option<ChipTemperature>,
    hover_distance_mm: Option<(&'static str, usize, usize, u16)>,
    fps: f32,
    connected: bool,
    status: String,
    min_depth_mm: f32,
    max_depth_mm: f32,
    overlay_alpha: f32,
    orientation: CameraImageOrientation,
    enhance_depth: bool,
    align_depth_to_rgb: bool,
    live_flags: Arc<LiveViewFlags>,
    shared_config: Arc<Mutex<PipelineConfig>>,
    pending_config: PipelineConfig,
    applied_config: PipelineConfig,
    show_rgb: bool,
    show_depth: bool,
    show_overlay: bool,
    show_mono_pair: bool,
    show_settings: bool,
}

impl OakdGuiApp {
    fn new(
        rx: Receiver<CameraUpdate>,
        live_flags: Arc<LiveViewFlags>,
        shared_config: Arc<Mutex<PipelineConfig>>,
    ) -> Self {
        let initial_config = shared_config
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default();
        Self {
            rx,
            rgb_texture: None,
            depth_texture: None,
            overlay_texture: None,
            mono_pair_texture: None,
            raw_depth_mm: None,
            center_distance_mm: 0,
            chip_temp: None,
            hover_distance_mm: None,
            fps: 0.0,
            connected: false,
            status: "Searching for OAK-D Lite...".to_string(),
            min_depth_mm: 150.0,
            max_depth_mm: 3000.0,
            overlay_alpha: 0.55,
            orientation: CameraImageOrientation::Normal,
            enhance_depth: true,
            align_depth_to_rgb: true,
            live_flags,
            shared_config,
            pending_config: initial_config.clone(),
            applied_config: initial_config,
            show_rgb: true,
            show_depth: true,
            show_overlay: false,
            show_mono_pair: false,
            show_settings: true,
        }
    }

    fn sync_live_flags(&self) {
        self.live_flags
            .min_depth_mm
            .store(self.min_depth_mm as u32, Ordering::Relaxed);
        self.live_flags
            .max_depth_mm
            .store(self.max_depth_mm as u32, Ordering::Relaxed);
        self.live_flags.overlay_alpha_u8.store(
            (self.overlay_alpha.clamp(0.0, 1.0) * 255.0) as u32,
            Ordering::Relaxed,
        );
        self.live_flags
            .orientation_code
            .store(orientation_to_code(self.orientation), Ordering::Relaxed);
        self.live_flags
            .enhance_depth
            .store(self.enhance_depth, Ordering::Relaxed);
        self.live_flags
            .align_depth_to_rgb
            .store(self.align_depth_to_rgb, Ordering::Relaxed);
        self.live_flags
            .show_rgb
            .store(self.show_rgb, Ordering::Relaxed);
        self.live_flags
            .show_depth
            .store(self.show_depth, Ordering::Relaxed);
        self.live_flags
            .show_overlay
            .store(self.show_overlay, Ordering::Relaxed);
        self.live_flags.show_mono_pair.store(
            self.applied_config.stream_mono_pair && self.show_mono_pair,
            Ordering::Relaxed,
        );
    }

    fn apply_pipeline_config(&mut self) {
        if let Ok(mut cfg) = self.shared_config.lock() {
            *cfg = self.pending_config.clone();
        }
        self.show_mono_pair = self.pending_config.stream_mono_pair;
        self.applied_config = self.pending_config.clone();
        self.sync_live_flags();
        self.live_flags
            .force_reconnect
            .store(true, Ordering::Relaxed);
        self.status = "Applying settings & restarting...".to_string();
    }

    fn drain_updates(&mut self, ctx: &egui::Context) {
        while let Ok(update) = self.rx.try_recv() {
            if let Some(rgb) = update.rgb_image {
                update_or_create_texture(
                    ctx,
                    &mut self.rgb_texture,
                    "rgb_tex",
                    rgb,
                    TextureOptions::LINEAR,
                );
            }
            if let Some(depth) = update.depth_image {
                update_or_create_texture(
                    ctx,
                    &mut self.depth_texture,
                    "depth_tex",
                    depth,
                    TextureOptions::LINEAR,
                );
            }
            if let Some(overlay) = update.overlay_image {
                update_or_create_texture(
                    ctx,
                    &mut self.overlay_texture,
                    "overlay_tex",
                    overlay,
                    TextureOptions::LINEAR,
                );
            }
            if let Some(mono) = update.mono_pair_image {
                update_or_create_texture(
                    ctx,
                    &mut self.mono_pair_texture,
                    "mono_tex",
                    mono,
                    TextureOptions::LINEAR,
                );
            }
            if let Some(raw_depth) = update.raw_depth_mm {
                self.raw_depth_mm = Some(raw_depth);
            }
            if let Some(center_mm) = update.center_distance_mm {
                self.center_distance_mm = center_mm;
            }
            if let Some(temp) = update.chip_temp {
                self.chip_temp = Some(temp);
            } else if !update.connected {
                self.chip_temp = None;
            }
            self.fps = update.fps;
            self.connected = update.connected;
            self.status = update.status;
        }
    }

    fn render_view_card(&mut self, ui: &mut egui::Ui, panel: ViewPanel, allocated_size: Vec2) {
        let card_frame = Frame::group(ui.style())
            .fill(Color32::from_rgb(22, 24, 28))
            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(55, 60, 70)))
            .rounding(Rounding::same(6.0))
            .inner_margin(Margin::same(8.0));

        ui.allocate_ui_with_layout(allocated_size, Layout::top_down(Align::Min), |ui| {
            card_frame.show(ui, |ui| {
                ui.set_min_size((allocated_size - Vec2::splat(16.0)).max(Vec2::ZERO));

                let (title, subtitle) = match panel {
                    ViewPanel::Rgb => (
                        "RGB Stream (CamA)",
                        format!(
                            "{} @ {:.0} FPS",
                            self.applied_config.resolution.label(),
                            self.fps
                        ),
                    ),
                    ViewPanel::Depth => (
                        "Stereo Depth (CamB + CamC)",
                        format!("Center: {} mm", self.center_distance_mm),
                    ),
                    ViewPanel::Overlay => (
                        if self.align_depth_to_rgb {
                            "RGB + Depth Overlay (Aligned)"
                        } else {
                            "RGB + Depth Overlay"
                        },
                        format!("Opacity: {:.0}%", self.overlay_alpha * 100.0),
                    ),
                    ViewPanel::MonoPair => (
                        "Raw Stereo Mono Pair (Left CamB | Right CamC)",
                        "640x400 GRAY8".to_string(),
                    ),
                };

                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).strong().size(14.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(subtitle)
                                .small()
                                .color(Color32::from_rgb(160, 170, 185)),
                        );
                    });
                });
                ui.separator();

                let body_size = ui.available_size().max(Vec2::splat(32.0));
                ui.allocate_ui_with_layout(
                    body_size,
                    Layout::centered_and_justified(egui::Direction::TopDown),
                    |ui| match panel {
                        ViewPanel::Rgb => {
                            if let Some(tex) = &self.rgb_texture {
                                let size = fit_aspect_ratio(body_size, tex.size_vec2());
                                ui.image((tex.id(), size));
                            } else {
                                ui.label("Waiting for RGB frames...");
                            }
                        }
                        ViewPanel::Depth => {
                            if let Some(tex) = &self.depth_texture {
                                let size = fit_aspect_ratio(body_size, tex.size_vec2());
                                let resp = ui.image((tex.id(), size));
                                if let Some((px, py, mm)) =
                                    inspect_hover_depth(&resp, &self.raw_depth_mm)
                                {
                                    self.hover_distance_mm = Some(("Depth", px, py, mm));
                                }
                            } else {
                                ui.label("Waiting for Stereo Depth frames...");
                            }
                        }
                        ViewPanel::Overlay => {
                            if let Some(tex) = &self.overlay_texture {
                                let size = fit_aspect_ratio(body_size, tex.size_vec2());
                                let resp = ui.image((tex.id(), size));
                                if let Some((px, py, mm)) =
                                    inspect_hover_depth(&resp, &self.raw_depth_mm)
                                {
                                    self.hover_distance_mm = Some(("Overlay", px, py, mm));
                                }
                            } else {
                                ui.label("Waiting for RGB + Depth frames...");
                            }
                        }
                        ViewPanel::MonoPair => {
                            if let Some(tex) = &self.mono_pair_texture {
                                let size = fit_aspect_ratio(body_size, tex.size_vec2());
                                ui.image((tex.id(), size));
                            } else {
                                ui.label("Waiting for Mono L+R frames...");
                            }
                        }
                    },
                );
            });
        });
    }
}

fn update_or_create_texture(
    ctx: &egui::Context,
    slot: &mut Option<TextureHandle>,
    name: &str,
    image: ColorImage,
    options: TextureOptions,
) {
    if let Some(tex) = slot {
        tex.set(image, options);
    } else {
        *slot = Some(ctx.load_texture(name, image, options));
    }
}

impl eframe::App for OakdGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_updates(ctx);

        // Top header bar
        egui::TopBottomPanel::top("top_panel")
            .frame(
                Frame::side_top_panel(&ctx.style())
                    .inner_margin(Margin::symmetric(12.0, 8.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("OAK-D Lite Studio").strong().size(17.0));
                    ui.separator();

                    ui.label(RichText::new("Views:").color(Color32::from_rgb(160, 170, 185)));
                    if ui.toggle_value(&mut self.show_rgb, "RGB (CamA)").changed() {
                        self.sync_live_flags();
                    }
                    if ui
                        .toggle_value(&mut self.show_depth, "Stereo Depth")
                        .changed()
                    {
                        self.sync_live_flags();
                    }
                    if ui
                        .toggle_value(&mut self.show_overlay, "RGB + Depth Overlay")
                        .changed()
                    {
                        self.sync_live_flags();
                    }
                    if self.applied_config.stream_mono_pair
                        && ui
                            .toggle_value(&mut self.show_mono_pair, "Mono L+R")
                            .changed()
                    {
                        self.sync_live_flags();
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.toggle_value(&mut self.show_settings, "⚙ Settings");
                        if ui.button("↻ Reconnect").clicked() {
                            self.live_flags
                                .force_reconnect
                                .store(true, Ordering::Relaxed);
                            self.status = "Reconnecting...".to_string();
                        }

                        if let Some(temp) = self.chip_temp {
                            ui.separator();
                            let temp_color = if temp.average >= 80.0 {
                                Color32::from_rgb(255, 90, 90)
                            } else if temp.average >= 65.0 {
                                Color32::from_rgb(255, 200, 80)
                            } else {
                                Color32::from_rgb(130, 220, 255)
                            };
                            ui.label(
                                RichText::new(format!("Temp: {:.1} °C", temp.average))
                                    .color(temp_color)
                                    .monospace()
                                    .size(12.5),
                            )
                            .on_hover_text(format!(
                                "OAK-D Lite VPU Temperatures:\n\
                                 • Average: {:.1} °C\n\
                                 • CSS (CPU): {:.1} °C\n\
                                 • MSS (Media): {:.1} °C\n\
                                 • UPA (SHAVE): {:.1} °C\n\
                                 • DSS (DRAM): {:.1} °C",
                                temp.average, temp.css, temp.mss, temp.upa, temp.dss
                            ));
                        }

                        ui.separator();
                        let status_color = if self.connected {
                            Color32::from_rgb(80, 220, 100)
                        } else {
                            Color32::from_rgb(255, 180, 60)
                        };
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "● {:.1} FPS — {}",
                                    self.fps, self.status
                                ))
                                .color(status_color)
                                .size(12.5),
                            )
                            .truncate(),
                        );
                    });
                });
            });

        // Bottom metrics & live depth range bar
        egui::TopBottomPanel::bottom("bottom_panel")
            .frame(
                Frame::side_top_panel(&ctx.style())
                    .inner_margin(Margin::symmetric(12.0, 6.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "Center Distance: {} mm ({:.2} m)",
                            self.center_distance_mm,
                            self.center_distance_mm as f32 / 1000.0
                        ))
                        .monospace(),
                    );

                    ui.separator();
                    if let Some((view_name, x, y, mm)) = self.hover_distance_mm {
                        ui.label(
                            RichText::new(format!(
                                "{view_name} ({x}, {y}): {mm} mm ({:.2} m)",
                                mm as f32 / 1000.0
                            ))
                            .monospace()
                            .color(Color32::from_rgb(130, 210, 255)),
                        );
                    } else {
                        ui.label(
                            RichText::new("Hover over Depth / Overlay to inspect pixel distance")
                                .color(Color32::from_rgb(145, 155, 170)),
                        );
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().slider_width = 110.0;
                        if ui
                            .add(
                                egui::Slider::new(&mut self.max_depth_mm, 500.0..=10000.0)
                                    .text("Max (mm)"),
                            )
                            .changed()
                        {
                            if self.max_depth_mm <= self.min_depth_mm + 100.0 {
                                self.min_depth_mm = (self.max_depth_mm - 100.0).max(0.0);
                            }
                            self.sync_live_flags();
                        }
                        ui.separator();
                        if ui
                            .add(
                                egui::Slider::new(&mut self.min_depth_mm, 0.0..=4000.0)
                                    .text("Min (mm)"),
                            )
                            .changed()
                        {
                            if self.min_depth_mm >= self.max_depth_mm - 100.0 {
                                self.max_depth_mm = (self.min_depth_mm + 100.0).min(10000.0);
                            }
                            self.sync_live_flags();
                        }
                    });
                });
            });

        // Collapsible left Settings sidebar
        if self.show_settings {
            egui::SidePanel::left("settings_panel")
                .default_width(310.0)
                .min_width(280.0)
                .max_width(370.0)
                .resizable(true)
                .frame(
                    Frame::side_top_panel(&ctx.style())
                        .inner_margin(Margin::symmetric(10.0, 10.0)),
                )
                .show(ctx, |ui| {
                    egui::TopBottomPanel::bottom("settings_footer")
                        .frame(Frame::none().inner_margin(Margin::symmetric(0.0, 6.0)))
                        .show_inside(ui, |ui| {
                            let has_changes = self.pending_config != self.applied_config;
                            if has_changes {
                                ui.colored_label(
                                    Color32::from_rgb(255, 200, 90),
                                    "● Unapplied pipeline changes",
                                );
                                ui.add_space(4.0);
                            }
                            ui.columns(2, |cols| {
                                cols[0].vertical_centered_justified(|ui| {
                                    if ui
                                        .add_enabled(
                                            has_changes,
                                            egui::Button::new("Apply & Restart"),
                                        )
                                        .clicked()
                                    {
                                        self.apply_pipeline_config();
                                    }
                                });
                                cols[1].vertical_centered_justified(|ui| {
                                    if ui.button("Reset Defaults").clicked() {
                                        self.pending_config = PipelineConfig::default();
                                        self.orientation = CameraImageOrientation::Normal;
                                        self.enhance_depth = true;
                                        self.align_depth_to_rgb = true;
                                        self.overlay_alpha = 0.55;
                                        self.min_depth_mm = 150.0;
                                        self.max_depth_mm = 3000.0;
                                        self.sync_live_flags();
                                    }
                                });
                            });
                        });

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.label(RichText::new("Configuration").strong().size(15.0));
                        ui.add_space(6.0);

                        // Group 1: Live Processing (Instant, no camera restart needed!)
                        Frame::group(ui.style())
                            .inner_margin(Margin::same(8.0))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("1. Live View & Depth Filter (Instant)").strong(),
                                );
                                ui.add_space(4.0);

                                if ui
                                    .checkbox(
                                        &mut self.enhance_depth,
                                        "Enhance Depth (Hole Fill + Median + Temporal)",
                                    )
                                    .changed()
                                {
                                    self.sync_live_flags();
                                }

                                if ui
                                    .checkbox(
                                        &mut self.align_depth_to_rgb,
                                        "Align Depth to RGB FOV & Baseline",
                                    )
                                    .changed()
                                {
                                    self.sync_live_flags();
                                }

                                ui.add_space(4.0);
                                Grid::new("live_grid")
                                    .num_columns(2)
                                    .spacing([10.0, 6.0])
                                    .show(ui, |ui| {
                                        ui.label("Orientation");
                                        egui::ComboBox::from_id_salt("orient_combo")
                                            .selected_text(orientation_label(self.orientation))
                                            .show_ui(ui, |ui| {
                                                for orient in [
                                                    CameraImageOrientation::Normal,
                                                    CameraImageOrientation::Rotate180Deg,
                                                    CameraImageOrientation::HorizontalMirror,
                                                    CameraImageOrientation::VerticalFlip,
                                                ] {
                                                    if ui
                                                        .selectable_value(
                                                            &mut self.orientation,
                                                            orient,
                                                            orientation_label(orient),
                                                        )
                                                        .changed()
                                                    {
                                                        self.sync_live_flags();
                                                    }
                                                }
                                            });
                                        ui.end_row();

                                        ui.label("Overlay Alpha");
                                        if ui
                                            .add(egui::Slider::new(
                                                &mut self.overlay_alpha,
                                                0.0..=1.0,
                                            ))
                                            .changed()
                                        {
                                            self.sync_live_flags();
                                        }
                                        ui.end_row();
                                    });
                            });

                        ui.add_space(8.0);

                        // Group 2: Camera Stream (Device)
                        Frame::group(ui.style())
                            .inner_margin(Margin::same(8.0))
                            .show(ui, |ui| {
                                ui.label(RichText::new("2. Camera Stream (Device)").strong());
                                ui.add_space(4.0);
                                Grid::new("cam_grid")
                                    .num_columns(2)
                                    .spacing([10.0, 6.0])
                                    .show(ui, |ui| {
                                        ui.label("RGB Resolution");
                                        egui::ComboBox::from_id_salt("res_combo")
                                            .selected_text(self.pending_config.resolution.label())
                                            .show_ui(ui, |ui| {
                                                for preset in ResolutionPreset::ALL {
                                                    ui.selectable_value(
                                                        &mut self.pending_config.resolution,
                                                        preset,
                                                        preset.label(),
                                                    );
                                                }
                                            });
                                        ui.end_row();

                                        ui.label("Target FPS");
                                        ui.add(
                                            egui::Slider::new(
                                                &mut self.pending_config.fps,
                                                5.0..=60.0,
                                            )
                                            .step_by(1.0),
                                        );
                                        ui.end_row();

                                        ui.label("Resize Mode");
                                        egui::ComboBox::from_id_salt("resize_combo")
                                            .selected_text(resize_mode_label(
                                                self.pending_config.resize_mode,
                                            ))
                                            .show_ui(ui, |ui| {
                                                for mode in [
                                                    ResizeMode::Crop,
                                                    ResizeMode::Stretch,
                                                    ResizeMode::Letterbox,
                                                ] {
                                                    ui.selectable_value(
                                                        &mut self.pending_config.resize_mode,
                                                        mode,
                                                        resize_mode_label(mode),
                                                    );
                                                }
                                            });
                                        ui.end_row();
                                    });

                                ui.add_space(4.0);
                                ui.checkbox(
                                    &mut self.pending_config.stream_mono_pair,
                                    "Stream Raw Mono Pair (CamB + CamC)",
                                );
                            });

                        ui.add_space(8.0);

                        // Group 3: Stereo Depth (Device)
                        Frame::group(ui.style())
                            .inner_margin(Margin::same(8.0))
                            .show(ui, |ui| {
                                ui.label(RichText::new("3. Stereo Depth (Device)").strong());
                                ui.add_space(4.0);
                                Grid::new("stereo_grid")
                                    .num_columns(2)
                                    .spacing([10.0, 6.0])
                                    .show(ui, |ui| {
                                        ui.label("Disparity Range");
                                        egui::ComboBox::from_id_salt("disp_mode_combo")
                                            .selected_text(
                                                self.pending_config.disparity_mode.label(),
                                            )
                                            .show_ui(ui, |ui| {
                                                for mode in DisparityMode::ALL {
                                                    ui.selectable_value(
                                                        &mut self.pending_config.disparity_mode,
                                                        mode,
                                                        mode.label(),
                                                    );
                                                }
                                            });
                                        ui.end_row();

                                        ui.label("Device Profile");
                                        egui::ComboBox::from_id_salt("stereo_preset_combo")
                                            .selected_text(stereo_preset_label(
                                                self.pending_config.stereo_preset,
                                            ))
                                            .show_ui(ui, |ui| {
                                                for preset in [
                                                    StereoPresetMode::FastDensity,
                                                    StereoPresetMode::FastAccuracy,
                                                    StereoPresetMode::Robotics,
                                                    StereoPresetMode::Default,
                                                    StereoPresetMode::HighDetail,
                                                    StereoPresetMode::Face,
                                                ] {
                                                    ui.selectable_value(
                                                        &mut self.pending_config.stereo_preset,
                                                        preset,
                                                        stereo_preset_label(preset),
                                                    );
                                                }
                                            });
                                        ui.end_row();
                                    });

                                ui.add_space(4.0);
                                ui.checkbox(
                                    &mut self.pending_config.left_right_check,
                                    "Left-Right Consistency Check",
                                );
                                ui.checkbox(
                                    &mut self.pending_config.distortion_correction,
                                    "Stereo Rectification / Distortion Corr.",
                                );
                            });
                    });
                });
        }

        // Main central viewport using a responsive 1x1, 1x2, or 2x2 card grid
        egui::CentralPanel::default()
            .frame(
                Frame::central_panel(&ctx.style())
                    .inner_margin(Margin::same(10.0)),
            )
            .show(ctx, |ui| {
                self.hover_distance_mm = None;

                let mut panels: Vec<ViewPanel> = Vec::with_capacity(4);
                if self.show_rgb {
                    panels.push(ViewPanel::Rgb);
                }
                if self.show_depth {
                    panels.push(ViewPanel::Depth);
                }
                if self.show_overlay {
                    panels.push(ViewPanel::Overlay);
                }
                if self.applied_config.stream_mono_pair && self.show_mono_pair {
                    panels.push(ViewPanel::MonoPair);
                }

                if panels.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label("Select at least one view in the top toolbar.");
                    });
                    return;
                }

                let avail = ui.available_size();
                let gap = 10.0;

                match panels.len() {
                    1 => {
                        self.render_view_card(ui, panels[0], avail);
                    }
                    2 => {
                        let card_w = ((avail.x - gap) * 0.5).max(100.0);
                        let card_size = Vec2::new(card_w, avail.y);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            self.render_view_card(ui, panels[0], card_size);
                            self.render_view_card(ui, panels[1], card_size);
                        });
                    }
                    _ => {
                        let card_w = ((avail.x - gap) * 0.5).max(100.0);
                        let card_h = ((avail.y - gap) * 0.5).max(100.0);
                        let card_size = Vec2::new(card_w, card_h);

                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = gap;
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                self.render_view_card(ui, panels[0], card_size);
                                self.render_view_card(ui, panels[1], card_size);
                            });
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                self.render_view_card(ui, panels[2], card_size);
                                if panels.len() == 4 {
                                    self.render_view_card(ui, panels[3], card_size);
                                }
                            });
                        });
                    }
                }
            });
    }
}

fn inspect_hover_depth(
    response: &egui::Response,
    raw_depth: &Option<(usize, usize, Vec<u16>)>,
) -> Option<(usize, usize, u16)> {
    let (pointer_pos, (w, h, raw)) = (response.hover_pos()?, raw_depth.as_ref()?);
    let rect = response.rect;
    if !rect.contains(pointer_pos) || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let norm_x = ((pointer_pos.x - rect.left()) / rect.width()).clamp(0.0, 0.999);
    let norm_y = ((pointer_pos.y - rect.top()) / rect.height()).clamp(0.0, 0.999);
    let px = (norm_x * (*w as f32)) as usize;
    let py = (norm_y * (*h as f32)) as usize;
    let mm = *raw.get(py * (*w) + px)?;
    Some((px, py, mm))
}

fn spawn_oakd_worker(
    ctx: egui::Context,
    tx: SyncSender<CameraUpdate>,
    live_flags: Arc<LiveViewFlags>,
    shared_config: Arc<Mutex<PipelineConfig>>,
) {
    thread::spawn(move || {
        let mut attempt = 1u32;
        loop {
            live_flags.force_reconnect.store(false, Ordering::Relaxed);

            let config = shared_config
                .lock()
                .map(|c| c.clone())
                .unwrap_or_default();

            let _ = tx.try_send(CameraUpdate {
                rgb_image: None,
                depth_image: None,
                overlay_image: None,
                mono_pair_image: None,
                raw_depth_mm: None,
                center_distance_mm: None,
                chip_temp: None,
                fps: 0.0,
                connected: false,
                status: format!("Connecting (attempt #{attempt})..."),
            });
            ctx.request_repaint();

            match run_oakd_pipeline(&ctx, &tx, &live_flags, &config) {
                Ok(()) => {
                    attempt = 1;
                    let _ = tx.try_send(CameraUpdate {
                        rgb_image: None,
                        depth_image: None,
                        overlay_image: None,
                        mono_pair_image: None,
                        raw_depth_mm: None,
                        center_distance_mm: None,
                        chip_temp: None,
                        fps: 0.0,
                        connected: false,
                        status: "Restarting pipeline...".to_string(),
                    });
                    ctx.request_repaint();
                }
                Err(err) => {
                    let _ = tx.try_send(CameraUpdate {
                        rgb_image: None,
                        depth_image: None,
                        overlay_image: None,
                        mono_pair_image: None,
                        raw_depth_mm: None,
                        center_distance_mm: None,
                        chip_temp: None,
                        fps: 0.0,
                        connected: false,
                        status: format!(
                            "Disconnected ({err}) — retry in {:.1}s (#{attempt})",
                            RECONNECT_DELAY.as_secs_f32()
                        ),
                    });
                    ctx.request_repaint();
                    attempt = attempt.saturating_add(1);
                }
            }

            let start_wait = Instant::now();
            while start_wait.elapsed() < RECONNECT_DELAY {
                thread::sleep(Duration::from_millis(100));
            }
        }
    });
}

fn run_oakd_pipeline(
    ctx: &egui::Context,
    tx: &SyncSender<CameraUpdate>,
    live_flags: &Arc<LiveViewFlags>,
    config: &PipelineConfig,
) -> anyhow::Result<()> {
    let (stream_width, stream_height) = config.resolution.dimensions();
    let (mono_width, mono_height) = config.resolution.mono_dimensions();
    let target_fps = config.fps.clamp(5.0, 60.0);

    let pipeline = Pipeline::new().build()?;
    let device = match pipeline.default_device() {
        Ok(d) => d,
        Err(e) => {
            let _ = pipeline.stop();
            return Err(e.into());
        }
    };
    let platform = match device.platform() {
        Ok(p) => p,
        Err(e) => {
            let _ = pipeline.stop();
            let _ = device.close();
            return Err(e.into());
        }
    };

    let run_result: anyhow::Result<()> = (|| {
        let cam_rgb = pipeline.create_with::<CameraNode, _>(CameraBoardSocket::CamA)?;
        let rgb_out = cam_rgb.request_output(CameraOutputConfig {
            size: (stream_width, stream_height),
            frame_type: Some(ImageFrameType::RGB888i),
            resize_mode: config.resize_mode,
            fps: Some(target_fps),
            enable_undistortion: None,
        })?;

        // Keep CamB and CamC in native calibrated orientation so StereoDepth rectification
        // and disparity search direction (x_L - x_R) remain valid.
        let left = pipeline.create_with::<CameraNode, _>(CameraBoardSocket::CamB)?;
        let right = pipeline.create_with::<CameraNode, _>(CameraBoardSocket::CamC)?;

        let out_left = left.request_output(CameraOutputConfig {
            size: (mono_width, mono_height),
            frame_type: Some(ImageFrameType::GRAY8),
            resize_mode: config.resize_mode,
            fps: Some(target_fps),
            enable_undistortion: None,
        })?;

        let out_right = right.request_output(CameraOutputConfig {
            size: (mono_width, mono_height),
            frame_type: Some(ImageFrameType::GRAY8),
            resize_mode: config.resize_mode,
            fps: Some(target_fps),
            enable_undistortion: None,
        })?;

        let stereo = pipeline.create::<StereoDepthNode>()?;
        stereo.set_default_profile_preset(config.stereo_preset);
        stereo.set_left_right_check(config.left_right_check);
        stereo.set_subpixel(config.disparity_mode == DisparityMode::Subpixel);
        stereo.set_extended_disparity(config.disparity_mode == DisparityMode::Extended);
        stereo.enable_distortion_correction(config.distortion_correction);
        stereo.set_output_size(mono_width as i32, mono_height as i32);
        stereo.set_output_keep_aspect_ratio(config.resize_mode != ResizeMode::Stretch);

        out_left.link_to(stereo.as_node(), Some("left"))?;
        out_right.link_to(stereo.as_node(), Some("right"))?;

        let q_depth = stereo.as_node().output("depth")?.create_queue(2, false)?;
        let q_rgb = rgb_out.create_queue(2, false)?;
        let q_mono = if config.stream_mono_pair {
            Some((
                out_left.create_queue(2, false)?,
                out_right.create_queue(2, false)?,
            ))
        } else {
            None
        };

        pipeline.start()?;

        let mut current_temp = query_chip_temperature(&device);

        let _ = tx.try_send(CameraUpdate {
            rgb_image: None,
            depth_image: None,
            overlay_image: None,
            mono_pair_image: None,
            raw_depth_mm: None,
            center_distance_mm: None,
            chip_temp: current_temp,
            fps: 0.0,
            connected: true,
            status: format!("Connected ({platform:?}) — waiting for frames..."),
        });
        ctx.request_repaint();

        let mut frame_count = 0u32;
        let mut last_fps_inst = Instant::now();
        let mut last_frame_inst = Instant::now();
        let mut current_fps = 0.0f32;
        let mut latest_unoriented_rgb: Option<ColorImage> = None;
        let mut temporal_state: Vec<(u16, u8)> = Vec::new();
        let mut work_buf: Vec<u16> = Vec::new();
        let mut filtered_depth: Vec<u16> = Vec::new();

        loop {
            if live_flags.force_reconnect.swap(false, Ordering::Relaxed) {
                return Ok(());
            }

            if !device.is_connected() {
                anyhow::bail!("USB disconnected");
            }

            let want_rgb = live_flags.show_rgb.load(Ordering::Relaxed);
            let want_depth = live_flags.show_depth.load(Ordering::Relaxed);
            let want_overlay = live_flags.show_overlay.load(Ordering::Relaxed);
            let want_mono = live_flags.show_mono_pair.load(Ordering::Relaxed);
            let orient = code_to_orientation(live_flags.orientation_code.load(Ordering::Relaxed));
            let enhance_depth = live_flags.enhance_depth.load(Ordering::Relaxed);

            let mut got_any_frame = false;

            let rgb_image = if let Some(rgb_frame) = q_rgb.try_next()? {
                got_any_frame = true;
                if want_rgb || want_overlay {
                    let w = rgb_frame.width() as usize;
                    let h = rgb_frame.height() as usize;
                    let bytes = rgb_frame.bytes();
                    let raw_img = rgb_bytes_to_color_image(w, h, &bytes);
                    if want_overlay {
                        latest_unoriented_rgb = Some(raw_img.clone());
                    }
                    if want_rgb {
                        Some(orient_color_image(raw_img, orient))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let mono_pair_image = if let Some((ql, qr)) = &q_mono {
                match (ql.try_next()?, qr.try_next()?) {
                    (Some(fl), Some(fr)) => {
                        got_any_frame = true;
                        if want_mono {
                            let img = combine_mono_pair_side_by_side(
                                fl.width() as usize,
                                fl.height() as usize,
                                &fl.bytes(),
                                fr.width() as usize,
                                fr.height() as usize,
                                &fr.bytes(),
                            );
                            Some(orient_color_image(img, orient))
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            } else {
                None
            };

            let mut center_distance_mm = None;
            let mut raw_depth_mm = None;
            let mut overlay_image = None;

            let depth_image = if let Some(depth_frame) = q_depth.try_next()? {
                got_any_frame = true;
                let w = depth_frame.width() as usize;
                let h = depth_frame.height() as usize;
                let bytes = depth_frame.bytes();

                let min_depth = live_flags.min_depth_mm.load(Ordering::Relaxed) as f32;
                let max_depth = live_flags
                    .max_depth_mm
                    .load(Ordering::Relaxed)
                    .max(live_flags.min_depth_mm.load(Ordering::Relaxed) + 100)
                    as f32;

                let raw_u16 = unpack_depth_u16(w, h, &bytes);
                let depth_slice: &[u16] = if enhance_depth {
                    post_process_depth(
                        w,
                        h,
                        &raw_u16,
                        &mut temporal_state,
                        &mut work_buf,
                        &mut filtered_depth,
                    );
                    &filtered_depth
                } else {
                    &raw_u16
                };

                if want_overlay {
                    if let Some(rgb) = &latest_unoriented_rgb {
                        let alpha_u8 = live_flags.overlay_alpha_u8.load(Ordering::Relaxed) as u8;
                        let align = live_flags.align_depth_to_rgb.load(Ordering::Relaxed);
                        let blended = blend_rgb_and_depth(
                            rgb,
                            w,
                            h,
                            depth_slice,
                            min_depth,
                            max_depth,
                            alpha_u8,
                            align,
                        );
                        overlay_image = Some(orient_color_image(blended, orient));
                    }
                }

                let oriented_u16 = orient_u16_grid(w, h, depth_slice, orient);
                let center_idx = (h / 2) * w + (w / 2);
                center_distance_mm = Some(oriented_u16.get(center_idx).copied().unwrap_or(0));

                let color_img = if want_depth {
                    Some(depth_u16_to_color_image(
                        w,
                        h,
                        &oriented_u16,
                        min_depth,
                        max_depth,
                    ))
                } else {
                    None
                };

                raw_depth_mm = Some((w, h, oriented_u16));
                color_img
            } else {
                None
            };

            if got_any_frame {
                last_frame_inst = Instant::now();
                frame_count += 1;
                let elapsed = last_fps_inst.elapsed().as_secs_f32();
                if elapsed >= 0.5 {
                    current_fps = frame_count as f32 / elapsed;
                    frame_count = 0;
                    last_fps_inst = Instant::now();
                    current_temp = query_chip_temperature(&device);
                }

                let _ = tx.try_send(CameraUpdate {
                    rgb_image,
                    depth_image,
                    overlay_image,
                    mono_pair_image,
                    raw_depth_mm,
                    center_distance_mm,
                    chip_temp: current_temp,
                    fps: current_fps,
                    connected: true,
                    status: format!("Streaming ({platform:?})"),
                });
                ctx.request_repaint();
            } else {
                if last_frame_inst.elapsed() > FRAME_WATCHDOG_TIMEOUT {
                    anyhow::bail!("Stream timed out — resetting connection");
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
    })();

    let _ = pipeline.stop();
    let _ = device.close();
    drop(pipeline);
    drop(device);
    run_result
}

fn unpack_depth_u16(width: usize, height: usize, bytes: &[u8]) -> Vec<u16> {
    let num_pixels = width * height;
    let mut out = Vec::with_capacity(num_pixels);
    for chunk in bytes.chunks_exact(2).take(num_pixels) {
        out.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    out.resize(num_pixels, 0);
    out
}

/// Host-side 3-stage depth post-processor:
/// 1. Outlier clamp (150 mm..12 m) + Temporal EMA & 4-frame persistence
/// 2. Horizontal scanline dropout interpolation (up to 12 px gaps on matching surfaces)
/// 3. 9-tap 5x5 sparse non-zero median + edge-preserving inlier average
fn post_process_depth(
    w: usize,
    h: usize,
    raw: &[u16],
    temporal_state: &mut Vec<(u16, u8)>,
    work_buf: &mut Vec<u16>,
    out_buf: &mut Vec<u16>,
) {
    let n = w * h;
    if temporal_state.len() != n {
        *temporal_state = vec![(0u16, 0u8); n];
    }
    work_buf.clear();
    work_buf.resize(n, 0);
    out_buf.clear();
    out_buf.resize(n, 0);

    // Stage 1: Temporal EMA + Persistence
    for i in 0..n {
        let d = raw.get(i).copied().unwrap_or(0);
        let valid_d = if (150..=12000).contains(&d) { d } else { 0 };
        let (prev_d, age) = temporal_state[i];

        if valid_d > 0 {
            if prev_d > 0 && valid_d.abs_diff(prev_d) < (prev_d / 8).max(80) {
                let blended = ((valid_d as u32 * 3 + prev_d as u32 * 2) / 5) as u16;
                temporal_state[i] = (blended, 4);
                work_buf[i] = blended;
            } else {
                temporal_state[i] = (valid_d, 4);
                work_buf[i] = valid_d;
            }
        } else if age > 0 && prev_d > 0 {
            temporal_state[i] = (prev_d, age - 1);
            work_buf[i] = prev_d;
        } else {
            temporal_state[i] = (0, 0);
            work_buf[i] = 0;
        }
    }

    // Stage 2: Horizontal scanline gap interpolation
    for y in 0..h {
        let row = y * w;
        let mut last_valid_x: Option<(usize, u16)> = None;
        for x in 0..w {
            let v = work_buf[row + x];
            if v > 0 {
                if let Some((lx, lv)) = last_valid_x {
                    let gap = x - lx - 1;
                    if gap > 0 && gap <= 12 && v.abs_diff(lv) < (lv / 6).max(120) {
                        let fill_val = ((lv as u32 + v as u32) / 2) as u16;
                        for gx in (lx + 1)..x {
                            work_buf[row + gx] = fill_val;
                        }
                    }
                }
                last_valid_x = Some((x, v));
            }
        }
    }

    // Stage 3: 5x5 sparse non-zero median + edge-preserving smoothing
    const OFFSETS: [(isize, isize); 9] = [
        (0, 0),
        (-1, 0),
        (1, 0),
        (0, -1),
        (0, 1),
        (-2, 0),
        (2, 0),
        (0, -2),
        (0, 2),
    ];

    for y in 2..(h.saturating_sub(2)) {
        let row = y * w;
        for x in 2..(w.saturating_sub(2)) {
            let center = work_buf[row + x];
            let mut neighbors = [0u16; 9];
            let mut count = 0usize;
            for &(dx, dy) in &OFFSETS {
                let ny = (y as isize + dy) as usize;
                let nx = (x as isize + dx) as usize;
                let nv = work_buf[ny * w + nx];
                if nv > 0 {
                    neighbors[count] = nv;
                    count += 1;
                }
            }
            if count >= 4 {
                neighbors[..count].sort_unstable();
                let median = neighbors[count / 2];
                if center > 0 && center.abs_diff(median) < (median / 6).max(100) {
                    let mut sum = 0u32;
                    let mut inliers = 0u32;
                    let thresh = (median / 8).max(80);
                    for &nv in &neighbors[..count] {
                        if nv.abs_diff(median) <= thresh {
                            sum += nv as u32;
                            inliers += 1;
                        }
                    }
                    out_buf[row + x] = if inliers > 0 {
                        (sum / inliers) as u16
                    } else {
                        median
                    };
                } else {
                    out_buf[row + x] = median;
                }
            }
        }
    }
}

fn depth_u16_to_color_image(
    width: usize,
    height: usize,
    depth_u16: &[u16],
    min_depth_mm: f32,
    max_depth_mm: f32,
) -> ColorImage {
    let num_pixels = width * height;
    let mut pixels = Vec::with_capacity(num_pixels);
    let inv_range_255 = 255.0 / (max_depth_mm - min_depth_mm).max(1.0);

    for &mm in depth_u16.iter().take(num_pixels) {
        if mm == 0 {
            pixels.push(Color32::BLACK);
        } else {
            let lut_idx = (255.0 - ((mm as f32 - min_depth_mm) * inv_range_255))
                .clamp(0.0, 255.0) as usize;
            pixels.push(JET_LUT[lut_idx]);
        }
    }
    pixels.resize(num_pixels, Color32::BLACK);
    ColorImage {
        size: [width, height],
        pixels,
    }
}

fn orient_color_image(mut img: ColorImage, orient: CameraImageOrientation) -> ColorImage {
    let [w, h] = img.size;
    match orient {
        CameraImageOrientation::Auto | CameraImageOrientation::Normal => img,
        CameraImageOrientation::Rotate180Deg => {
            img.pixels.reverse();
            img
        }
        CameraImageOrientation::HorizontalMirror => {
            for y in 0..h {
                let row = y * w;
                img.pixels[row..row + w].reverse();
            }
            img
        }
        CameraImageOrientation::VerticalFlip => {
            for y in 0..(h / 2) {
                let top = y * w;
                let bot = (h - 1 - y) * w;
                for x in 0..w {
                    img.pixels.swap(top + x, bot + x);
                }
            }
            img
        }
    }
}

fn orient_u16_grid(
    w: usize,
    h: usize,
    src: &[u16],
    orient: CameraImageOrientation,
) -> Vec<u16> {
    let mut out = src.to_vec();
    out.resize(w * h, 0);
    match orient {
        CameraImageOrientation::Auto | CameraImageOrientation::Normal => out,
        CameraImageOrientation::Rotate180Deg => {
            out.reverse();
            out
        }
        CameraImageOrientation::HorizontalMirror => {
            for y in 0..h {
                let row = y * w;
                out[row..row + w].reverse();
            }
            out
        }
        CameraImageOrientation::VerticalFlip => {
            for y in 0..(h / 2) {
                let top = y * w;
                let bot = (h - 1 - y) * w;
                for x in 0..w {
                    out.swap(top + x, bot + x);
                }
            }
            out
        }
    }
}

fn rgb_bytes_to_color_image(width: usize, height: usize, bytes: &[u8]) -> ColorImage {
    let expected_len = width * height * 3;
    if bytes.len() >= expected_len {
        ColorImage::from_rgb([width, height], &bytes[..expected_len])
    } else {
        ColorImage::new([width, height], Color32::BLACK)
    }
}

fn combine_mono_pair_side_by_side(
    w1: usize,
    h1: usize,
    b1: &[u8],
    w2: usize,
    h2: usize,
    b2: &[u8],
) -> ColorImage {
    let out_w = w1 + w2;
    let out_h = h1.max(h2);
    let mut pixels = vec![Color32::BLACK; out_w * out_h];

    for y in 0..h1 {
        let src_row = y * w1;
        let dst_row = y * out_w;
        for x in 0..w1 {
            let v = b1.get(src_row + x).copied().unwrap_or(0);
            pixels[dst_row + x] = Color32::from_gray(v);
        }
    }
    for y in 0..h2 {
        let src_row = y * w2;
        let dst_row = y * out_w + w1;
        for x in 0..w2 {
            let v = b2.get(src_row + x).copied().unwrap_or(0);
            pixels[dst_row + x] = Color32::from_gray(v);
        }
    }

    ColorImage {
        size: [out_w, out_h],
        pixels,
    }
}

#[allow(clippy::too_many_arguments)]
fn blend_rgb_and_depth(
    rgb: &ColorImage,
    dw: usize,
    dh: usize,
    raw_depth_mm: &[u16],
    min_depth_mm: f32,
    max_depth_mm: f32,
    alpha_u8: u8,
    align_to_rgb: bool,
) -> ColorImage {
    let [rw, rh] = rgb.size;
    if rw == 0 || rh == 0 || dw == 0 || dh == 0 {
        return rgb.clone();
    }
    let inv_range_255 = 255.0 / (max_depth_mm - min_depth_mm).max(1.0);
    let a = alpha_u8 as u16;
    let inv_a = 255u16 - a;
    let mut pixels = Vec::with_capacity(rw * rh);

    // OAK-D Lite CamA (RGB ~68.8° HFOV) vs CamC (Right Mono ~72.9° HFOV, 37.5mm baseline)
    let fov_scale = if align_to_rgb { 0.935f32 } else { 1.0f32 };
    let parallax_num = (37.5f32 * (dw as f32) * 0.68f32) as i32;

    for y in 0..rh {
        let ny = (y as f32 / rh as f32) - 0.5;
        let dy_f = (0.5 + ny * fov_scale) * (dh as f32);
        let dy = (dy_f as isize).clamp(0, dh as isize - 1) as usize;
        let depth_row = dy * dw;
        let rgb_row = y * rw;

        for x in 0..rw {
            let nx = (x as f32 / rw as f32) - 0.5;
            let base_dx_f = (0.5 + nx * fov_scale) * (dw as f32);
            let base_dx = (base_dx_f as isize).clamp(0, dw as isize - 1) as usize;

            let mut mm = raw_depth_mm.get(depth_row + base_dx).copied().unwrap_or(0);
            if align_to_rgb && mm > 120 {
                let parallax_px = parallax_num / (mm as i32);
                let aligned_dx =
                    ((base_dx as isize) - (parallax_px as isize)).clamp(0, dw as isize - 1) as usize;
                let aligned_mm = raw_depth_mm.get(depth_row + aligned_dx).copied().unwrap_or(0);
                if aligned_mm > 0 {
                    mm = aligned_mm;
                }
            }

            let base = rgb.pixels[rgb_row + x];
            if mm == 0 {
                pixels.push(base);
            } else {
                let lut_idx = (255.0 - ((mm as f32 - min_depth_mm) * inv_range_255))
                    .clamp(0.0, 255.0) as usize;
                let jet = JET_LUT[lut_idx];
                let r = ((base.r() as u16 * inv_a + jet.r() as u16 * a) >> 8) as u8;
                let g = ((base.g() as u16 * inv_a + jet.g() as u16 * a) >> 8) as u8;
                let b = ((base.b() as u16 * inv_a + jet.b() as u16 * a) >> 8) as u8;
                pixels.push(Color32::from_rgb(r, g, b));
            }
        }
    }

    ColorImage {
        size: [rw, rh],
        pixels,
    }
}

fn fit_aspect_ratio(available: Vec2, image_size: Vec2) -> Vec2 {
    if image_size.x <= 0.0 || image_size.y <= 0.0 {
        return available;
    }
    let scale = (available.x / image_size.x)
        .min(available.y / image_size.y)
        .max(0.1);
    Vec2::new(image_size.x * scale, image_size.y * scale)
}

fn resize_mode_label(mode: ResizeMode) -> &'static str {
    match mode {
        ResizeMode::Crop => "Crop",
        ResizeMode::Stretch => "Stretch",
        ResizeMode::Letterbox => "Letterbox",
    }
}

fn orientation_to_code(orient: CameraImageOrientation) -> u32 {
    match orient {
        CameraImageOrientation::Auto | CameraImageOrientation::Normal => 0,
        CameraImageOrientation::Rotate180Deg => 1,
        CameraImageOrientation::HorizontalMirror => 2,
        CameraImageOrientation::VerticalFlip => 3,
    }
}

fn code_to_orientation(code: u32) -> CameraImageOrientation {
    match code {
        1 => CameraImageOrientation::Rotate180Deg,
        2 => CameraImageOrientation::HorizontalMirror,
        3 => CameraImageOrientation::VerticalFlip,
        _ => CameraImageOrientation::Normal,
    }
}

fn orientation_label(orient: CameraImageOrientation) -> &'static str {
    match orient {
        CameraImageOrientation::Auto | CameraImageOrientation::Normal => "Normal",
        CameraImageOrientation::Rotate180Deg => "Rotate 180° (Upside-Down)",
        CameraImageOrientation::HorizontalMirror => "Horizontal Mirror",
        CameraImageOrientation::VerticalFlip => "Vertical Flip",
    }
}

fn stereo_preset_label(preset: StereoPresetMode) -> &'static str {
    match preset {
        StereoPresetMode::FastDensity => "Fast Density (Full 640x400, Recommended)",
        StereoPresetMode::FastAccuracy => "Fast Accuracy (Full 640x400)",
        StereoPresetMode::Robotics => "Robotics (Decimated 320x200)",
        StereoPresetMode::Default => "Default (Decimated 320x200)",
        StereoPresetMode::Face => "Face (Decimated 320x200)",
        StereoPresetMode::HighDetail => "High Detail (Decimated 320x200)",
    }
}

fn main() -> eframe::Result<()> {
    set_c_env("DEPTHAI_DISABLE_CRASHDUMP_COLLECTION", "1");
    set_c_env("DEPTHAI_CRASHDUMP", "0");
    set_c_env("DEPTHAI_CRASHDUMP_TIMEOUT", "0");

    let (tx, rx) = mpsc::sync_channel::<CameraUpdate>(2);
    let live_flags = Arc::new(LiveViewFlags::default());
    let shared_config = Arc::new(Mutex::new(PipelineConfig::default()));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OAK-D Lite Studio — RGB & Stereo Depth Viewer (Rust)")
            .with_inner_size([1360.0, 740.0])
            .with_min_inner_size([920.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "OAK-D Lite Studio",
        options,
        Box::new(move |cc| {
            spawn_oakd_worker(
                cc.egui_ctx.clone(),
                tx,
                Arc::clone(&live_flags),
                Arc::clone(&shared_config),
            );
            Ok(Box::new(OakdGuiApp::new(rx, live_flags, shared_config)))
        }),
    )
}
