#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod grab;
mod icons;
mod keymap;
mod logo;
#[cfg(target_os = "macos")]
mod macos;
mod mark;
mod player;
mod theme;
mod update;
mod util;
mod view;
mod widgets;

fn main() -> eframe::Result {
    if std::env::args().nth(1).is_some_and(|a| a == "--version" || a == "-V") {
        println!("{}", logo::version_line());
        return Ok(());
    }
    // A copy of Peek started by an update to run the Windows installer once Peek itself has closed.
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|a| a == update::FINISH_ARG) {
        if let (Some(msi), Some(exe), Some(sha256)) = (args.next(), args.next(), args.next()) {
            update::finish_update(msi.as_ref(), exe.as_ref(), &sha256.to_string_lossy());
        }
        return Ok(());
    }

    // Rendering defaults: DX12 on Windows (no third-party Vulkan overlay
    // layers), and the power-saving GPU, which is plenty for a desktop
    // stream. Both can be overridden with the usual wgpu env vars.
    for (key, value) in [("WGPU_POWER_PREF", "low"), ("WGPU_BACKEND", if cfg!(windows) { "dx12" } else { "" })] {
        if !value.is_empty() && std::env::var_os(key).is_none() {
            // SAFETY: still single-threaded; nothing reads the environment concurrently.
            unsafe { std::env::set_var(key, value) };
        }
    }

    // Log to stderr and to a file (the release build has no console). The
    // previous run's log is kept as peek.old.log.
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_env("PEEK_LOG").unwrap_or_else(|_| {
            "warn,wgpu_hal=error,peek=info,peek_rdp=info,peek_vnc=info,peek_audio=info,peek_record=info".into()
        })
    };
    let log_file = util::log_dir().and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("peek.log");
        let _ = std::fs::rename(&path, dir.join("peek.old.log"));
        std::fs::File::create(path).ok()
    });
    {
        use tracing_subscriber::fmt::writer::MakeWriterExt;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        let console =
            tracing_subscriber::fmt::layer().with_writer(std::io::stderr.with_max_level(tracing::Level::TRACE));
        let file =
            log_file.map(|f| tracing_subscriber::fmt::layer().with_ansi(false).with_writer(std::sync::Mutex::new(f)));
        tracing_subscriber::registry().with(filter()).with(console).with(file).init();
    }
    tracing::info!("{}", logo::version_line());

    // Network and protocol work runs here; the UI thread only draws.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("peek-net")
        .enable_all()
        .build()
        .expect("tokio runtime");

    let viewport = egui::ViewportBuilder::default()
        .with_title("Peek")
        .with_app_id("peek")
        .with_inner_size([1360.0, 860.0])
        .with_min_inner_size([720.0, 480.0])
        .with_icon(logo::icon_data());
    // Peek draws its own title bar (see app::top_bar). On macOS the window keeps its native
    // frame (traffic lights, edge resizing, full screen, Mission Control), with the title bar
    // made transparent and the content drawn underneath it.
    #[cfg(not(target_os = "macos"))]
    let viewport = viewport.with_decorations(false);
    #[cfg(target_os = "macos")]
    let viewport = viewport.with_fullsize_content_view(true).with_title_shown(false).with_titlebar_shown(false);
    let options = eframe::NativeOptions { viewport, ..Default::default() };

    let handle = rt.handle().clone();
    // `peek <address>` connects right away; `peek file.peekrec` plays a recording.
    let arg = std::env::args().nth(1);
    let creator: eframe::AppCreator<'_> = Box::new(move |cc| {
        #[cfg(target_os = "macos")]
        macos::set_context(&cc.egui_ctx);
        Ok(Box::new(app::PeekApp::new(cc, handle, arg)))
    });
    run(options, creator)
}

#[cfg(not(target_os = "macos"))]
fn run(options: eframe::NativeOptions, creator: eframe::AppCreator<'_>) -> eframe::Result {
    eframe::run_native("Peek", options, creator)
}

/// macOS: the event loop is created here, so that files opened from Finder can be received
/// (see macos.rs) before it starts.
#[cfg(target_os = "macos")]
fn run(options: eframe::NativeOptions, creator: eframe::AppCreator<'_>) -> eframe::Result {
    let event_loop = winit::event_loop::EventLoop::<eframe::UserEvent>::with_user_event().build()?;
    macos::install_open_handler();
    let mut app = eframe::create_native("Peek", options, creator, &event_loop);
    event_loop.run_app(&mut app)?;
    Ok(())
}
