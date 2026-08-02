//! Minimal downstream binary using the bounded external-frontend SDK.

use jfn_rust::{ExternalFrontend, HostOptions};

fn main() {
    let url = std::env::var("EXTERNAL_FRONTEND_URL")
        .unwrap_or_else(|_| "https://media.example.com".to_string());
    let frontend = ExternalFrontend::new(url).expect("valid external frontend URL");
    let options = HostOptions::with_external_frontend(frontend);
    std::process::exit(jfn_rust::app::jfn_app_main_with(options));
}
