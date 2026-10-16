// SPDX-License-Identifier: MIT
#![windows_subsystem = "windows"]
//! Native Windows controls keep the portable application small, accessible, and
//! independent of WebView, graphics drivers, and third-party GUI runtimes.
mod ui;
fn main() {
    std::panic::set_hook(Box::new(|info| {
        textmerge::logging::error(&format!("Unhandled panic: {info}"));
    }));
    if let Err(error) = ui::run() {
        textmerge::logging::error(&format!("Startup failed: {error:#}"));
        ui::show_startup_error(&format!("TextMerge could not start.\n\n{error:#}"));
    }
}
