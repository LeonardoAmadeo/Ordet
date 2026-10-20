mod app;
mod backup;
mod clipboard;
mod commands;
mod components;
mod editor_buffer;
mod file_browser;
mod history;
mod i18n;
mod latency;
mod prompt;
mod recovery;
mod settings;
mod state;
mod stats;
mod theme;
mod workspace;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    app::run()
}
