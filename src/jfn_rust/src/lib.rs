pub mod app;
mod cli;
pub mod host;
mod instance_id;
pub mod manager;
mod platform_install;
mod window_geometry;

pub use host::HostOptions;
#[cfg(feature = "external-frontend")]
pub use host::{ConfigError, ExternalFrontend};
#[cfg(feature = "external-frontend")]
pub use jfn_cef::{
    HostAuthError, HostAuthService, HostConfigService, JellyfinSessionBootstrap,
    business_external::jfn_external_complete_setup_navigation,
};
