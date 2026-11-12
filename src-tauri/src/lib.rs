pub mod config;
pub mod error;
pub mod logging;
pub mod platform;

pub fn run() {
    if let Err(error) = tauri::Builder::default().run(tauri::generate_context!()) {
        eprintln!("fatal startup error: {error}");
        std::process::exit(1);
    }
}
