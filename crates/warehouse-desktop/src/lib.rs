//! Desktop client for the warehouse API.
//!
//! The window is egui (immediate mode). HTTP and MQTT run on a background
//! thread so `App::ui` never waits on the network.

mod api;
mod app;
mod audio;
mod config;
mod expiry;
mod mqtt;
mod speech;
mod theme;
mod voice;
mod worker;

pub use app::WarehouseApp;
pub use config::Config;

pub fn run() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([1320.0, 720.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Warehouse",
        options,
        Box::new(move |cc| Ok(Box::new(WarehouseApp::new(cc.egui_ctx.clone(), config)))),
    )
}
