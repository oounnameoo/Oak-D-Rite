use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Workaround for depthai-sys 0.1.3 downloading opencv_world4110.dll
    // while depthai-core v3.6.1 links against opencv_world4130.dll and delay-loads libusb-1.0.dll on Windows.
    let out_dir = match env::var("OUT_DIR") {
        Ok(v) => PathBuf::from(v),
        Err(_) => return,
    };

    // Navigate from target/<profile>/build/<pkg>/out -> target/<profile>
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };

    let src_4110 = profile_dir.join("opencv_world4110.dll");
    let dst_4130 = profile_dir.join("opencv_world4130.dll");
    if src_4110.exists() && !dst_4130.exists() {
        let _ = fs::copy(&src_4110, &dst_4130);
    }

    let src_4110d = profile_dir.join("opencv_world4110d.dll");
    let dst_4130d = profile_dir.join("opencv_world4130d.dll");
    if src_4110d.exists() && !dst_4130d.exists() {
        let _ = fs::copy(&src_4110d, &dst_4130d);
    }

    // Ensure libusb-1.0.dll (delay-loaded by depthai-core.dll for USB XLink) is staged
    let dst_libusb = profile_dir.join("libusb-1.0.dll");
    if !dst_libusb.exists() {
        let mut candidates: Vec<PathBuf> = Vec::new();

        if let Ok(custom_libusb) = env::var("LIBUSB_DLL_PATH") {
            candidates.push(PathBuf::from(custom_libusb));
        }

        if let Some(target_dir) = profile_dir.parent() {
            candidates.push(
                target_dir
                    .join("dai-build")
                    .join("v3.6.1")
                    .join("depthai-core")
                    .join("bin")
                    .join("libusb-1.0.dll"),
            );
        }

        if let Ok(path_env) = env::var("PATH") {
            for dir in env::split_paths(&path_env) {
                candidates.push(dir.join("libusb-1.0.dll"));
            }
        }

        candidates.extend([
            PathBuf::from(r"C:\Windows\System32\libusb-1.0.dll"),
            PathBuf::from(r"C:\Program Files\PICO Connect\openvr_driver\bin\win64\libusb-1.0.dll"),
            PathBuf::from(
                r"C:\Program Files\Epic Games\UE_5.5\Engine\Extras\ThirdPartyNotUE\libimobiledevice\x64\libusb-1.0.dll",
            ),
        ]);

        for candidate in candidates {
            if candidate.exists() {
                let _ = fs::copy(&candidate, &dst_libusb);
                break;
            }
        }
    }
}
