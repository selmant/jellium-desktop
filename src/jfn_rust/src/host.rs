//! Narrow configuration API for binaries embedding the Jellium runtime.

#[cfg(feature = "external-frontend")]
use std::fmt;
#[cfg(feature = "external-frontend")]
use url::Url;

/// A separately hosted web frontend displayed in Jellium's native window.
#[cfg(feature = "external-frontend")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalFrontend {
    start_url: String,
    allowed_origin: String,
}

#[cfg(feature = "external-frontend")]
impl ExternalFrontend {
    /// Configure an HTTP(S) frontend and derive its exact native-call origin.
    pub fn new(start_url: impl Into<String>) -> Result<Self, ConfigError> {
        let start_url = start_url.into();
        let parsed = Url::parse(&start_url).map_err(|_| ConfigError::InvalidUrl)?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(ConfigError::UnsupportedScheme);
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(ConfigError::CredentialsNotAllowed);
        }
        Ok(Self {
            allowed_origin: parsed.origin().ascii_serialization(),
            start_url,
        })
    }

    pub(crate) fn start_url(&self) -> &str {
        &self.start_url
    }

    pub(crate) fn allowed_origin(&self) -> &str {
        &self.allowed_origin
    }
}

/// Options supplied by a desktop binary hosting the Jellium runtime.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostOptions {
    #[cfg(feature = "external-frontend")]
    external_frontend: Option<ExternalFrontend>,
}

impl HostOptions {
    #[cfg(feature = "external-frontend")]
    pub fn with_external_frontend(frontend: ExternalFrontend) -> Self {
        Self {
            external_frontend: Some(frontend),
        }
    }

    #[cfg(feature = "external-frontend")]
    pub(crate) fn external_frontend(&self) -> Option<&ExternalFrontend> {
        self.external_frontend.as_ref()
    }
}

#[cfg(feature = "external-frontend")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigError {
    InvalidUrl,
    UnsupportedScheme,
    CredentialsNotAllowed,
}

#[cfg(feature = "external-frontend")]
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidUrl => "external frontend URL is invalid",
            Self::UnsupportedScheme => "external frontend URL must use HTTP or HTTPS",
            Self::CredentialsNotAllowed => "external frontend URL must not contain credentials",
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
    }

    #[test]
    fn rejects_url_credentials() {
        assert_eq!(
            ExternalFrontend::new("https://user:secret@media.example"),
            Err(ConfigError::CredentialsNotAllowed)
        );
    }
}
