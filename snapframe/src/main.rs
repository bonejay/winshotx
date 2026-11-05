#![cfg_attr(not(test), windows_subsystem = "windows")]

mod actions;
mod app;
mod archivos;
mod audio_devices;
mod capture;
mod encode;
mod error;
mod overlay;
mod platform;
mod record;
mod recorder;
mod settings;
mod shell;
mod state;
mod ui;

slint::include_modules!();

use std::sync::Arc;

use shell::instance::{self, Request};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Developer aid: render the settings tabs to PNG files off-screen and exit.
    if let Some(i) = args.iter().position(|a| a == "--snapshot-settings") {
        let dir = std::path::PathBuf::from(args.get(i + 1).cloned().unwrap_or_else(|| ".".into()));
        snapshot_settings(&dir);
        return;
    }
    // A second instance hands its request to the running one and exits.
    if !instance::acquire(Request::from_args(&args)) {
        return;
    }

    // Automatic encoding needs to know whether NVENC works; find out before the first overlay.
    #[cfg(windows)]
    std::thread::spawn(record::nvenc::available);
    encode::auto::start_cpu_sampler();
    let mut config = settings::load();
    // Configure a supplied FFmpeg executable once, without changing PATH.
    if let Some(i) = args.iter().position(|a| a == "--ffmpeg-path") {
        if let Some(path) = args.get(i + 1) {
            if encode::ffmpeg::available_at(path) {
                config.encoding.ffmpeg_path = path.clone();
                let _ = settings::save(&config);
            }
        }
    }

    // Refresh the enabled portable-app startup entry after an executable moves.
    // This also repairs entries pointing at a removed build directory.
    if config.start_with_windows {
        if let Err(error) = platform::autostart::set(true) {
            eprintln!("Could not update the startup entry: {error}");
        }
    }

    // The CPU renderer: no GPU context, so no graphics-driver memory is held.
    let backend = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into());
    if let Err(e) = backend.select() {
        eprintln!("could not start the UI: {e}");
        return;
    }

    let state = Arc::new(state::AppState::new(config));
    let app = app::App::install(state);
    instance::listen(|request| app::on_ui(move |a| a.instance_request(request)));
    if args.iter().any(|a| a == "--capture") {
        app.open_capture(state::OverlayIntent::Capture);
    } else if !args
        .iter()
        .any(|a| a == "--background" || a == "--autostart")
    {
        app.show_settings();
    }
    if let Err(e) = slint::run_event_loop_until_quit() {
        eprintln!("the UI stopped: {e}");
    }
}

/// Show the settings window far off-screen, render each tab and save it as PNG.
fn snapshot_settings(dir: &std::path::Path) {
    let _ = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select();
    let config = settings::load();
    let Some(mut ctl) = ui::settings::SettingsCtl::new() else {
        return;
    };
    ctl.load(
        &config,
        state::ShortcutStatus {
            capture: true,
            record: true,
        },
    );
    let w = ctl.window.clone_strong();
    use slint::ComponentHandle;
    w.window()
        .set_position(slint::PhysicalPosition::new(-30000, -30000));
    w.window().set_size(slint::LogicalSize::new(960.0, 1500.0));
    let _ = w.show();
    let dir = dir.to_path_buf();
    let step = std::rc::Rc::new(std::cell::Cell::new(0));
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(400),
        move || {
            let n = step.get();
            if n > 0 {
                if let Ok(buf) = w.window().take_snapshot() {
                    let img = image::RgbaImage::from_raw(
                        buf.width(),
                        buf.height(),
                        buf.as_bytes().to_vec(),
                    );
                    if let Some(img) = img {
                        let _ = img.save(dir.join(format!("settings-tab{}.png", n - 1)));
                    }
                }
            }
            if n == 3 {
                let _ = slint::quit_event_loop();
                return;
            }
            w.set_tab(n);
            step.set(n + 1);
        },
    );
    let _ = slint::run_event_loop_until_quit();
    drop(ctl);
}
