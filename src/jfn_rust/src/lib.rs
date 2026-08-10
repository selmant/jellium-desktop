pub mod app;
mod cli;
pub mod host;
pub mod manager;
mod platform_install;
mod window_geometry;

pub use host::HostOptions;

#[cfg(feature = "host-extension")]
pub use jfn_cef::{
    ExtensionConfigError, ExtensionSource, FrontendSource, HostExtension, HostExtensionDescriptor,
    MAX_EXTENSION_PAYLOAD_BYTES, Presentation, RuntimeEvent, RuntimeHandle,
};
