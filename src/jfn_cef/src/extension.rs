//! Opaque host-extension API for embedding binaries.
//!
//! Product protocol, auth, and script content live in the embedder. Jellium
//! only provides CEF layers, structured messaging, presentation, and playback
//! lifecycle events.

#![cfg(feature = "host-extension")]

use std::fmt;
use std::sync::Arc;

use url::Url;

/// Fixed inbound/outbound payload ceiling for extension messages.
pub const MAX_EXTENSION_PAYLOAD_BYTES: usize = 16 * 1024;

/// Maximum size for a trusted setup HTML data URL.
pub const MAX_SETUP_DOCUMENT_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrontendSource {
    /// Exact HTTP(S) URL for the hosted frontend.
    Url(String),
    /// Bounded trusted setup document (`data:text/html;base64,...`).
    SetupDocument(String),
}

#[derive(Clone, Debug)]
pub struct HostExtensionDescriptor {
    pub frontend: FrontendSource,
    /// Exact allowed frontend origin (`ascii_serialization`), or `"null"` for setup.
    pub allowed_origin: String,
    /// Trusted scripts injected into the frontend layer.
    pub frontend_scripts: Vec<String>,
    /// Trusted scripts injected into the private primary-web layer.
    pub primary_web_scripts: Vec<String>,
    /// When false, the stock server-selection overlay is skipped.
    pub server_overlay_enabled: bool,
}

impl HostExtensionDescriptor {
    pub fn from_url(
        start_url: impl Into<String>,
        frontend_scripts: Vec<String>,
        primary_web_scripts: Vec<String>,
        server_overlay_enabled: bool,
    ) -> Result<Self, ExtensionConfigError> {
        let start_url = start_url.into();
        let parsed = Url::parse(&start_url).map_err(|_| ExtensionConfigError::InvalidUrl)?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(ExtensionConfigError::UnsupportedScheme);
        }
        if parsed.host_str().is_none() {
            return Err(ExtensionConfigError::UnsupportedScheme);
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(ExtensionConfigError::CredentialsNotAllowed);
        }
        Ok(Self {
            allowed_origin: parsed.origin().ascii_serialization(),
            frontend: FrontendSource::Url(start_url),
            frontend_scripts,
            primary_web_scripts,
            server_overlay_enabled,
        })
    }

    pub fn from_setup_document(
        start_url: impl Into<String>,
        frontend_scripts: Vec<String>,
        primary_web_scripts: Vec<String>,
        server_overlay_enabled: bool,
    ) -> Result<Self, ExtensionConfigError> {
        let start_url = start_url.into();
        let parsed = Url::parse(&start_url).map_err(|_| ExtensionConfigError::InvalidUrl)?;
        if parsed.scheme() != "data"
            || !start_url.starts_with("data:text/html;base64,")
            || start_url.len() > MAX_SETUP_DOCUMENT_BYTES
        {
            return Err(ExtensionConfigError::SetupDocumentRequired);
        }
        Ok(Self {
            frontend: FrontendSource::SetupDocument(start_url),
            allowed_origin: "null".to_string(),
            frontend_scripts,
            primary_web_scripts,
            server_overlay_enabled,
        })
    }

    pub fn is_setup_document(&self) -> bool {
        matches!(self.frontend, FrontendSource::SetupDocument(_))
    }

    pub fn start_url(&self) -> &str {
        match &self.frontend {
            FrontendSource::Url(url) | FrontendSource::SetupDocument(url) => url,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionConfigError {
    InvalidUrl,
    UnsupportedScheme,
    CredentialsNotAllowed,
    SetupDocumentRequired,
}

impl fmt::Display for ExtensionConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidUrl => "host-extension frontend URL is invalid",
            Self::UnsupportedScheme => "host-extension frontend URL must use HTTP or HTTPS",
            Self::CredentialsNotAllowed => {
                "host-extension frontend URL must not contain credentials"
            }
            Self::SetupDocumentRequired => {
                "setup frontend must be a bounded built-in HTML data document"
            }
        })
    }
}

impl std::error::Error for ExtensionConfigError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionSource {
    Frontend,
    PrimaryWeb,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Presentation {
    Frontend,
    /// Primary CEF mapped and not WasHidden (CSS/timers run) but page chrome
    /// stays veiled until the player shell mounts or [`PrimaryWeb`] is set.
    PrimaryWebPreparing,
    PrimaryWeb,
}

#[derive(Clone, Debug)]
pub enum RuntimeEvent {
    FrontendCreated,
    FrontendLoaded { url: String },
    FrontendClosed,
    PrimaryWebNavigated { url: String },
    PrimaryWebLoaded { url: String },
    PlaybackStarted,
    PlaybackFinished,
    PlaybackCanceled,
    PlaybackError,
    ShutdownBeginning,
}

/// Embedder-implemented extension. All callbacks must return quickly; never
/// block the CEF UI thread on network or long work.
pub trait HostExtension: Send + Sync {
    fn descriptor(&self) -> HostExtensionDescriptor;
    fn on_runtime_ready(&self, runtime: RuntimeHandle);
    /// Return true when the payload is accepted for asynchronous handling.
    fn admit_message(&self, source: ExtensionSource, origin: &str, payload: &[u8]) -> bool;
    fn on_runtime_event(&self, event: RuntimeEvent);
}

/// Cloneable opaque handle for embedder-driven runtime operations.
#[derive(Clone, Default)]
pub struct RuntimeHandle {
    _private: (),
}

impl RuntimeHandle {
    pub(crate) fn new() -> Self {
        Self { _private: () }
    }

    /// Deliver structured bytes to a renderer through CEF process messaging.
    pub fn post_message(&self, target: ExtensionSource, payload: &[u8]) -> bool {
        crate::business_extension::runtime_post_message(target, payload)
    }

    /// Navigate the primary web layer and atomically replace its allowed origin.
    pub fn navigate_primary_web(&self, url: &str) -> bool {
        crate::business_extension::runtime_navigate_primary_web(url)
    }

    /// One-way transition from the setup document to a hosted HTTP(S) URL.
    pub fn complete_setup_navigation(&self, url: &str) -> bool {
        crate::business_extension::runtime_complete_setup_navigation(url)
    }

    pub fn set_presentation(&self, presentation: Presentation) -> bool {
        crate::business_extension::runtime_set_presentation(presentation)
    }

    pub fn minimize(&self) {
        crate::business_extension::runtime_minimize();
    }

    pub fn toggle_maximize(&self) {
        crate::business_extension::runtime_toggle_maximize();
    }

    pub fn toggle_fullscreen(&self) {
        crate::business_extension::runtime_toggle_fullscreen();
    }

    pub fn request_shutdown(&self) {
        crate::business_extension::runtime_request_shutdown();
    }

    /// Clear only Chromium's HTTP cache, preserving authentication/profile
    /// state. The operation is owned by Jellium's CEF request context.
    pub fn clear_http_cache(&self) -> bool {
        crate::ffi::jfn_cef_clear_http_cache()
    }
}

/// Shared extension install used by the Rust app host.
pub fn install_extension(extension: Arc<dyn HostExtension>) {
    crate::business_extension::install(extension);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_exact_origin_from_start_url() {
        let desc = HostExtensionDescriptor::from_url(
            "https://media.example:8443/discover?q=one",
            vec![],
            vec![],
            false,
        )
        .expect("valid URL");
        assert_eq!(
            desc.start_url(),
            "https://media.example:8443/discover?q=one"
        );
        assert_eq!(desc.allowed_origin, "https://media.example:8443");
        assert!(!desc.is_setup_document());
    }

    #[test]
    fn rejects_non_web_urls_credentials_and_evil_shapes() {
        assert_eq!(
            HostExtensionDescriptor::from_url("file:///tmp/x.html", vec![], vec![], false)
                .unwrap_err(),
            ExtensionConfigError::UnsupportedScheme
        );
        assert_eq!(
            HostExtensionDescriptor::from_url(
                "https://user:secret@media.example",
                vec![],
                vec![],
                false
            )
            .unwrap_err(),
            ExtensionConfigError::CredentialsNotAllowed
        );
        assert_eq!(
            HostExtensionDescriptor::from_url(
                "data:text/html;base64,PGgxPkhlbGxvPC9oMT4=",
                vec![],
                vec![],
                false
            )
            .unwrap_err(),
            ExtensionConfigError::UnsupportedScheme
        );
    }

    #[test]
    fn setup_document_is_explicit_and_bounded() {
        let setup = HostExtensionDescriptor::from_setup_document(
            "data:text/html;base64,PGgxPkhlbGxvPC9oMT4=",
            vec![],
            vec![],
            false,
        )
        .expect("setup");
        assert!(setup.is_setup_document());
        assert_eq!(setup.allowed_origin, "null");
        assert_eq!(
            HostExtensionDescriptor::from_setup_document(
                "data:text/plain,hello",
                vec![],
                vec![],
                false
            )
            .unwrap_err(),
            ExtensionConfigError::SetupDocumentRequired
        );
    }

    #[test]
    fn rejects_evil_subdomain_as_different_origin() {
        let desc =
            HostExtensionDescriptor::from_url("https://media.example.com/", vec![], vec![], false)
                .unwrap();
        assert_ne!(desc.allowed_origin, "https://evil.media.example.com");
        assert_ne!(desc.allowed_origin, "https://media.example.com.evil.com");
    }
}
