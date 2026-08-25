//! CEF process bootstrap + App handlers.

mod app;
pub mod app_menu;
pub mod bridge;
pub mod browsers;
pub mod business_about;
mod business_common;
#[cfg(feature = "host-extension")]
pub mod business_extension;
pub mod business_overlay;
pub mod business_web;
mod cef_string;
pub mod client;
mod client_impl;
mod embedded_js;
#[cfg(feature = "host-extension")]
pub mod extension;
pub mod ffi;
pub mod injection;
mod ipc;
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
mod mallinfo_shim;
mod menu_ownership;
mod paint_scheduler;
pub mod platform_ops;
mod resource;
pub mod sink_routing;
mod state;
mod v8_handler;
pub mod version;
pub mod window_controls;
mod window_sync;

pub use client::{BeforeCloseFn, ContextBuilderFn, ContextDispatcherFn, CreatedFn, JfnCefLayer};
pub use ffi::*;

#[cfg(feature = "host-extension")]
pub use extension::{
    ExtensionConfigError, ExtensionSource, FrontendSource, HostExtension, HostExtensionDescriptor,
    MAX_EXTENSION_PAYLOAD_BYTES, Presentation, RuntimeEvent, RuntimeHandle,
};

pub const APP_VERSION: &str = env!("JFN_APP_VERSION");
pub const APP_VERSION_FULL: &str = env!("JFN_APP_VERSION_FULL");
pub use version::cef_version;
