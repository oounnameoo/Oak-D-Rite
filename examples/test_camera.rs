use std::ffi::{c_void, CString};
use std::os::raw::{c_char, c_int};
use std::time::Duration;

use depthai::camera::{CameraNode, CameraOutputConfig};
use depthai::common::{CameraBoardSocket, ImageFrameType, ResizeMode};
use depthai::pipeline::Pipeline;
use depthai::Device;

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

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct ChipTemperature {
    css: f32,     // CPU Subsystem (°C)
    mss: f32,     // Media Subsystem (°C)
    upa: f32,     // Shave Array (°C)
    dss: f32,     // DRAM Subsystem (°C)
    average: f32, // Average (°C)
}

fn query_chip_temperature(device: &Device) -> Option<ChipTemperature> {
    if !device.is_connected() {
        return None;
    }
    unsafe {
        // `depthai::Device` wraps a single pointer (`DaiDevice` = `*mut std::shared_ptr<dai::Device>`).
        // The first pointer inside `std::shared_ptr<dai::Device>` is `dai::Device*` (`dai::DeviceBase*`).
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

fn main() {
    set_c_env("DEPTHAI_DISABLE_CRASHDUMP_COLLECTION", "1");
    set_c_env("DEPTHAI_CRASHDUMP", "0");
    set_c_env("DEPTHAI_CRASHDUMP_TIMEOUT", "0");

    let pipeline = Pipeline::new().build().expect("Pipeline build");
    let device = pipeline.default_device().expect("Default device");

    let cam_rgb = pipeline
        .create_with::<CameraNode, _>(CameraBoardSocket::CamA)
        .unwrap();
    let rgb_out = cam_rgb
        .request_output(CameraOutputConfig {
            size: (640, 400),
            frame_type: Some(ImageFrameType::RGB888i),
            resize_mode: ResizeMode::Crop,
            fps: Some(30.0),
            enable_undistortion: None,
        })
        .unwrap();
    let _q_rgb = rgb_out.create_queue(2, false).unwrap();

    pipeline.start().expect("Pipeline start");
    std::thread::sleep(Duration::from_millis(500));

    let temp = query_chip_temperature(&device);
    println!("OAK-D Lite Chip Temperature: {temp:?}");

    let _ = pipeline.stop();
    let _ = device.close();
}
