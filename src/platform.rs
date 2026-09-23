//! What the console asks of the machine it runs on: the time zone, where the
//! project lives, and the window's icon.

use std::path::PathBuf;

/// Seconds east of UTC. Windows answers this without a date library.
#[cfg(windows)]
pub fn local_offset() -> i64 {
    use std::mem::zeroed;
    #[allow(non_snake_case)]
    #[repr(C)]
    struct TimeZoneInformation {
        Bias: i32,
        StandardName: [u16; 32],
        StandardDate: [u16; 8],
        StandardBias: i32,
        DaylightName: [u16; 32],
        DaylightDate: [u16; 8],
        DaylightBias: i32,
    }
    extern "system" {
        fn GetTimeZoneInformation(info: *mut TimeZoneInformation) -> u32;
    }
    unsafe {
        let mut info: TimeZoneInformation = zeroed();
        let result = GetTimeZoneInformation(&mut info);
        // 0 unknown, 1 standard, 2 daylight; the bias is minutes *west*.
        let extra = match result {
            2 => info.DaylightBias,
            _ => info.StandardBias,
        };
        -((info.Bias + extra) as i64) * 60
    }
}

#[cfg(not(windows))]
pub fn local_offset() -> i64 {
    0
}

/// Walk up from the executable for the project, so a debug build run from
/// anywhere still finds the library and the station.
pub fn project_root() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        while let Some(current) = dir {
            if current.join("radio").join("__main__.py").is_file()
                || current.join("cache").join("station.db").is_file()
            {
                return current;
            }
            dir = current.parent().map(|p| p.to_path_buf());
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// The window icon.
///
/// Decoded rather than drawn: the artwork is a real asset now, and the image
/// crate is already here for screenshots, so this costs nothing new. The
/// executable gets the same icon stamped into its resource table by build.rs,
/// which is what Explorer and the taskbar read -- neither asks the running
/// process what it would like to look like.
pub fn window_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../icons/icon.png");
    let image = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData { rgba: image.into_raw(), width, height })
}
