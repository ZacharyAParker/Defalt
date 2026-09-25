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

static LAUNCHED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Noted first thing in `main`, so startup can be timed from it.
pub fn mark_launch() {
    LAUNCHED.get_or_init(std::time::Instant::now);
}

pub fn since_launch() -> std::time::Duration {
    LAUNCHED.get_or_init(std::time::Instant::now).elapsed()
}

/// Round the window's corners (Windows 11), or give it back its default.
/// Older Windows doesn't know the attribute and says so, which is fine.
#[cfg(windows)]
pub fn round_corners(hwnd: isize, round: bool) {
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attribute: u32, value: *const std::ffi::c_void, size: u32) -> i32;
    }
    // DWMWA_WINDOW_CORNER_PREFERENCE; DWMWCP_ROUND or DWMWCP_DEFAULT.
    let preference: u32 = if round { 2 } else { 0 };
    unsafe {
        DwmSetWindowAttribute(hwnd, 33, (&preference as *const u32).cast(), std::mem::size_of::<u32>() as u32);
    }
}

#[cfg(not(windows))]
pub fn round_corners(_hwnd: isize, _round: bool) {}

/// Cloak the window: Windows still lets it draw, but doesn't show it. The
/// console sits cloaked at its full size behind the startup splash, so it
/// never has to be resized in front of anyone.
#[cfg(windows)]
pub fn cloak(hwnd: isize, cloaked: bool) -> bool {
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attribute: u32, value: *const std::ffi::c_void, size: u32) -> i32;
    }
    // DWMWA_CLOAK, a BOOL.
    let value: i32 = cloaked.into();
    unsafe { DwmSetWindowAttribute(hwnd, 13, (&value as *const i32).cast(), 4) == 0 }
}

#[cfg(not(windows))]
pub fn cloak(_hwnd: isize, _cloaked: bool) -> bool {
    false
}

/// A top-level window of ours, by its title.
#[cfg(windows)]
pub fn find_window(title: &str) -> Option<isize> {
    #[link(name = "user32")]
    extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> isize;
    }
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    match unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) } {
        0 => None,
        hwnd => Some(hwnd),
    }
}

#[cfg(not(windows))]
pub fn find_window(_title: &str) -> Option<isize> {
    None
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
