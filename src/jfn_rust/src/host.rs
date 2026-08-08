//! Narrow configuration API for binaries embedding the Jellium runtime.

#[cfg(feature = "external-frontend")]
use std::fmt;
#[cfg(feature = "external-frontend")]
use std::sync::Arc;
#[cfg(feature = "external-frontend")]
use url::Url;

#[cfg(feature = "external-frontend")]
use jfn_cef::{HostAuthService, HostConfigService};

/// A separately hosted web frontend displayed in Jellium's native window.
#[cfg(feature = "external-frontend")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalFrontend {
    start_url: String,
    allowed_origin: String,
    setup_document: bool,
}

#[cfg(feature = "external-frontend")]
impl ExternalFrontend {
    /// Configure an HTTP(S) frontend and derive its exact native-call origin.
    pub fn new(start_url: impl Into<String>) -> Result<Self, ConfigError> {
        let start_url = start_url.into();
        let parsed = Url::parse(&start_url).map_err(|_| ConfigError::InvalidUrl)?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(ConfigError::UnsupportedScheme);
        }
        if parsed.host_str().is_none() {
            return Err(ConfigError::UnsupportedScheme);
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(ConfigError::CredentialsNotAllowed);
        }
        Ok(Self {
            allowed_origin: parsed.origin().ascii_serialization(),
            start_url,
            setup_document: false,
        })
    }

    /// Configure the built-in, one-shot setup document. This is deliberately
    /// distinct from a normal external frontend: only this profile may expose
    /// configuration calls, and Jellium checks the exact document URL on every
    /// such call.
    pub fn setup_document(start_url: impl Into<String>) -> Result<Self, ConfigError> {
        let start_url = start_url.into();
        let parsed = Url::parse(&start_url).map_err(|_| ConfigError::InvalidUrl)?;
        if parsed.scheme() != "data"
            || !start_url.starts_with("data:text/html;base64,")
            || start_url.len() > 256 * 1024
        {
            return Err(ConfigError::SetupDocumentRequired);
        }
        Ok(Self {
            start_url,
            allowed_origin: "null".to_string(),
            setup_document: true,
        })
    }

    pub(crate) fn start_url(&self) -> &str {
        &self.start_url
    }

    pub(crate) fn allowed_origin(&self) -> &str {
        &self.allowed_origin
    }

    pub(crate) fn is_setup_document(&self) -> bool {
        self.setup_document
    }
}

/// Options supplied by a desktop binary hosting the Jellium runtime.
#[derive(Clone, Default)]
pub struct HostOptions {
    #[cfg(feature = "external-frontend")]
    external_frontend: Option<ExternalFrontend>,
    #[cfg(feature = "external-frontend")]
    auth_service: Option<Arc<dyn HostAuthService>>,
    #[cfg(feature = "external-frontend")]
    config_service: Option<Arc<dyn HostConfigService>>,
}

impl HostOptions {
    #[cfg(feature = "external-frontend")]
    pub fn with_external_frontend(frontend: ExternalFrontend) -> Self {
        Self {
            external_frontend: Some(frontend),
            auth_service: None,
            config_service: None,
        }
    }

    #[cfg(feature = "external-frontend")]
    pub fn with_auth_service(mut self, service: Arc<dyn HostAuthService>) -> Self {
        self.auth_service = Some(service);
        self
    }

    #[cfg(feature = "external-frontend")]
    pub(crate) fn external_frontend(&self) -> Option<&ExternalFrontend> {
        self.external_frontend.as_ref()
    }

    #[cfg(feature = "external-frontend")]
    pub(crate) fn auth_service(&self) -> Option<Arc<dyn HostAuthService>> {
        self.auth_service.clone()
    }

    #[cfg(feature = "external-frontend")]
    pub fn with_config_service(mut self, service: Arc<dyn HostConfigService>) -> Self {
        self.config_service = Some(service);
        self
    }

    #[cfg(feature = "external-frontend")]
    pub(crate) fn config_service(&self) -> Option<Arc<dyn HostConfigService>> {
        self.config_service.clone()
    }
}

#[cfg(feature = "external-frontend")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigError {
    InvalidUrl,
    UnsupportedScheme,
    CredentialsNotAllowed,
    SetupDocumentRequired,
}

#[cfg(feature = "external-frontend")]
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidUrl => "external frontend URL is invalid",
            Self::UnsupportedScheme => "external frontend URL must use HTTP or HTTPS",
            Self::CredentialsNotAllowed => "external frontend URL must not contain credentials",
            Self::SetupDocumentRequired => {
                "setup frontend must be a bounded built-in HTML data document"
            }
        })
    }
}

#[cfg(feature = "external-frontend")]
impl std::error::Error for ConfigError {}

#[cfg(all(test, feature = "external-frontend"))]
mod tests {
    use super::*;

    #[test]
    fn derives_exact_origin_from_start_url() {
        let frontend =
            ExternalFrontend::new("https://media.example:8443/discover?q=one").expect("valid URL");
        assert_eq!(
            frontend.start_url(),
            "https://media.example:8443/discover?q=one"
        );
        assert_eq!(frontend.allowed_origin(), "https://media.example:8443");
    }

    #[test]
    fn rejects_non_web_urls_and_missing_hosts() {
        assert_eq!(
            ExternalFrontend::new("file:///tmp/frontend.html"),
            Err(ConfigError::UnsupportedScheme)
        );
        assert_eq!(
            ExternalFrontend::new("https://"),
            Err(ConfigError::InvalidUrl)
        );
        assert_eq!(
            ExternalFrontend::new("data:text/html;base64,PGgxPkhlbGxvPC9oMT4="),
            Err(ConfigError::UnsupportedScheme)
        );
    }

    #[test]
    fn rejects_url_credentials() {
        assert_eq!(
            ExternalFrontend::new("https://user:secret@media.example"),
            Err(ConfigError::CredentialsNotAllowed)
        );
    }

    #[test]
    fn setup_document_is_explicit_and_bounded() {
        let setup = ExternalFrontend::setup_document("data:text/html;base64,PGgxPkhlbGxvPC9oMT4=")
            .expect("built-in setup document");
        assert!(setup.is_setup_document());
        assert_eq!(setup.allowed_origin(), "null");
        assert_eq!(
            ExternalFrontend::setup_document("data:text/plain,hello"),
            Err(ConfigError::SetupDocumentRequired)
        );
    }
}
