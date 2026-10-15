#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod icons;
mod keymap;
mod player;
mod theme;
mod util;
mod view;

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PEEK_LOG").unwrap_or_else(|_| "warn,peek=info".into()),
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
