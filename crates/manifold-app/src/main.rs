#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod app;
mod app_lifecycle;
mod app_render;
mod content_command;
mod content_commands;
mod content_export;
mod content_pipeline;
mod content_state;
mod content_thread;
mod dialog_path_memory;
#[cfg(target_os = "macos")]
mod display_link;
mod editing_host;
mod edr_surface;
mod frame_timer;
mod input_handler;
mod input_host;
mod perform_mode;
mod project_io;
#[cfg(target_os = "macos")]
mod shared_texture;
mod text_input;
mod transport_state;
mod ui_bridge;
mod ui_root;
mod user_prefs;
mod window_registry;

fn main() {
    // --- Panic hook (10.1, 10.12) ---
    // Install before anything else so even early panics get logged to disk.
    // Critical with `panic=abort` + stripped symbols in release builds.
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let timestamp = {
            let d = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            d.as_secs()
        };
        let msg =
            format!("MANIFOLD PANIC at unix_ts={timestamp}\n{info}\n\nBacktrace:\n{backtrace}",);
        eprintln!("{msg}");

        // Write crash log to ~/Library/Logs/com.latentspace.manifold/crash.log
        if let Some(home) = std::env::var_os("HOME") {
            let log_dir =
                std::path::PathBuf::from(home).join("Library/Logs/com.latentspace.manifold");
            let _ = std::fs::create_dir_all(&log_dir);
            let log_path = log_dir.join("crash.log");
            let _ = std::fs::write(&log_path, &msg);
        }
    }));

    // --- SIGPIPE handler (10.9) ---
    // Prevent broken-pipe signals from killing the process (e.g. piped output).
    #[cfg(target_os = "macos")]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    // --- Multiple instance protection (10.11) ---
    // Hold a file lock for the lifetime of the process.
    #[cfg(target_os = "macos")]
    let _instance_lock = acquire_instance_lock();

    env_logger::init();
    log::info!("MANIFOLD starting...");

    // --- IOPMAssertion — prevent display sleep (10.2) ---
    #[cfg(target_os = "macos")]
    let _iopm_id = create_iopm_assertion();

    // --- App Nap suppression (10.3) ---
    #[cfg(target_os = "macos")]
    let _activity_token = suppress_app_nap();

    let event_loop = winit::event_loop::EventLoop::new().unwrap();
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    let mut application = app::Application::new();
    event_loop.run_app(&mut application).unwrap();
}

// ---------------------------------------------------------------------------
// Platform-specific helpers (macOS)
// ---------------------------------------------------------------------------

/// Acquire a file lock to prevent multiple instances.
/// Returns the locked `File` handle — the lock is released when dropped.
#[cfg(target_os = "macos")]
fn acquire_instance_lock() -> std::fs::File {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;

    let home = std::env::var("HOME").expect("HOME not set");
    let lock_dir = std::path::PathBuf::from(&home)
        .join("Library/Application Support/com.latentspace.manifold");
    std::fs::create_dir_all(&lock_dir).expect("failed to create lock directory");

    let lock_path = lock_dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("failed to open lock file");

    let fd = file.as_raw_fd();
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        let _ = writeln!(
            std::io::stderr(),
            "MANIFOLD is already running. Only one instance is allowed."
        );
        std::process::exit(1);
    }
    file
}

/// Create an IOPMAssertion to prevent the display from sleeping.
/// Returns the assertion ID (kept alive for the process lifetime).
#[cfg(target_os = "macos")]
fn create_iopm_assertion() -> u32 {
    use core_foundation::string::CFString;

    // kIOPMAssertionLevelOn
    const IOPM_ASSERTION_LEVEL_ON: u32 = 255;

    use core_foundation::base::TCFType;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: *const std::ffi::c_void,
            level: u32,
            name: *const std::ffi::c_void,
            assertion_id: *mut u32,
        ) -> i32;
    }

    let assertion_type = CFString::new("PreventUserIdleDisplaySleep");
    let reason = CFString::new("MANIFOLD live performance");
    let mut assertion_id: u32 = 0;

    let status = unsafe {
        IOPMAssertionCreateWithName(
            assertion_type.as_CFTypeRef().cast(),
            IOPM_ASSERTION_LEVEL_ON,
            reason.as_CFTypeRef().cast(),
            &mut assertion_id,
        )
    };

    if status != 0 {
        log::warn!("IOPMAssertionCreateWithName failed (status={})", status);
    } else {
        log::info!("IOPMAssertion created (id={assertion_id}) — display sleep prevented");
    }

    assertion_id
}

/// Suppress App Nap so macOS doesn't throttle MANIFOLD when backgrounded.
/// Returns the activity token object (must stay alive).
#[cfg(target_os = "macos")]
fn suppress_app_nap() -> objc::rc::StrongPtr {
    use objc::runtime::Object;
    use objc::*;

    // NSActivityUserInitiated | NSActivityLatencyCritical
    // NSActivityUserInitiated          = 0x00000001
    // NSActivityLatencyCritical        = 0xFF00000000
    // Combined keeps the process at full speed and prevents App Nap.
    let options: u64 = 0xFF00000001;

    unsafe {
        let process_info: *mut Object = msg_send![class!(NSProcessInfo), processInfo];
        let reason = {
            let s: *const objc::runtime::Object = msg_send![
                class!(NSString),
                stringWithUTF8String: c"MANIFOLD live performance".as_ptr()
            ];
            s
        };
        let token: *mut Object = msg_send![
            process_info,
            beginActivityWithOptions: options
            reason: reason
        ];
        objc::rc::StrongPtr::retain(token)
    }
}
