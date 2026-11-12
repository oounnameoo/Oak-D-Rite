#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use depthai::camera::{CameraNode, CameraOutputConfig};
use depthai::common::{CameraBoardSocket, ImageFrameType, ResizeMode};
use depthai::device::Device;
use depthai::pipeline::Pipeline;
use depthai::stereo_depth::StereoDepthNode;
use depthai::StereoPresetMode;
use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions, Vec2};

const STREAM_WIDTH: u32 = 640;
const STREAM_HEIGHT: u32 = 400;
const FRAME_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(4);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// Packet sent from the OAK-D Lite worker thread to the GUI thread.
struct CameraUpdate {
    rgb_image: Option<ColorImage>,
    depth_image: Option<ColorImage>,
    raw_depth_mm: Option<(usize, usize, Vec<u16>)>,
    center_distance_mm: Option<u16>,
    fps: f32,
    connected: bool,
    status: String,
}

struct OakdGuiApp {
    rx: Receiver<CameraUpdate>,
    rgb_texture: Option<TextureHandle>,
    depth_texture: Option<TextureHandle>,
    raw_depth_mm: Option<(usize, usize, Vec<u16>)>,
    center_distance_mm: u16,
    hover_distance_mm: Option<(usize, usize, u16)>,
    fps: f32,
    connected: bool,
    status: String,
    max_depth_mm: f32,
    shared_max_depth_mm: Arc<AtomicU32>,
    force_reconnect: Arc<AtomicBool>,
    show_rgb: bool,
    show_depth: bool,
}

impl OakdGuiApp {
    fn new(
        _cc: &eframe::CreationContext<'_>,
        rx: Receiver<CameraUpdate>,
        shared_max_depth_mm: Arc<AtomicU32>,
        force_reconnect: Arc<AtomicBool>,
    ) -> Self {
        Self {
            rx,
            rgb_texture: None,
            depth_texture: None,
            raw_depth_mm: None,
            center_distance_mm: 0,
            hover_distance_mm: None,
            fps: 0.0,
            connected: false,
            status: "Searching for OAK-D Lite...".to_string(),
            max_depth_mm: 3000.0, // 3 meters default visualization range
            shared_max_depth_mm,
            force_reconnect,
            show_rgb: true,
            show_depth: true,
        }
    }
}

impl eframe::App for OakdGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain latest frame updates from the OAK-D Lite worker thread
        while let Ok(update) = self.rx.try_recv() {
            if let Some(rgb) = update.rgb_image {
                self.rgb_texture = Some(ctx.load_texture(
                    "oakd_rgb",
                    rgb,
                    TextureOptions::LINEAR,
                ));
            }
            if let Some(depth) = update.depth_image {
                self.depth_texture = Some(ctx.load_texture(
                    "oakd_depth",
                    depth,
                    TextureOptions::NEAREST,
                ));
            }
            if let Some(raw_depth) = update.raw_depth_mm {
                self.raw_depth_mm = Some(raw_depth);
            }
            if let Some(center_mm) = update.center_distance_mm {
                self.center_distance_mm = center_mm;
            }
            self.fps = update.fps;
            self.connected = update.connected;
            self.status = update.status;
        }

        // Top toolbar
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("OAK-D Lite — Rust RGB & Stereo Depth Viewer");
                ui.separator();

                let status_color = if self.connected {
                    Color32::from_rgb(80, 220, 100)
                } else {
                    Color32::from_rgb(255, 180, 60)
                };
                ui.colored_label(status_color, format!("● {}", self.status));

                if ui.button("Reconnect Now").clicked() {
                    self.force_reconnect.store(true, Ordering::Relaxed);
                    self.status = "Reconnecting to OAK-D Lite...".to_string();
                }

                ui.separator();
                ui.label(format!("FPS: {:.1}", self.fps));
                ui.separator();
                ui.checkbox(&mut self.show_rgb, "RGB (CamA)");
                ui.checkbox(&mut self.show_depth, "Depth (CamB + CamC)");
            });
        });

        // Bottom metrics bar
        egui::TopBottomPanel::bottom("bottom_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "Center Distance: {} mm ({:.2} m)",
                    self.center_distance_mm,
                    self.center_distance_mm as f32 / 1000.0
                ));
                ui.separator();
                if let Some((x, y, mm)) = self.hover_distance_mm {
                    ui.label(format!(
                        "Cursor ({}, {}): {} mm ({:.2} m)",
                        x,
                        y,
                        mm,
                        mm as f32 / 1000.0
                    ));
                } else {
                    ui.label("Hover over the Depth view to inspect pixel distance");
                }
                ui.separator();
                if ui
                    .add(
                        egui::Slider::new(&mut self.max_depth_mm, 500.0..=8000.0)
                            .text("Max Depth Range (mm)"),
                    )
                    .changed()
                {
                    self.shared_max_depth_mm
                        .store(self.max_depth_mm as u32, Ordering::Relaxed);
                }
            });
        });

        // Main central viewport showing RGB and Stereo Depth streams side-by-side
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(
                if self.show_rgb && self.show_depth { 2 } else { 1 },
                |columns| {
                    let mut col_idx = 0;

                    if self.show_rgb {
                        let ui = &mut columns[col_idx];
                        ui.vertical_centered(|ui| {
                            ui.heading("RGB Stream (CamA)");
                            if let Some(tex) = &self.rgb_texture {
                                let size = fit_aspect_ratio(ui.available_size(), tex.size_vec2());
                                ui.image((tex.id(), size));
                            } else {
                                ui.centered_and_justified(|ui| {
                                    ui.label("Waiting for RGB frames from OAK-D Lite...");
                                });
                            }
                        });
                        if self.show_depth {
                            col_idx += 1;
                        }
                    }

                    if self.show_depth {
                        let ui = &mut columns[col_idx];
                        ui.vertical_centered(|ui| {
                            ui.heading("Stereo Depth Map (CamB + CamC)");
                            if let Some(tex) = &self.depth_texture {
                                let size = fit_aspect_ratio(ui.available_size(), tex.size_vec2());
                                let response = ui.image((tex.id(), size));

                                // Inspect depth in mm when hovering over the depth image
                                self.hover_distance_mm = None;
                                if let (Some(pointer_pos), Some((w, h, raw))) =
                                    (response.hover_pos(), &self.raw_depth_mm)
                                {
                                    let rect = response.rect;
                                    if rect.contains(pointer_pos)
                                        && rect.width() > 0.0
                                        && rect.height() > 0.0
                                    {
                                        let norm_x = ((pointer_pos.x - rect.left()) / rect.width())
                                            .clamp(0.0, 0.999);
                                        let norm_y = ((pointer_pos.y - rect.top()) / rect.height())
                                            .clamp(0.0, 0.999);
                                        let px = (norm_x * (*w as f32)) as usize;
                                        let py = (norm_y * (*h as f32)) as usize;
                                        let idx = py * (*w) + px;
                                        if let Some(&mm) = raw.get(idx) {
                                            self.hover_distance_mm = Some((px, py, mm));
                                        }
                                    }
                                }
                            } else {
                                ui.centered_and_justified(|ui| {
                                    ui.label("Waiting for Stereo Depth frames from OAK-D Lite...");
                                });
                            }
                        });
                    }
                },
            );
        });

        // Request continuous repaint for live video
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}

/// Spawns the background thread that manages the OAK-D Lite pipeline and auto-reconnects on disconnect/error.
fn spawn_oakd_worker(
    tx: SyncSender<CameraUpdate>,
    shared_max_depth_mm: Arc<AtomicU32>,
    force_reconnect: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut attempt = 1u32;
        loop {
            force_reconnect.store(false, Ordering::Relaxed);

            let _ = tx.try_send(CameraUpdate {
                rgb_image: None,
                depth_image: None,
                raw_depth_mm: None,
                center_distance_mm: None,
                fps: 0.0,
                connected: false,
                status: format!("Connecting to OAK-D Lite (attempt #{attempt})..."),
            });

            match run_oakd_pipeline(&tx, &shared_max_depth_mm, &force_reconnect) {
                Ok(()) => {
                    attempt = 1;
                    let _ = tx.try_send(CameraUpdate {
                        rgb_image: None,
                        depth_image: None,
                        raw_depth_mm: None,
                        center_distance_mm: None,
                        fps: 0.0,
                        connected: false,
                        status: "Reconnecting to OAK-D Lite...".to_string(),
                    });
                }
                Err(err) => {
                    let _ = tx.try_send(CameraUpdate {
                        rgb_image: None,
                        depth_image: None,
                        raw_depth_mm: None,
                        center_distance_mm: None,
                        fps: 0.0,
                        connected: false,
                        status: format!(
                            "Disconnected ({err}) — auto-reconnecting in {}s (attempt #{attempt})...",
                            RECONNECT_DELAY.as_secs()
                        ),
                    });
                    attempt = attempt.saturating_add(1);
                }
            }

            // Sleep in small increments so "Reconnect Now" wakes up immediately
            let start_wait = Instant::now();
            while start_wait.elapsed() < RECONNECT_DELAY {
                if force_reconnect.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    });
}

fn run_oakd_pipeline(
    tx: &SyncSender<CameraUpdate>,
    shared_max_depth_mm: &Arc<AtomicU32>,
    force_reconnect: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    // 1. Open connection to OAK-D Lite
    let device = Device::new()?;
    let platform = device.platform()?;

    // 2. Build pipeline attached to the device
    let pipeline = Pipeline::new().with_device(&device).build()?;

    // 3. Configure Center RGB Camera (CamA)
    let cam_rgb = pipeline.create_with::<CameraNode, _>(CameraBoardSocket::CamA)?;
    let rgb_out = cam_rgb.request_output(CameraOutputConfig {
        size: (STREAM_WIDTH, STREAM_HEIGHT),
        frame_type: Some(ImageFrameType::RGB888i),
        resize_mode: ResizeMode::Crop,
        fps: Some(30.0),
        enable_undistortion: None,
    })?;

    // 4. Configure Left (CamB) and Right (CamC) Stereo Cameras + StereoDepthNode
    // Explicitly request 8-bit GRAY8 outputs so stride (640) equals width (640).
    let left = pipeline.create_with::<CameraNode, _>(CameraBoardSocket::CamB)?;
    let right = pipeline.create_with::<CameraNode, _>(CameraBoardSocket::CamC)?;

    let out_left = left.request_output(CameraOutputConfig {
        size: (STREAM_WIDTH, STREAM_HEIGHT),
        frame_type: Some(ImageFrameType::GRAY8),
        resize_mode: ResizeMode::Crop,
        fps: Some(30.0),
        enable_undistortion: None,
    })?;

    let out_right = right.request_output(CameraOutputConfig {
        size: (STREAM_WIDTH, STREAM_HEIGHT),
        frame_type: Some(ImageFrameType::GRAY8),
        resize_mode: ResizeMode::Crop,
        fps: Some(30.0),
        enable_undistortion: None,
    })?;

    let stereo = pipeline.create::<StereoDepthNode>()?;
    stereo.set_default_profile_preset(StereoPresetMode::Robotics);
    stereo.set_left_right_check(true);
    stereo.set_output_size(STREAM_WIDTH as i32, STREAM_HEIGHT as i32);
    stereo.set_output_keep_aspect_ratio(true);

    out_left.link_to(stereo.as_node(), Some("left"))?;
    out_right.link_to(stereo.as_node(), Some("right"))?;

    // 5. Create host output queues (max_size = 4, blocking = false)
    let q_rgb = rgb_out.create_queue(4, false)?;
    let q_depth = stereo.as_node().output("depth")?.create_queue(4, false)?;

    // 6. Start the pipeline
    pipeline.start()?;

    let _ = tx.try_send(CameraUpdate {
        rgb_image: None,
        depth_image: None,
        raw_depth_mm: None,
        center_distance_mm: None,
        fps: 0.0,
        connected: true,
        status: format!("Connected ({platform:?}) — waiting for first frame..."),
    });

    let mut frame_count = 0u32;
    let mut last_fps_inst = Instant::now();
    let mut last_frame_inst = Instant::now();
    let mut current_fps = 0.0f32;

    loop {
        if force_reconnect.load(Ordering::Relaxed) {
            return Ok(());
        }

        if !device.is_connected() {
            anyhow::bail!("OAK-D Lite USB disconnected");
        }

        // Pull RGB frame
        let rgb_image = if let Some(rgb_frame) = q_rgb.try_next()? {
            let w = rgb_frame.width() as usize;
            let h = rgb_frame.height() as usize;
            let bytes = rgb_frame.bytes();
            Some(rgb_bytes_to_color_image(w, h, &bytes))
        } else {
            None
        };

        // Pull Stereo Depth frame (u16 millimeters per pixel)
        let mut center_distance_mm = None;
        let mut raw_depth_mm = None;
        let depth_image = if let Some(depth_frame) = q_depth.try_next()? {
            let w = depth_frame.width() as usize;
            let h = depth_frame.height() as usize;
            let bytes = depth_frame.bytes();

            let max_depth = shared_max_depth_mm.load(Ordering::Relaxed).max(100) as f32;
            let (color_img, depth_u16, center_mm) =
                depth_bytes_to_color_image(w, h, &bytes, max_depth);
            center_distance_mm = Some(center_mm);
            raw_depth_mm = Some((w, h, depth_u16));
            Some(color_img)
        } else {
            None
        };

        if rgb_image.is_some() || depth_image.is_some() {
            last_frame_inst = Instant::now();
            frame_count += 1;
            let elapsed = last_fps_inst.elapsed().as_secs_f32();
            if elapsed >= 0.5 {
                current_fps = frame_count as f32 / elapsed;
                frame_count = 0;
                last_fps_inst = Instant::now();
            }

            let _ = tx.try_send(CameraUpdate {
                rgb_image,
                depth_image,
                raw_depth_mm,
                center_distance_mm,
                fps: current_fps,
                connected: true,
                status: format!("Streaming ({platform:?})"),
            });
        } else {
            // If no frames arrive within the watchdog window, reset and reconnect
            if last_frame_inst.elapsed() > FRAME_WATCHDOG_TIMEOUT {
                anyhow::bail!("Stream timed out — resetting connection");
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Converts interleaved RGB888 bytes into an `egui::ColorImage`.
fn rgb_bytes_to_color_image(width: usize, height: usize, bytes: &[u8]) -> ColorImage {
    let expected_len = width * height * 3;
    if bytes.len() >= expected_len {
        ColorImage::from_rgb([width, height], &bytes[..expected_len])
    } else {
        ColorImage::new([width, height], Color32::BLACK)
    }
}

/// Converts RAW16 little-endian depth bytes (millimeters) into a colorized Jet/Turbo heatmap.
fn depth_bytes_to_color_image(
    width: usize,
    height: usize,
    bytes: &[u8],
    max_depth_mm: f32,
) -> (ColorImage, Vec<u16>, u16) {
    let num_pixels = width * height;
    let mut pixels = Vec::with_capacity(num_pixels);
    let mut depth_u16 = Vec::with_capacity(num_pixels);

    for chunk in bytes.chunks_exact(2).take(num_pixels) {
        let mm = u16::from_le_bytes([chunk[0], chunk[1]]);
        depth_u16.push(mm);

        if mm == 0 {
            pixels.push(Color32::BLACK);
        } else {
            // Normalize closer objects -> warm (red/yellow), farther -> cool (blue)
            let norm = 1.0 - (mm as f32 / max_depth_mm).clamp(0.0, 1.0);
            pixels.push(colormap_jet(norm));
        }
    }

    // Pad if buffer was shorter than expected
    pixels.resize(num_pixels, Color32::BLACK);
    depth_u16.resize(num_pixels, 0);

    let center_idx = (height / 2) * width + (width / 2);
    let center_mm = depth_u16.get(center_idx).copied().unwrap_or(0);

    (
        ColorImage {
            size: [width, height],
            pixels,
        },
        depth_u16,
        center_mm,
    )
}

/// Maps a normalized value `t` in `[0.0, 1.0]` to an RGB Jet/Turbo-style color.
fn colormap_jet(t: f32) -> Color32 {
    let r = (1.5 - (4.0 * t - 3.0).abs()).clamp(0.0, 1.0);
    let g = (1.5 - (4.0 * t - 2.0).abs()).clamp(0.0, 1.0);
    let b = (1.5 - (4.0 * t - 1.0).abs()).clamp(0.0, 1.0);
    Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// Scales an image size to fit within the available UI space while preserving aspect ratio.
fn fit_aspect_ratio(available: Vec2, image_size: Vec2) -> Vec2 {
    if image_size.x <= 0.0 || image_size.y <= 0.0 {
        return available;
    }
    let scale = (available.x / image_size.x)
        .min(available.y / image_size.y)
        .max(0.1);
    Vec2::new(image_size.x * scale, image_size.y * scale)
}

fn main() -> eframe::Result<()> {
    let (tx, rx) = mpsc::sync_channel::<CameraUpdate>(2);
    let shared_max_depth_mm = Arc::new(AtomicU32::new(3000));
    let force_reconnect = Arc::new(AtomicBool::new(false));
    spawn_oakd_worker(
        tx,
        Arc::clone(&shared_max_depth_mm),
        Arc::clone(&force_reconnect),
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OAK-D Lite — RGB & Stereo Depth Viewer (Rust)")
            .with_inner_size([1280.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "OAK-D Lite Viewer",
        options,
        Box::new(|cc| {
            Ok(Box::new(OakdGuiApp::new(
                cc,
                rx,
                shared_max_depth_mm,
                force_reconnect,
            )))
        }),
    )
}
