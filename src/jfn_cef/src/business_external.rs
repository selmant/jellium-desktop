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
use crate::{HostAuthError, HostAuthService, JellyfinSessionBootstrap};

struct ExternalState {
    layer: Arc<Inner>,
    web_layer: Arc<Inner>,
    allowed_origin: String,
    external_visible: bool,
    auth_service: Option<Arc<dyn HostAuthService>>,
    pending_bootstrap: Option<PendingBootstrap>,
    session_ready: bool,
    auth_epoch: u64,
    active_request_id: Option<String>,
}

struct PendingBootstrap {
    request_id: String,
    bootstrap: JellyfinSessionBootstrap,
}

static INSTANCE: Mutex<Option<ExternalState>> = Mutex::new(None);

/// Create the external frontend above a permanently headless Jellyfin Web
/// control plane. SDK hosts own the user-facing surface; Jellyfin Web is used
/// only to resolve streams and report playback, so exposing its login UI would
/// be both confusing and an unintended fallback.
pub fn jfn_external_init(
    web_layer: *mut JfnCefLayer,
    start_url: &str,
    allowed_origin: &str,
    auth_service: Option<Arc<dyn HostAuthService>>,
) {
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
        auth_service,
        pending_bootstrap: None,
        session_ready: false,
        auth_epoch: 0,
        active_request_id: None,
    });

    unsafe {
        // Keep the private Jellyfin layer headless for the entire external
        // frontend lifetime. It continues to execute JS, network requests and
        // native playback IPC while CEF stops presenting its surface.
        jfn_cef_layer_set_visible(web_layer, false);
    }
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
    if !matches!(
        message.name(),
        "playJellyfinItem" | "requestAuthChallenge" | "completeAuth" | "clearJellyfinSession"
    ) {
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
    if message.name() == "clearJellyfinSession" {
        if !valid_request_id(&request_id) {
            return true;
        }
        let service = {
            let mut instance = INSTANCE.lock();
            let Some(state) = instance.as_mut() else {
                return true;
            };
            state.pending_bootstrap = None;
            state.session_ready = false;
            state.auth_epoch = state.auth_epoch.wrapping_add(1);
            state.active_request_id = None;
            state.auth_service.clone()
        };
        if let Some(service) = service {
            service.clear_session();
        }
        crate::business_web::jfn_web_clear_session();
        emit_event("stopped", &request_id);
        jfn_external_restore_async();
        return true;
    }
    if message.name() == "requestAuthChallenge" {
        if !valid_request_id(&request_id) {
            return true;
        }
        let service = INSTANCE
            .lock()
            .as_ref()
            .and_then(|s| s.auth_service.clone());
        tracing::info!(target: "ExternalHost", "auth challenge requested");
        if let Some(challenge) = service.and_then(|s| s.request_challenge(&request_id)) {
            emit_event_with_payload("auth-challenge", &request_id, &challenge);
        } else {
            tracing::warn!(target: "ExternalHost", "auth challenge unavailable");
            emit_event("error", &request_id);
        }
        return true;
    }
    if message.name() == "completeAuth" {
        let ticket = list_string(args, 1);
        if !valid_request_id(&request_id) || !valid_ticket(&ticket) {
            return true;
        }
        let service_and_epoch = INSTANCE.lock().as_ref().and_then(|state| {
            state
                .auth_service
                .clone()
                .map(|service| (service, state.auth_epoch))
        });
        if let Some((service, auth_epoch)) = service_and_epoch {
            tracing::info!(target: "ExternalHost", "redeeming native auth ticket");
            service.complete_auth(
                request_id.clone(),
                ticket,
                Box::new(move |result| match result {
                    Ok(bootstrap) => {
                        tracing::info!(target: "ExternalHost", "native auth redemption succeeded");
                        install_bootstrap(&request_id, bootstrap, auth_epoch)
                    }
                    Err(error) => {
                        tracing::warn!(target: "ExternalHost", error_code = error.code(), "native auth redemption failed");
                        emit_error(error, &request_id)
                    }
                }),
            );
        } else {
            tracing::warn!(target: "ExternalHost", "auth service unavailable");
        }
        return true;
    }
    let item_id = list_string(args, 1);
    if !valid_request_id(&request_id) || !valid_item_id(&item_id) {
        tracing::warn!(target: "ExternalHost", "rejected invalid Jellyfin item id");
        return true;
    }
    let admitted = {
        let mut instance = INSTANCE.lock();
        let Some(state) = instance.as_mut() else {
            return true;
        };
        if !state.session_ready || state.active_request_id.is_some() {
            false
        } else {
            state.active_request_id = Some(request_id.clone());
            true
        }
    };
    if !admitted {
        emit_event("error", &request_id);
        return true;
    }
    emit_event_on_ui("accepted", &request_id);
    crate::business_web::jfn_web_play_item(&item_id);
    tracing::info!(target: "ExternalHost", "Jellyfin item play request accepted");
    true
}

/// Emit only the versioned, non-sensitive command acknowledgement. Results
/// from playback remain native-owned and use the same event envelope later.
fn emit_event(kind: &str, request_id: &str) {
    emit_event_with_challenge(kind, request_id, None);
}

fn emit_event_on_ui(kind: &str, request_id: &str) {
    let detail = serde_json::json!({
        "protocolVersion": 1,
        "requestId": request_id,
        "type": kind,
    });
    let Some(detail) = serde_json::to_string(&detail).ok() else {
        return;
    };
    let layer = INSTANCE
        .lock()
        .as_ref()
        .map(|state| Arc::clone(&state.layer));
    if let Some(layer) = layer {
        layer.exec_js(&format!(
            "window.dispatchEvent(new CustomEvent('jellium:host-event',{{detail:{detail}}}));"
        ));
    }
}

fn emit_error(error: HostAuthError, request_id: &str) {
    let detail = serde_json::json!({
        "protocolVersion": 1,
        "requestId": request_id,
        "type": "error",
        "errorCode": error.code(),
    });
    let Some(detail) = serde_json::to_string(&detail).ok() else {
        return;
    };
    let layer = INSTANCE
        .lock()
        .as_ref()
        .map(|state| Arc::clone(&state.layer));
    if let Some(layer) = layer {
        post_external_js(
            layer,
            format!(
                "window.dispatchEvent(new CustomEvent('jellium:host-event',{{detail:{detail}}}));"
            ),
        );
    }
}

fn emit_event_with_payload(kind: &str, request_id: &str, payload: &str) {
    emit_event_with_challenge(kind, request_id, Some(payload));
}

fn emit_event_with_challenge(kind: &str, request_id: &str, challenge: Option<&str>) {
    let detail = if let Some(challenge) = challenge {
        serde_json::json!({
            "protocolVersion": 1,
            "requestId": request_id,
            "type": kind,
            "challenge": challenge,
        })
    } else {
        serde_json::json!({
            "protocolVersion": 1,
            "requestId": request_id,
            "type": kind,
        })
    };
    let Some(detail) = serde_json::to_string(&detail).ok() else {
        return;
    };
    let layer = INSTANCE
        .lock()
        .as_ref()
        .map(|state| Arc::clone(&state.layer));
    if let Some(layer) = layer {
        post_external_js(
            layer,
            format!(
                "window.dispatchEvent(new CustomEvent('jellium:host-event',{{detail:{detail}}}));"
            ),
        );
    }
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

fn valid_ticket(ticket: &str) -> bool {
    ticket.len() == 43
        && ticket
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn install_bootstrap(request_id: &str, bootstrap: JellyfinSessionBootstrap, auth_epoch: u64) {
    let mut task = InstallBootstrapTask::new(request_id.to_string(), bootstrap, auth_epoch);
    let _ = post_task(ThreadId::UI, Some(&mut task));
}

fn install_bootstrap_on_ui(request_id: &str, bootstrap: JellyfinSessionBootstrap, auth_epoch: u64) {
    let Ok(server_url) = Url::parse(&bootstrap.server_url) else {
        emit_error(HostAuthError::InvalidBootstrapResponse, request_id);
        return;
    };
    if !matches!(server_url.scheme(), "http" | "https")
        || server_url.host_str().is_none()
        || !server_url.username().is_empty()
        || server_url.password().is_some()
        || server_url.query().is_some()
        || server_url.fragment().is_some()
    {
        emit_error(HostAuthError::InvalidBootstrapResponse, request_id);
        return;
    }
    let (web_layer, target_url) = {
        let mut instance = INSTANCE.lock();
        let Some(state) = instance.as_mut() else {
            return;
        };
        if state.auth_epoch != auth_epoch {
            return;
        }
        state.session_ready = false;
        state.active_request_id = None;
        state.pending_bootstrap = Some(PendingBootstrap {
            request_id: request_id.to_string(),
            bootstrap,
        });
        (Arc::clone(&state.web_layer), server_url.to_string())
    };
    web_layer.load_url(&target_url);
}

pub fn jfn_external_on_web_load(url: &str) {
    let Ok(loaded_url) = Url::parse(url) else {
        return;
    };
    let (web_layer, script) = {
        let instance = INSTANCE.lock();
        let Some(state) = instance.as_ref() else {
            return;
        };
        let Some(pending) = state.pending_bootstrap.as_ref() else {
            return;
        };
        let Ok(expected_url) = Url::parse(&pending.bootstrap.server_url) else {
            return;
        };
        if loaded_url.origin() != expected_url.origin() {
            let request_id = pending.request_id.clone();
            drop(instance);
            jfn_external_fail_pending(&request_id);
            return;
        }
        let Ok(bootstrap) = serde_json::to_string(&serde_json::json!({
            "serverUrl": pending.bootstrap.server_url,
            "serverId": pending.bootstrap.server_id,
            "userId": pending.bootstrap.user_id,
            "deviceId": pending.bootstrap.device_id,
            "accessToken": pending.bootstrap.access_token,
            "generation": pending.bootstrap.bootstrap_generation,
        })) else {
            return;
        };
        (
            Arc::clone(&state.web_layer),
            format!(
                "window.__jelliumSessionBootstrap={bootstrap};window._jelliumApplySessionBootstrap?.();"
            ),
        )
    };
    web_layer.exec_js(&script);
}

wrap_task! {
    struct InstallBootstrapTask {
        request_id: String,
        bootstrap: JellyfinSessionBootstrap,
        auth_epoch: u64,
    }
    impl Task {
        fn execute(&self) {
            install_bootstrap_on_ui(&self.request_id, self.bootstrap.clone(), self.auth_epoch);
        }
    }
}

wrap_task! {
    struct ExternalJsTask {
        layer: Arc<Inner>,
        script: String,
    }
    impl Task {
        fn execute(&self) {
            self.layer.exec_js(&self.script);
        }
    }
}

fn post_external_js(layer: Arc<Inner>, script: String) {
    let mut task = ExternalJsTask::new(layer, script);
    let _ = post_task(ThreadId::UI, Some(&mut task));
}

/// Complete the auth exchange only after the private Jellyfin Web layer has
/// accepted the bootstrap through its live ApiClient.
pub fn jfn_external_on_session_ready(server_id: &str, user_id: &str, generation: &str) {
    let request_id = {
        let mut instance = INSTANCE.lock();
        let Some(state) = instance.as_mut() else {
            return;
        };
        let Some(pending) = state.pending_bootstrap.as_ref() else {
            return;
        };
        if pending.bootstrap.server_id != server_id
            || pending.bootstrap.user_id != user_id
            || pending.bootstrap.bootstrap_generation != generation
        {
            tracing::warn!(target: "ExternalHost", "rejected unmatched Jellyfin session acknowledgement");
            return;
        }
        let request_id = state
            .pending_bootstrap
            .take()
            .map(|pending| pending.request_id);
        if request_id.is_some() {
            state.session_ready = true;
        }
        request_id
    };
    if let Some(request_id) = request_id {
        tracing::info!(target: "ExternalHost", "private Jellyfin session is ready");
        emit_event("ready", &request_id);
    }
}

pub fn jfn_external_on_session_failed(generation: &str) {
    let request_id = {
        let instance = INSTANCE.lock();
        instance.as_ref().and_then(|state| {
            state
                .pending_bootstrap
                .as_ref()
                .filter(|pending| pending.bootstrap.bootstrap_generation == generation)
                .map(|pending| pending.request_id.clone())
        })
    };
    if let Some(request_id) = request_id {
        jfn_external_fail_pending(&request_id);
    }
}

fn jfn_external_fail_pending(request_id: &str) {
    let removed = {
        let mut instance = INSTANCE.lock();
        let Some(state) = instance.as_mut() else {
            return;
        };
        if state
            .pending_bootstrap
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id)
        {
            state.pending_bootstrap = None;
            state.session_ready = false;
            true
        } else {
            false
        }
    };
    if removed {
        emit_error(HostAuthError::InvalidBootstrapResponse, request_id);
    }
}

/// Hide every CEF surface while native playback is active and restore the
/// external frontend after playback stops. The private Jellyfin Web browser
/// resolves and controls the stream, but it must never cover the mpv surface:
/// its login route is opaque and otherwise makes native playback appear to be
/// a web fallback.
fn apply_external_visible(show_external: bool) {
    let (external_ptr, web_ptr) = {
        let mut instance = INSTANCE.lock();
        let Some(state) = instance.as_mut() else {
            return;
        };
        if state.external_visible == show_external {
            return;
        }
        let external_ptr = state.layer.layer_ptr();
        let web_ptr = state.web_layer.layer_ptr();
        if external_ptr.is_null() || web_ptr.is_null() {
            tracing::warn!(target: "ExternalHost", "surface switch ignored: browser layer is not ready");
            return;
        }
        state.external_visible = show_external;
        (external_ptr, web_ptr)
    };
    tracing::info!(
        target: "ExternalHost",
        show_external,
        "switching visible frontend surface"
    );
    unsafe {
        jfn_cef_layer_set_visible(external_ptr, show_external);
        // Jellyfin Web continues to service the native player while hidden.
        // Leaving it mapped here would place its opaque login screen over mpv.
        jfn_cef_layer_set_visible(web_ptr, false);
    }
    if !show_external {
        // Wayland GPU surfaces can retain their last opaque frame until their
        // next compositor transaction. The private Jellyfin document is a
        // control plane only, so make that frame transparent as a second
        // guard; its JS continues to report session and playback state.
        let web_layer = INSTANCE
            .lock()
            .as_ref()
            .map(|state| Arc::clone(&state.web_layer));
        if let Some(web_layer) = web_layer {
            web_layer.exec_js(
                "document.documentElement.style.setProperty('opacity','0','important');document.documentElement.style.setProperty('background','transparent','important');document.body?.style.setProperty('background','transparent','important');",
            );
        }
    }
    // Native playback owns keyboard and pointer input until its terminal
    // event restores the hosted frontend.
    jfn_browsers_set_active(if show_external {
        external_ptr
    } else {
        std::ptr::null_mut()
    });
}

wrap_task! {
    struct ShowPlayerTask {}
    impl Task {
        fn execute(&self) {
            apply_external_visible(false);
        }
    }
}

fn show_player_async() {
    let mut task = ShowPlayerTask::new();
    let _ = post_task(ThreadId::UI, Some(&mut task));
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
        let kind = match event.kind {
            jfn_playback::PlaybackEventKind::Started => Some("playing"),
            jfn_playback::PlaybackEventKind::Finished => Some("finished"),
            jfn_playback::PlaybackEventKind::Canceled => Some("canceled"),
            jfn_playback::PlaybackEventKind::Error => Some("error"),
            _ => None,
        };
        let emitted = kind.is_some_and(emit_playback_event);
        if emitted && matches!(event.kind, jfn_playback::PlaybackEventKind::Started) {
            show_player_async();
        }
        if emitted
            && matches!(
                event.kind,
                jfn_playback::PlaybackEventKind::Finished
                    | jfn_playback::PlaybackEventKind::Canceled
                    | jfn_playback::PlaybackEventKind::Error
            )
        {
            if let Some(state) = INSTANCE.lock().as_mut() {
                state.active_request_id = None;
            }
            jfn_external_restore_async();
        }
    }));
}

fn emit_playback_event(kind: &str) -> bool {
    let request_id = INSTANCE
        .lock()
        .as_ref()
        .and_then(|state| state.active_request_id.clone());
    if let Some(request_id) = request_id {
        emit_event(kind, &request_id);
        true
    } else {
        false
    }
}

/// Use Jellyfin's JS notification only as an early failure fallback. Normal
/// playback completion is driven by authoritative native coordinator events.
pub fn jfn_external_on_playback_state(state: &str) {
    if state == "Stopped" {
        let request_id = INSTANCE
            .lock()
            .as_mut()
            .and_then(|state| state.active_request_id.take());
        if let Some(request_id) = request_id {
            emit_event("canceled", &request_id);
        }
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
