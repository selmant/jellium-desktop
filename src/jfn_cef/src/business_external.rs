//! Restricted bridge for a separately hosted web frontend.
//!
//! The external page can request playback of a Jellyfin item. It cannot call
//! mpv directly and never receives the full `web` injection profile. Jellyfin
//! Web remains responsible for resolving and controlling playback.

// JfnCefLayer is an opaque internal handle created and retained by the layer
// registry for the entire business object's lifetime.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use cef::rc::Rc;
use cef::{ImplFrame, ImplTask, Task, ThreadId, WrapTask, post_task, wrap_task};
use parking_lot::Mutex;
use std::os::raw::c_void;
use std::sync::Arc;
use url::Url;

use crate::app::userfree_to_string;
use crate::browsers::{jfn_browsers_create, jfn_browsers_set_active};
use crate::client::{
    Inner, JfnCefLayer, jfn_cef_layer_create, jfn_cef_layer_inner, jfn_cef_layer_set_name,
    jfn_cef_layer_set_visible,
};
use crate::ipc::{BrowserMessage, list_string};

struct ExternalState {
    layer: Arc<Inner>,
    web_layer: Arc<Inner>,
    allowed_origin: String,
    external_visible: bool,
}

static INSTANCE: Mutex<Option<ExternalState>> = Mutex::new(None);

/// Create the external frontend above the normal Jellyfin Web layer.
pub fn jfn_external_init(web_layer: *mut JfnCefLayer, start_url: &str, allowed_origin: &str) {
    if web_layer.is_null() || start_url.is_empty() || allowed_origin.is_empty() {
        return;
    }
    if INSTANCE.lock().is_some() {
        tracing::warn!(target: "ExternalHost", "external frontend already initialized");
        return;
    }

    let kind = c"external";
    let layer = unsafe { jfn_browsers_create(kind.as_ptr()) };
    if layer.is_null() {
        return;
    }

    unsafe { jfn_cef_layer_set_name(layer, c"external".as_ptr()) };
    let inner = unsafe { jfn_cef_layer_inner(layer) };
    let web_inner = unsafe { jfn_cef_layer_inner(web_layer) };
    install_handlers(layer, Arc::clone(&inner));

    *INSTANCE.lock() = Some(ExternalState {
        layer: Arc::clone(&inner),
        web_layer: web_inner,
        allowed_origin: allowed_origin.to_string(),
        external_visible: true,
    });

    unsafe {
        jfn_cef_layer_set_visible(layer, true);
        jfn_cef_layer_create(layer, start_url.as_ptr().cast(), start_url.len());
    }
}

fn install_handlers(layer: *mut JfnCefLayer, inner_for_created: Arc<Inner>) {
    let layer_ref = unsafe { &*layer };
    layer_ref.set_created_callback_rust(Some(Box::new(move |_browser: *mut c_void| {
        let ptr = inner_for_created.layer_ptr();
        if !ptr.is_null() {
            jfn_browsers_set_active(ptr);
        }
    })));
    layer_ref.set_message_handler_rust(Some(Box::new(handle_message)));
    layer_ref.set_before_close_callback_rust(Some(Box::new(|| {
        *INSTANCE.lock() = None;
    })));
}

fn handle_message(message: BrowserMessage) -> bool {
    if message.name() != "playJellyfinItem" {
        return false;
    }
    if !message_origin_allowed(&message) {
        tracing::warn!(target: "ExternalHost", "rejected native call from non-allowlisted origin");
        return true;
    }
    let Some(args) = message.args() else {
        return true;
    };
    let request_id = list_string(args, 0);
    let item_id = list_string(args, 1);
    if !valid_request_id(&request_id) || !valid_item_id(&item_id) {
        tracing::warn!(target: "ExternalHost", "rejected invalid Jellyfin item id");
        return true;
    }
    crate::business_web::jfn_web_play_item(&item_id);
    emit_event("accepted", &request_id);
    true
}

/// Emit only the versioned, non-sensitive command acknowledgement. Results
/// from playback remain native-owned and use the same event envelope later.
fn emit_event(kind: &str, request_id: &str) {
    let instance = INSTANCE.lock();
    let Some(state) = instance.as_ref() else {
        return;
    };
    state.layer.exec_js(&format!(
        "window.dispatchEvent(new CustomEvent('foreseer:native-event',{{detail:{{protocolVersion:1,requestId:'{request_id}',type:'{kind}'}}}}));"
    ));
}

fn message_origin_allowed(message: &BrowserMessage) -> bool {
    let Some(frame) = message.main_frame() else {
        return false;
    };
    let frame_url = userfree_to_string(&frame.url());
    INSTANCE
        .lock()
        .as_ref()
        .is_some_and(|state| url_origin_matches(&frame_url, &state.allowed_origin))
}

fn url_origin_matches(frame_url: &str, allowed_origin: &str) -> bool {
    Url::parse(frame_url).is_ok_and(|url| url.origin().ascii_serialization() == allowed_origin)
}

fn valid_item_id(item_id: &str) -> bool {
    !item_id.is_empty()
        && item_id.len() <= 128
        && item_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn valid_request_id(request_id: &str) -> bool {
    !request_id.is_empty()
        && request_id.len() <= 64
        && request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Show Jellyfin Web while native playback is active and restore the external
/// frontend after playback stops. Called from the Jellyfin browser's CEF UI
/// message handler.
fn apply_external_visible(show_external: bool) {
    let (external_ptr, active) = {
        let mut instance = INSTANCE.lock();
        let Some(state) = instance.as_mut() else {
            return;
        };
        if state.external_visible == show_external {
            return;
        }
        let external_ptr = state.layer.layer_ptr();
        let active = if show_external {
            external_ptr
        } else {
            state.web_layer.layer_ptr()
        };
        if external_ptr.is_null() || active.is_null() {
            tracing::warn!(target: "ExternalHost", "surface switch ignored: browser layer is not ready");
            return;
        }
        state.external_visible = show_external;
        (external_ptr, active)
    };
    tracing::info!(
        target: "ExternalHost",
        show_external,
        "switching visible frontend surface"
    );
    unsafe { jfn_cef_layer_set_visible(external_ptr, show_external) };
    jfn_browsers_set_active(active);
}

/// Reveal the Jellyfin/mpv layer immediately before mpv receives a resolved
/// media URL. This runs on CEF's UI thread from the Jellyfin IPC handler.
pub fn jfn_external_show_player() {
    apply_external_visible(false);
}

wrap_task! {
    struct RestoreExternalTask {}
    impl Task {
        fn execute(&self) {
            apply_external_visible(true);
        }
    }
}

/// Restore the external frontend from a playback-coordinator worker thread.
pub fn jfn_external_restore_async() {
    let mut task = RestoreExternalTask::new();
    let _ = post_task(ThreadId::UI, Some(&mut task));
}

/// Restore the hosted frontend after authoritative native playback terminal
/// events. Called once after the playback coordinator is initialized.
pub fn jfn_external_start_playback_observer() {
    if INSTANCE.lock().is_none() {
        return;
    }
    jfn_playback::register_event_sink(Box::new(|event| {
        if matches!(
            event.kind,
            jfn_playback::PlaybackEventKind::Finished
                | jfn_playback::PlaybackEventKind::Canceled
                | jfn_playback::PlaybackEventKind::Error
        ) {
            jfn_external_restore_async();
        }
    }));
}

/// Use Jellyfin's JS notification only as an early failure fallback. Normal
/// playback completion is driven by authoritative native coordinator events.
pub fn jfn_external_on_playback_state(state: &str) {
    if state == "Stopped" {
        jfn_external_restore_async();
    }
}

#[cfg(test)]
mod tests {
    use super::{url_origin_matches, valid_item_id, valid_request_id};

    #[test]
    fn accepts_normal_jellyfin_ids() {
        assert!(valid_item_id("30c9b8c96f6940efb9fca35e32fb70e5"));
        assert!(valid_item_id("30c9b8c9-6f69-40ef-b9fc-a35e32fb70e5"));
    }

    #[test]
    fn rejects_empty_oversized_and_script_values() {
        assert!(!valid_item_id(""));
        assert!(!valid_item_id(&"a".repeat(129)));
        assert!(!valid_item_id("x');window.bad=true;//"));
    }

    #[test]
    fn validates_request_ids() {
        assert!(valid_request_id("play-123"));
        assert!(!valid_request_id(""));
        assert!(!valid_request_id("request id"));
        assert!(!valid_request_id(&"a".repeat(65)));
    }

    #[test]
    fn origin_match_is_exact() {
        assert!(url_origin_matches(
            "https://media.example/discover?q=one",
            "https://media.example"
        ));
        assert!(!url_origin_matches(
            "https://media.example.evil.test/",
            "https://media.example"
        ));
        assert!(!url_origin_matches(
            "http://media.example/",
            "https://media.example"
        ));
        assert!(!url_origin_matches("not a URL", "https://media.example"));
    }
}
