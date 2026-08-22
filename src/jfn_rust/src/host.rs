//! Narrow configuration API for binaries embedding the Jellium runtime.

#[cfg(feature = "host-extension")]
use std::sync::Arc;

/// Options supplied by a desktop binary hosting the Jellium runtime.
#[derive(Clone, Default)]
pub struct HostOptions {
    cef_disk_cache_limit_bytes: Option<u64>,
    #[cfg(feature = "host-extension")]
    extension: Option<Arc<dyn jfn_cef::HostExtension>>,
}

impl HostOptions {
    /// Limit Chromium's HTTP disk cache. The caller owns the policy; Jellium
    /// only translates this generic byte budget into Chromium configuration.
    pub fn with_cef_disk_cache_limit(mut self, bytes: u64) -> Self {
        self.cef_disk_cache_limit_bytes = Some(bytes);
        self
    }

    pub(crate) fn cef_disk_cache_limit(&self) -> Option<u64> {
        self.cef_disk_cache_limit_bytes
    }
    #[cfg(feature = "host-extension")]
    pub fn with_extension(extension: Arc<dyn jfn_cef::HostExtension>) -> Self {
        Self {
            cef_disk_cache_limit_bytes: None,
            extension: Some(extension),
        }
    }

    #[cfg(feature = "host-extension")]
    pub(crate) fn extension(&self) -> Option<Arc<dyn jfn_cef::HostExtension>> {
        self.extension.clone()
    }
}

#[cfg(all(test, feature = "host-extension"))]
mod tests {
    use super::*;
    use jfn_cef::{
        ExtensionSource, HostExtension, HostExtensionDescriptor, RuntimeEvent, RuntimeHandle,
    };
    use std::sync::Mutex;

    struct NoopExt {
        desc: HostExtensionDescriptor,
        ready: Mutex<bool>,
    }

    impl HostExtension for NoopExt {
        fn descriptor(&self) -> HostExtensionDescriptor {
            self.desc.clone()
        }
        fn on_runtime_ready(&self, _runtime: RuntimeHandle) {
            *self.ready.lock().unwrap() = true;
        }
        fn admit_message(&self, _source: ExtensionSource, _origin: &str, _payload: &[u8]) -> bool {
            false
        }
        fn on_runtime_event(&self, _event: RuntimeEvent) {}
    }

    #[test]
    fn default_host_options_have_no_extension() {
        let opts = HostOptions::default();
        assert!(opts.extension().is_none());
    }

    #[test]
    fn with_extension_stores_arc() {
        let desc = HostExtensionDescriptor::from_url(
            "https://app.example/",
            vec!["/* frontend */".into()],
            vec![],
            false,
        )
        .unwrap();
        let ext = Arc::new(NoopExt {
            desc,
            ready: Mutex::new(false),
        });
        let opts = HostOptions::with_extension(ext);
        assert!(opts.extension().is_some());
    }
}
