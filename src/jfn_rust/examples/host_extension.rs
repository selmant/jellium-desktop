//! Minimal embedding binary proving the generic host-extension seam.
//!
//! No product/protocol names. Scripts talk through `jmpNative.extensionPostMessage`
//! and listen for `jellium:extension-message`.

use std::sync::{Arc, Mutex};

use jfn_rust::{
    ExtensionSource, HostExtension, HostExtensionDescriptor, HostOptions, Presentation,
    RuntimeEvent, RuntimeHandle,
};

struct DemoExtension {
    runtime: Mutex<Option<RuntimeHandle>>,
}

impl HostExtension for DemoExtension {
    fn descriptor(&self) -> HostExtensionDescriptor {
        let url = std::env::var("HOST_EXTENSION_URL")
            .unwrap_or_else(|_| "https://example.com/".to_string());
        let frontend_script = r#"
(function () {
  function post(obj) {
    try {
      window.jmpNative.extensionPostMessage(JSON.stringify(obj));
      return true;
    } catch (e) {
      return false;
    }
  }
  window.addEventListener('jellium:extension-message', function (ev) {
    console.log('host-extension message', ev.detail);
  });
  post({ type: 'demo.hello', id: 'startup' });
})();
"#;
        HostExtensionDescriptor::from_url(url, vec![frontend_script.into()], vec![], false)
            .expect("valid HOST_EXTENSION_URL")
    }

    fn on_runtime_ready(&self, runtime: RuntimeHandle) {
        *self.runtime.lock().expect("runtime lock") = Some(runtime);
    }

    fn admit_message(&self, source: ExtensionSource, origin: &str, payload: &[u8]) -> bool {
        let _ = (source, origin);
        if payload.len() > jfn_rust::MAX_EXTENSION_PAYLOAD_BYTES {
            return false;
        }
        let Ok(text) = std::str::from_utf8(payload) else {
            return false;
        };
        tracing::info!(target: "HostExtensionDemo", "admitted payload bytes={}", text.len());
        if let Some(runtime) = self.runtime.lock().ok().and_then(|g| g.clone()) {
            let _ = runtime.post_message(ExtensionSource::Frontend, payload);
            let _ = runtime.set_presentation(Presentation::Frontend);
        }
        true
    }

    fn on_runtime_event(&self, event: RuntimeEvent) {
        tracing::info!(target: "HostExtensionDemo", "runtime event: {event:?}");
        if matches!(event, RuntimeEvent::ShutdownBeginning) {
            // nothing else to do; stock shutdown proceeds
        }
    }
}

fn main() {
    let extension = Arc::new(DemoExtension {
        runtime: Mutex::new(None),
    });
    let options = HostOptions::with_extension(extension);
    std::process::exit(jfn_rust::app::jfn_app_main_with(options));
}
