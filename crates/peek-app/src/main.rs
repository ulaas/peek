#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod grab;
mod icons;
mod keymap;
mod player;
mod theme;
mod util;
mod view;

fn main() -> eframe::Result {
    // Rendering defaults: DX12 on Windows (no third-party Vulkan overlay
    // layers), and the power-saving GPU, which is plenty for a desktop
    // stream. Both can be overridden with the usual wgpu env vars.
    for (key, value) in [("WGPU_POWER_PREF", "low"), ("WGPU_BACKEND", if cfg!(windows) { "dx12" } else { "" })] {
        if !value.is_empty() && std::env::var_os(key).is_none() {
            // SAFETY: still single-threaded; nothing reads the environment concurrently.
            unsafe { std::env::set_var(key, value) };
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PEEK_LOG")
                .unwrap_or_else(|_| "warn,wgpu_hal=error,peek=info".into()),
        )
        .init();

    // Network and protocol work runs here; the UI thread only draws.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("peek-net")
        .enable_all()
        .build()
        .expect("tokio runtime");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Peek")
            .with_app_id("peek")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };

    let handle = rt.handle().clone();
    // `peek <address>` connects right away; `peek file.peekrec` plays a recording.
    let arg = std::env::args().nth(1);
    eframe::run_native("Peek", options, Box::new(move |cc| Ok(Box::new(app::PeekApp::new(cc, handle, arg)))))
}
