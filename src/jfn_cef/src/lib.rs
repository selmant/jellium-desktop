//! CEF process bootstrap + App handlers.

mod app;
pub mod app_menu;
pub mod bridge;
pub mod browsers;
pub mod business_about;
mod business_common;
#[cfg(feature = "external-frontend")]
pub mod business_external;
pub mod business_overlay;
pub mod business_web;
pub mod client;
mod client_impl;
mod embedded_js;
pub mod ffi;
pub mod injection;
mod ipc;
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

pub const APP_VERSION: &str = env!("JFN_APP_VERSION");
pub const APP_VERSION_FULL: &str = env!("JFN_APP_VERSION_FULL");
pub use version::cef_version;

#[cfg(feature = "external-frontend")]
#[derive(Clone)]
pub struct JellyfinSessionBootstrap {
    pub server_url: String,
    pub server_id: String,
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
    pub bootstrap_generation: String,
}

#[cfg(feature = "external-frontend")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostAuthError {
    InvalidRequest,
    ServerUnreachable,
    SessionExpired,
    TicketExpired,
    TicketUsed,
    NotLinked,
    TokenInvalid,
    UnsupportedMediaServer,
    InvalidBootstrapResponse,
}

#[cfg(feature = "external-frontend")]
impl HostAuthError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::ServerUnreachable => "server_unreachable",
            Self::SessionExpired => "session_expired",
            Self::TicketExpired => "ticket_expired",
            Self::TicketUsed => "ticket_used",
            Self::NotLinked => "not_linked",
            Self::TokenInvalid => "token_invalid",
            Self::UnsupportedMediaServer => "unsupported_media_server",
            Self::InvalidBootstrapResponse => "invalid_bootstrap_response",
        }
    }
}

#[cfg(feature = "external-frontend")]
pub trait HostAuthService: Send + Sync {
    fn request_challenge(&self, request_id: &str) -> Option<String>;
    fn clear_session(&self);
    fn complete_auth(
        &self,
        request_id: String,
        ticket: String,
        callback: Box<dyn FnOnce(Result<JellyfinSessionBootstrap, HostAuthError>) + Send>,
    );
}
