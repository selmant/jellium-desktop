//! Narrow configuration API for binaries embedding the Jellium runtime.

use std::sync::Arc;

/// Options supplied by a desktop binary hosting the Jellium runtime.
#[derive(Clone, Default)]
pub struct HostOptions {
    #[cfg(feature = "host-extension")]
    extension: Option<Arc<dyn jfn_cef::HostExtension>>,
}

impl HostOptions {
    #[cfg(feature = "host-extension")]
    pub fn with_extension(extension: Arc<dyn jfn_cef::HostExtension>) -> Self {
        Self {
            extension: Some(extension),
        }
    }

    #[cfg(feature = "host-extension")]
    pub(crate) fn extension(&self) -> Option<Arc<dyn jfn_cef::HostExtension>> {
        self.extension.clone()
    }

    #[cfg(feature = "host-extension")]
    pub(crate) fn has_extension(&self) -> bool {
        self.extension.is_some()
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
        assert!(!opts.has_extension());
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
        assert!(opts.has_extension());
    }
}
