/*!
 * SecureNote — Native Desktop Edition (egui / eframe)
 *
 * Cross-platform: Windows · macOS · Linux
 * Encrypted note storage is wire-format compatible with the web (Rust/Node) version.
 *
 * Crypto wire format: base64( salt[32] || iv[12] || tag[16] || ciphertext )
 */

// NOTE: We do NOT set #![windows_subsystem = "windows"] here.
// eframe sets it internally for release builds. Setting it ourselves
// causes silent startup failures on Windows because panics and early
// process::exit() calls produce no visible output whatsoever.

mod app;
mod crypto;
mod markdown;
mod storage;
mod vim;

use app::SecureNote;
use clap::Parser;
use eframe::{egui, wgpu};
use std::{fs, io::Write as _, path::PathBuf, process};
use storage::load_prefs;

// ─── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "secure-note", about = "SecureNote — encrypted notepad (native GUI)")]
struct Cli {
    /// Data directory (config, prefs, notes.enc)
    #[arg(long, default_value = "./secure-notes")]
    data: PathBuf,

    /// Open a specific .enc file directly, bypassing --data directory layout
    #[arg(long)]
    file: Option<PathBuf>,
}

// ─── Entry point ──────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    let cli = Cli::parse();

    // Resolve data directory and notes file.
    // --file overrides --data and opens a specific .enc file directly.
    let (data_abs, notes_file) = if let Some(ref file_path) = cli.file {
        let abs = if file_path.exists() {
            file_path.canonicalize().unwrap_or_else(|_| file_path.clone())
        } else {
            std::env::current_dir()
                .map(|d| d.join(file_path))
                .unwrap_or_else(|_| file_path.clone())
        };
        let parent = abs.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        (parent, abs)
    } else {
        if let Err(e) = fs::create_dir_all(&cli.data) {
            fatal_error(&format!("Could not create data directory {:?}: {}", cli.data, e));
        }
        let abs   = cli.data.canonicalize().unwrap_or_else(|_| cli.data.clone());
        let notes = abs.join("notes.enc");
        (abs, notes)
    };

    let data_str = data_abs.display().to_string();

    let config_file = data_abs.join("config.json");
    let prefs_file  = data_abs.join("prefs.json");
    let icon_file   = data_abs.join("icon.png");
    let lock_file   = data_abs.join("app.lock");

    if !acquire_lock(&lock_file) {
        fatal_error("SecureNote is already running.\n\nOnly one instance is allowed at a time.");
    }

    let prefs = load_prefs(&prefs_file);

    let icon_data = fs::read(&icon_file)
        .ok()
        .and_then(|b| eframe::icon_data::from_png_bytes(&b).ok())
        .unwrap_or_else(|| eframe::icon_data::from_png_bytes(ICON_PNG).unwrap_or_default());

    let mut vp = egui::ViewportBuilder::default()
        .with_title("SecureNote")
        .with_app_id("secure-note")  // matches secure-note.desktop on Linux
        .with_min_inner_size([600.0, 400.0])
        .with_icon(icon_data);

    if let (Some(x), Some(y)) = (prefs.win_x, prefs.win_y) {
        vp = vp.with_position([x, y]);
    }
    vp = vp.with_inner_size([
        prefs.win_w.unwrap_or(900.0),
        prefs.win_h.unwrap_or(640.0),
    ]);

    let options = eframe::NativeOptions {
        viewport: vp,
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_config(),
        ..Default::default()
    };

    eframe::run_native(
        "SecureNote",
        options,
        Box::new(|cc| {
            let app = SecureNote::new(notes_file, config_file, prefs_file, lock_file, data_str);
            app::setup_fonts(&cc.egui_ctx);
            app.apply_theme(&cc.egui_ctx);
            Box::new(app)
        }),
    )
    .map_err(|e| {
        eprintln!("SecureNote failed to start: {e}");
        e
    })
}

// ─── wgpu configuration ───────────────────────────────────────────────────────

fn wgpu_config() -> eframe::egui_wgpu::WgpuConfiguration {
    #[cfg(target_os = "windows")]
    {
        eframe::egui_wgpu::WgpuConfiguration {
            supported_backends: wgpu::Backends::DX12,
            device_descriptor: std::sync::Arc::new(|_adapter| {
                wgpu::DeviceDescriptor {
                    label: Some("secure-note"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                }
            }),
            power_preference: wgpu::PowerPreference::None,
            ..Default::default()
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        eframe::egui_wgpu::WgpuConfiguration {
            // Try Vulkan first; fall back to GL (EGL) if Vulkan drivers are
            // unavailable — common on Linux without dedicated GPU drivers.
            supported_backends: wgpu::Backends::PRIMARY | wgpu::Backends::GL,
            ..Default::default()
        }
    }
}

// ─── Fatal error helper ───────────────────────────────────────────────────────

fn fatal_error(msg: &str) -> ! {
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        let title: Vec<u16> = OsStr::new("SecureNote — Error")
            .encode_wide().chain(std::iter::once(0)).collect();
        let text: Vec<u16> = OsStr::new(msg)
            .encode_wide().chain(std::iter::once(0)).collect();

        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(hwnd: *mut std::ffi::c_void,
                           text: *const u16, caption: *const u16,
                           utype: u32) -> i32;
        }
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x10); }
    }

    #[cfg(not(target_os = "windows"))]
    eprintln!("SecureNote error: {}", msg);

    process::exit(1);
}

// ─── Single-instance lock ─────────────────────────────────────────────────────

fn pid_is_running(pid: u32) -> bool {
    if pid == 0 { return false; }

    #[cfg(target_os = "linux")]
    { std::path::Path::new(&format!("/proc/{}", pid)).exists() }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[cfg(target_os = "windows")]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }
        const SYNCHRONIZE: u32 = 0x00100000;
        unsafe {
            let h = OpenProcess(SYNCHRONIZE, 0, pid);
            if h.is_null() { false } else { CloseHandle(h); true }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    { false }
}

fn acquire_lock(lock_path: &PathBuf) -> bool {
    // Attempt atomic exclusive creation first to avoid TOCTOU.
    match std::fs::OpenOptions::new().write(true).create_new(true).open(lock_path) {
        Ok(mut f) => {
            let _ = write!(f, "{}", process::id());
            return true;
        }
        Err(_) => {}
    }
    // File already exists — check whether the recorded PID is still alive.
    if let Ok(contents) = fs::read_to_string(lock_path) {
        if let Ok(pid) = contents.trim().parse::<u32>() {
            if pid != process::id() && pid_is_running(pid) {
                return false;
            }
        }
    }
    // Stale lock — overwrite with our PID.
    fs::write(lock_path, process::id().to_string()).is_ok()
}

// Icon generated by build.rs — 256×256 purple padlock matching the app theme.
// Path exported via cargo:rustc-env because OUT_DIR is only visible in build scripts.
static ICON_PNG: &[u8] = include_bytes!(env!("SECURENOTE_ICON_PNG"));
