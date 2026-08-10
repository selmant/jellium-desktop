//! Generic host-extension runtime seam.
//!
//! Owns dual CEF layers, exact-origin messaging, presentation switching, and
//! playback lifecycle callbacks. Product protocol stays in the embedder.

#![cfg(feature = "host-extension")]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use cef::rc::Rc;
use cef::{ImplFrame, ImplTask, Task, ThreadId, WrapTask, post_task, wrap_task};
use parking_lot::Mutex;
use std::os::raw::c_void;
use std::sync::Arc;
use url::Url;

use crate::browsers::{jfn_browsers_create, jfn_browsers_set_active};
use crate::client::{
    Inner, JfnCefLayer, jfn_cef_layer_create, jfn_cef_layer_inner, jfn_cef_layer_set_name,
    jfn_cef_layer_set_visible,
};
use crate::extension::{
    ExtensionSource, HostExtension, MAX_EXTENSION_PAYLOAD_BYTES, Presentation, RuntimeEvent,
    RuntimeHandle,
};
use crate::ipc::{BrowserMessage, list_string};

struct ExtensionState {
    extension: Arc<dyn HostExtension>,
    frontend: Arc<Inner>,
    primary_web: Arc<Inner>,
    allowed_origin: String,
    primary_web_allowed_origin: Option<String>,
    setup_document_url: Option<String>,
    setup_generation: u64,
    frontend_visible: bool,
    playback_epoch: u64,
}

static INSTANCE: Mutex<Option<ExtensionState>> = Mutex::new(None);
static PENDING_EXTENSION: Mutex<Option<Arc<dyn HostExtension>>> = Mutex::new(None);
static HOST_SCRIPTS: Mutex<HostScripts> = Mutex::new(HostScripts {
    frontend: Vec::new(),
    primary_web: Vec::new(),
});

#[derive(Clone, Default)]
struct HostScripts {
    frontend: Vec<String>,
    primary_web: Vec<String>,
}

pub fn install(extension: Arc<dyn HostExtension>) {
    let desc = extension.descriptor();
    *HOST_SCRIPTS.lock() = HostScripts {
        frontend: desc.frontend_scripts.clone(),
        primary_web: desc.primary_web_scripts.clone(),
    };
    *PENDING_EXTENSION.lock() = Some(extension);
}

pub fn pending_extension() -> Option<Arc<dyn HostExtension>> {
    PENDING_EXTENSION.lock().clone()
}

pub fn take_pending_extension() -> Option<Arc<dyn HostExtension>> {
    PENDING_EXTENSION.lock().take()
}

pub fn host_frontend_scripts() -> Vec<String> {
    HOST_SCRIPTS.lock().frontend.clone()
}

pub fn host_primary_web_scripts() -> Vec<String> {
    HOST_SCRIPTS.lock().primary_web.clone()
}

pub fn extension_configured() -> bool {
    INSTANCE.lock().is_some() || PENDING_EXTENSION.lock().is_some()
}

pub fn server_overlay_enabled() -> bool {
    pending_extension()
        .map(|ext| ext.descriptor().server_overlay_enabled)
        .or_else(|| {
            INSTANCE
                .lock()
                .as_ref()
                .map(|state| state.extension.descriptor().server_overlay_enabled)
        })
        .unwrap_or(true)
}

/// Create the hosted frontend above a private primary-web control plane.
pub fn jfn_extension_init(web_layer: *mut JfnCefLayer) {
    let Some(extension) = take_pending_extension() else {
        return;
    };
    if web_layer.is_null() {
        return;
    }
    if INSTANCE.lock().is_some() {
        tracing::warn!(target: "HostExtension", "host extension already initialized");
        return;
    }

    let desc = extension.descriptor();
    let start_url = desc.start_url().to_string();
    let setup_document = desc.is_setup_document();
    let allowed_origin = desc.allowed_origin.clone();

    let kind = if setup_document {
        c"host-frontend-setup"
    } else {
        c"host-frontend"
    };
    let layer = unsafe { jfn_browsers_create(kind.as_ptr()) };
    if layer.is_null() {
        return;
    }

    unsafe { jfn_cef_layer_set_name(layer, c"host-frontend".as_ptr()) };
    let frontend = unsafe { jfn_cef_layer_inner(layer) };
    let primary_web = unsafe { jfn_cef_layer_inner(web_layer) };
    install_handlers(layer, ExtensionSource::Frontend, Arc::clone(&frontend));

    *INSTANCE.lock() = Some(ExtensionState {
        extension: Arc::clone(&extension),
        frontend: Arc::clone(&frontend),
        primary_web,
        allowed_origin,
        primary_web_allowed_origin: None,
        setup_document_url: setup_document.then_some(start_url.clone()),
        setup_generation: 0,
        frontend_visible: true,
        playback_epoch: 0,
    });

    unsafe {
        jfn_cef_layer_set_visible(web_layer, false);
    }
    if !desc.server_overlay_enabled {
        crate::business_overlay::jfn_overlay_hide();
    }
    unsafe {
        jfn_cef_layer_set_visible(layer, true);
        jfn_cef_layer_create(layer, start_url.as_ptr().cast(), start_url.len());
    }

    extension.on_runtime_ready(RuntimeHandle::new());
    emit_event(RuntimeEvent::FrontendCreated);
}

fn install_handlers(layer: *mut JfnCefLayer, source: ExtensionSource, inner_for_created: Arc<Inner>) {
    let layer_ref = unsafe { &*layer };
    layer_ref.set_created_callback_rust(Some(Box::new(move |_browser: *mut c_void| {
        let ptr = inner_for_created.layer_ptr();
        if !ptr.is_null() {
            jfn_browsers_set_active(ptr);
        }
        if source == ExtensionSource::Frontend {
            emit_event(RuntimeEvent::FrontendCreated);
        }
    })));
    layer_ref.set_message_handler_rust(Some(Box::new(move |message| {
        handle_extension_message(source, message)
    })));
    layer_ref.set_before_close_callback_rust(Some(Box::new(|| {
        emit_event(RuntimeEvent::FrontendClosed);
        *INSTANCE.lock() = None;
    })));
}

fn handle_extension_message(source: ExtensionSource, message: BrowserMessage) -> bool {
    match message.name() {
        "extensionPostMessage" => handle_post_message(source, message),
        "appExit" if source == ExtensionSource::Frontend => {
            if message_origin_allowed(source, &message) {
                runtime_request_shutdown();
            }
            true
        }
        "toggleFullscreen" if source == ExtensionSource::Frontend => {
            if message_origin_allowed(source, &message) {
                runtime_toggle_fullscreen();
            }
            true
        }
        _ => false,
    }
}

/// Entry used by the primary-web business handler for structured posts.
pub(crate) fn handle_primary_web_extension_message(message: BrowserMessage) -> bool {
    handle_extension_message(ExtensionSource::PrimaryWeb, message)
}

fn handle_post_message(source: ExtensionSource, message: BrowserMessage) -> bool {
    if !message_origin_allowed(source, &message) {
        tracing::warn!(target: "HostExtension", "rejected extension message from non-allowlisted origin");
        return true;
    }

    let Some(args) = message.args() else {
        return true;
    };
    let payload = list_string(args, 0);
    if payload.len() > MAX_EXTENSION_PAYLOAD_BYTES {
        tracing::warn!(target: "HostExtension", "rejected oversized extension payload");
        return true;
    }
    let bytes = payload.as_bytes();
    let origin = frame_origin(&message).unwrap_or_default();
    let extension = INSTANCE.lock().as_ref().map(|s| Arc::clone(&s.extension));
    if let Some(extension) = extension {
        let _ = extension.admit_message(source, &origin, bytes);
    }
    true
}

fn frame_origin(message: &BrowserMessage) -> Option<String> {
    let frame = message.main_frame()?;
    let url = crate::cef_string::userfree_to_string(&frame.url());
    let parsed = Url::parse(&url).ok()?;
    Some(parsed.origin().ascii_serialization())
}

fn frame_url(message: &BrowserMessage) -> Option<String> {
    let frame = message.main_frame()?;
    Some(crate::cef_string::userfree_to_string(&frame.url()))
}

fn message_origin_allowed(source: ExtensionSource, message: &BrowserMessage) -> bool {
    // Main-frame only: BrowserMessage::main_frame already selects the main frame.
    let Some(_frame) = message.main_frame() else {
        return false;
    };

    let state = INSTANCE.lock();
    let Some(state) = state.as_ref() else {
        return false;
    };
    match source {
        ExtensionSource::Frontend => {
            if let Some(setup_url) = state.setup_document_url.as_ref() {
                return frame_url(message).as_deref() == Some(setup_url.as_str());
            }
            frame_origin(message).as_deref() == Some(state.allowed_origin.as_str())
        }
        ExtensionSource::PrimaryWeb => {
            let Some(expected) = state.primary_web_allowed_origin.as_deref() else {
                return false;
            };
            frame_origin(message).as_deref() == Some(expected)
        }
    }
}

fn emit_event(event: RuntimeEvent) {
    let extension = INSTANCE.lock().as_ref().map(|s| Arc::clone(&s.extension));
    if let Some(extension) = extension {
        extension.on_runtime_event(event);
    }
}

pub fn jfn_extension_on_frontend_load(url: &str) {
    emit_event(RuntimeEvent::FrontendLoaded {
        url: url.to_string(),
    });
}

pub fn jfn_extension_on_web_load(url: &str) {
    emit_event(RuntimeEvent::PrimaryWebLoaded {
        url: url.to_string(),
    });
}

pub fn jfn_extension_on_web_navigate(url: &str) {
    emit_event(RuntimeEvent::PrimaryWebNavigated {
        url: url.to_string(),
    });
}

pub fn jfn_extension_notify_load_starting() {
    if let Some(state) = INSTANCE.lock().as_mut() {
        state.playback_epoch = state.playback_epoch.wrapping_add(1);
    }
}

pub fn jfn_extension_start_playback_observer() {
    if INSTANCE.lock().is_none() {
        return;
    }
    jfn_playback::register_event_sink(Box::new(|event| match event.kind {
        jfn_playback::PlaybackEventKind::Started => {
            emit_event(RuntimeEvent::PlaybackStarted);
            show_primary_web_async();
        }
        jfn_playback::PlaybackEventKind::Finished => {
            end_playback(RuntimeEvent::PlaybackFinished);
        }
        jfn_playback::PlaybackEventKind::Canceled => {
            end_playback(RuntimeEvent::PlaybackCanceled);
        }
        jfn_playback::PlaybackEventKind::Error => {
            end_playback(RuntimeEvent::PlaybackError);
        }
        _ => {}
    }));
}

fn end_playback(event: RuntimeEvent) {
    // Restore frontend first, then deliver the terminal event.
    jfn_extension_restore_async();
    emit_event(event);
}

fn apply_presentation(show_frontend: bool) {
    let (frontend_ptr, web_ptr) = {
        let mut instance = INSTANCE.lock();
        let Some(state) = instance.as_mut() else {
            return;
        };
        if state.frontend_visible == show_frontend {
            return;
        }
        let frontend_ptr = state.frontend.layer_ptr();
        let web_ptr = state.primary_web.layer_ptr();
        if frontend_ptr.is_null() || web_ptr.is_null() {
            tracing::warn!(target: "HostExtension", "presentation switch ignored: layer not ready");
            return;
        }
        state.frontend_visible = show_frontend;
        (frontend_ptr, web_ptr)
    };

    if !server_overlay_enabled() {
        crate::business_overlay::jfn_overlay_hide();
    }
    unsafe {
        jfn_cef_layer_set_visible(frontend_ptr, show_frontend);
        jfn_cef_layer_set_visible(web_ptr, !show_frontend);
    }
    let web_layer = INSTANCE
        .lock()
        .as_ref()
        .map(|state| Arc::clone(&state.primary_web));
    if let Some(web_layer) = web_layer {
        if show_frontend {
            web_layer.exec_js(
                "document.documentElement.style.setProperty('opacity','0','important');document.documentElement.style.setProperty('background','transparent','important');document.body?.style.setProperty('background','transparent','important');",
            );
        } else {
            web_layer.exec_js(
                "document.documentElement.style.removeProperty('opacity');document.documentElement.style.setProperty('background','transparent','important');document.body?.style.setProperty('background','transparent','important');",
            );
        }
    }
    jfn_browsers_set_active(if show_frontend { frontend_ptr } else { web_ptr });
}

wrap_task! {
    struct ShowPrimaryWebTask {}
    impl Task {
        fn execute(&self) {
            apply_presentation(false);
        }
    }
}

fn show_primary_web_async() {
    jfn_extension_notify_load_starting();
    let mut task = ShowPrimaryWebTask::new();
    let _ = post_task(ThreadId::UI, Some(&mut task));
}

wrap_task! {
    struct RestoreFrontendTask {
        epoch: u64,
    }
    impl Task {
        fn execute(&self) {
            let current = INSTANCE.lock().as_ref().map(|s| s.playback_epoch).unwrap_or(0);
            if self.epoch == current {
                apply_presentation(true);
            }
        }
    }
}

pub fn jfn_extension_restore_async() {
    let epoch = INSTANCE
        .lock()
        .as_ref()
        .map(|s| s.playback_epoch)
        .unwrap_or(0);
    let mut task = RestoreFrontendTask::new(epoch);
    let _ = cef::post_task(ThreadId::UI, Some(&mut task));
}

wrap_task! {
    struct ExtensionJsTask {
        layer: Arc<Inner>,
        script: String,
    }
    impl Task {
        fn execute(&self) {
            self.layer.exec_js(&self.script);
        }
    }
}

fn post_extension_js(layer: Arc<Inner>, script: String) {
    let mut task = ExtensionJsTask::new(layer, script);
    let _ = post_task(ThreadId::UI, Some(&mut task));
}

pub fn runtime_post_message(target: ExtensionSource, payload: &[u8]) -> bool {
    if payload.len() > MAX_EXTENSION_PAYLOAD_BYTES {
        return false;
    }
    let Ok(text) = std::str::from_utf8(payload) else {
        return false;
    };
    let layer = {
        let state = INSTANCE.lock();
        let Some(state) = state.as_ref() else {
            return false;
        };
        match target {
            ExtensionSource::Frontend => Arc::clone(&state.frontend),
            ExtensionSource::PrimaryWeb => Arc::clone(&state.primary_web),
        }
    };
    // Deliver as a JSON string detail; embedder scripts build branded APIs on top.
    let detail = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    let script = format!(
        "window.dispatchEvent(new CustomEvent('jellium:extension-message',{{detail:{detail}}}));"
    );
    post_extension_js(layer, script);
    true
}

pub fn runtime_navigate_primary_web(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return false;
    }
    let origin = parsed.origin().ascii_serialization();
    let layer = {
        let mut state = INSTANCE.lock();
        let Some(state) = state.as_mut() else {
            return false;
        };
        state.primary_web_allowed_origin = Some(origin);
        Arc::clone(&state.primary_web)
    };
    emit_event(RuntimeEvent::PrimaryWebNavigated {
        url: url.to_string(),
    });
    layer.load_url(url);
    true
}

pub fn runtime_complete_setup_navigation(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let layer = {
        let mut state = INSTANCE.lock();
        let Some(state) = state.as_mut() else {
            return false;
        };
        if state.setup_document_url.is_none()
            || !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return false;
        }
        state.allowed_origin = parsed.origin().ascii_serialization();
        state.setup_document_url = None;
        state.setup_generation = state.setup_generation.wrapping_add(1);
        Arc::clone(&state.frontend)
    };
    layer.load_url(url);
    true
}

pub fn runtime_set_presentation(presentation: Presentation) -> bool {
    match presentation {
        Presentation::Frontend => {
            apply_presentation(true);
            true
        }
        Presentation::PrimaryWeb => {
            apply_presentation(false);
            true
        }
    }
}

pub fn runtime_minimize() {
    if let Some(p) = jfn_platform_abi::try_get() {
        p.window_minimize();
    }
}

pub fn runtime_toggle_maximize() {
    if let Some(p) = jfn_platform_abi::try_get() {
        p.window_toggle_maximize();
    }
}

pub fn runtime_toggle_fullscreen() {
    if let Some(p) = jfn_platform_abi::try_get() {
        p.toggle_fullscreen();
    }
}

pub fn runtime_request_shutdown() {
    emit_event(RuntimeEvent::ShutdownBeginning);
    jfn_playback::jfn_shutdown_initiate();
}

pub fn jfn_extension_begin_shutdown() {
    emit_event(RuntimeEvent::ShutdownBeginning);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::{
        ExtensionConfigError, HostExtensionDescriptor, MAX_EXTENSION_PAYLOAD_BYTES,
    };

    #[test]
    fn payload_limit_is_16kib() {
        assert_eq!(MAX_EXTENSION_PAYLOAD_BYTES, 16 * 1024);
    }

    #[test]
    fn setup_authority_clears_after_navigation_helper_rejects_without_setup() {
        // Without an initialized INSTANCE, complete_setup_navigation is a no-op.
        assert!(!runtime_complete_setup_navigation("https://media.example/"));
    }

    #[test]
    fn descriptor_rejects_evil_setup_shapes() {
        assert_eq!(
            HostExtensionDescriptor::from_setup_document(
                "https://media.example/",
                vec![],
                vec![],
                false
            )
            .unwrap_err(),
            ExtensionConfigError::SetupDocumentRequired
        );
    }
}
