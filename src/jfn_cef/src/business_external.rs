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
use crate::business_overlay::jfn_overlay_hide;
use crate::client::{
    Inner, JfnCefLayer, jfn_cef_layer_create, jfn_cef_layer_inner, jfn_cef_layer_set_name,
    jfn_cef_layer_set_visible,
};
use crate::ipc::{BrowserMessage, list_string};
use crate::{HostAuthError, HostAuthService, HostConfigService, JellyfinSessionBootstrap};
use cef::ImplListValue;

struct ExternalState {
    layer: Arc<Inner>,
    web_layer: Arc<Inner>,
    allowed_origin: String,
    setup_document_url: Option<String>,
    setup_generation: u64,
    external_visible: bool,
    auth_service: Option<Arc<dyn HostAuthService>>,
    config_service: Option<Arc<dyn HostConfigService>>,
    pending_bootstrap: Option<PendingBootstrap>,
    session_ready: bool,
    auth_epoch: u64,
    active_request_id: Option<String>,
    playback_epoch: u64,
}

struct PendingBootstrap {
    request_id: String,
    bootstrap: JellyfinSessionBootstrap,
}

static INSTANCE: Mutex<Option<ExternalState>> = Mutex::new(None);

const PROTOCOL_VERSION: u8 = 1;
const REQUEST_ID_MAX_LENGTH: usize = 64;
const ITEM_ID_MAX_LENGTH: usize = 128;
const TICKET_LENGTH: usize = 43;
const CHALLENGE_HEX_LENGTH: usize = 64;
const SETUP_MESSAGE_MAX_LENGTH: usize = 256;
const HOST_EVENT_TYPES: &[&str] = &[
    "auth-challenge",
    "ready",
    "accepted",
    "resolving",
    "starting",
    "playing",
    "stopped",
    "finished",
    "canceled",
    "error",
];
const SETUP_EVENT_TYPES: &[&str] = &["connectivity-success", "save-config-success", "error"];

/// Create the external frontend above a private Jellyfin Web control plane.
/// Outside playback the Jellyfin layer stays headless so its login/setup UI
/// never becomes a user-facing surface; during playback it is shown so the
/// Video OSD / media controls can sit above mpv.
pub fn jfn_external_init(
    web_layer: *mut JfnCefLayer,
    start_url: &str,
    allowed_origin: &str,
    setup_document: bool,
    auth_service: Option<Arc<dyn HostAuthService>>,
    config_service: Option<Arc<dyn HostConfigService>>,
) {
    if web_layer.is_null() || start_url.is_empty() || allowed_origin.is_empty() {
        return;
    }
    if INSTANCE.lock().is_some() {
        tracing::warn!(target: "ExternalHost", "external frontend already initialized");
        return;
    }

    let kind = if setup_document {
        c"external-setup"
    } else {
        c"external"
    };
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
        setup_document_url: setup_document.then(|| start_url.to_string()),
        setup_generation: 0,
        external_visible: true,
        auth_service,
        config_service,
        pending_bootstrap: None,
        session_ready: false,
        auth_epoch: 0,
        active_request_id: None,
        playback_epoch: 0,
    });

    unsafe {
        // Keep the private Jellyfin layer headless until playback needs its
        // Video OSD. It still runs JS/network/native playback IPC while hidden.
        jfn_cef_layer_set_visible(web_layer, false);
    }
    // Stock server-selection overlay must never cover Foreseer or mpv.
    jfn_overlay_hide();
    unsafe {
        jfn_cef_layer_set_visible(layer, true);
        jfn_cef_layer_create(layer, start_url.as_ptr().cast(), start_url.len());
    }
}

/// Complete the one controlled transition out of the built-in setup document.
/// The normal origin is established once by the embedding desktop process after
/// it has validated and persisted the selected URL. Hosted JavaScript can never
/// replace it through the bridge.
pub fn jfn_external_complete_setup_navigation(url: &str) {
    let mut state = INSTANCE.lock();
    if let Some(state) = state.as_mut() {
        let Ok(parsed) = Url::parse(url) else {
            return;
        };
        if state.setup_document_url.is_none()
            || !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return;
        }
        state.allowed_origin = parsed.origin().ascii_serialization();
        state.setup_document_url = None;
        state.setup_generation = state.setup_generation.wrapping_add(1);
        state.config_service = None;
        state.layer.load_url(url);
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
        "playJellyfinItem"
            | "requestAuthChallenge"
            | "completeAuth"
            | "clearJellyfinSession"
            | "saveServerUrl"
            | "checkServerConnectivity"
            | "cancelServerConnectivity"
    ) {
        return false;
    }
    if !message_origin_allowed(&message) {
        tracing::warn!(target: "ExternalHost", "rejected native call from non-allowlisted origin");
        return true;
    }
    let setup_generation = if matches!(
        message.name(),
        "saveServerUrl" | "checkServerConnectivity" | "cancelServerConnectivity"
    ) {
        let Some(generation) = setup_call_generation(&message) else {
            tracing::warn!(target: "ExternalHost", "rejected configuration call outside built-in setup document");
            return true;
        };
        Some(generation)
    } else {
        None
    };
    if message.name() == "cancelServerConnectivity" {
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
            emit_event_with_challenge("auth-challenge", &request_id, Some(&challenge));
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
                        tracing::warn!(target: "ExternalHost", error_code = error.code(), "native auth redemption failed ({})", error.code());
                        emit_error(error, &request_id)
                    }
                }),
            );
        } else {
            tracing::warn!(target: "ExternalHost", "auth service unavailable");
        }
        return true;
    }
    if message.name() == "saveServerUrl" {
        let url = list_string(args, 1);
        let allow_insecure = args.int(2) != 0;
        if !valid_request_id(&request_id) {
            return true;
        }
        let Some(setup_generation) = setup_generation else {
            return true;
        };
        let config_service = INSTANCE
            .lock()
            .as_ref()
            .and_then(|s| s.config_service.clone());
        if let Some(service) = config_service {
            match service.save_server_url(&request_id, &url, allow_insecure) {
                Ok(_) => emit_setup_event(
                    setup_generation,
                    "save-config-success",
                    &request_id,
                    None,
                    Some("Configuration saved"),
                ),
                Err(e) => emit_setup_event(setup_generation, "error", &request_id, None, Some(&e)),
            }
        }
        return true;
    }
    if message.name() == "checkServerConnectivity" {
        let url = list_string(args, 1);
        let allow_insecure = args.int(2) != 0;
        if !valid_request_id(&request_id) {
            return true;
        }
        let Some(setup_generation) = setup_generation else {
            return true;
        };
        let config_service = INSTANCE
            .lock()
            .as_ref()
            .and_then(|s| s.config_service.clone());
        if let Some(service) = config_service {
            let request_id_clone = request_id.clone();
            service.check_server_connectivity(
                request_id.clone(),
                url,
                allow_insecure,
                Box::new(move |result| match result {
                    Ok(status) => emit_setup_event(
                        setup_generation,
                        "connectivity-success",
                        &request_id_clone,
                        Some(status),
                        None,
                    ),
                    Err(e) => emit_setup_event(
                        setup_generation,
                        "error",
                        &request_id_clone,
                        None,
                        Some(&e),
                    ),
                }),
            );
        }
        return true;
    }
    let item_id = list_string(args, 1);
    if !valid_request_id(&request_id) || !valid_item_id(&item_id) {
        tracing::warn!(target: "ExternalHost", "rejected invalid Jellyfin item id");
        return true;
    }
    let (admitted, replaced) = {
        let mut instance = INSTANCE.lock();
        let Some(state) = instance.as_mut() else {
            return true;
        };
        if !state.session_ready {
            (false, None)
        } else {
            let replaced = state.active_request_id.replace(request_id.clone());
            (true, replaced)
        }
    };
    if !admitted {
        emit_event("error", &request_id);
        return true;
    }
    if let Some(replaced) = replaced {
        // A prior play was still marked active (common after Video OSD Back when
        // terminal events race). Release the old request; Jellyfin's play()
        // replaces any in-flight item without a hard session reset.
        emit_event("canceled", &replaced);
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
    let Some(detail) = host_event_detail(kind, request_id, None, None) else {
        return;
    };
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
    let Some(detail) = host_event_detail("error", request_id, None, Some(error.code())) else {
        return;
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

fn emit_event_with_challenge(kind: &str, request_id: &str, challenge: Option<&str>) {
    let Some(detail) = host_event_detail(kind, request_id, challenge, None) else {
        return;
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

fn host_event_detail(
    kind: &str,
    request_id: &str,
    challenge: Option<&str>,
    error_code: Option<&str>,
) -> Option<serde_json::Value> {
    let challenge_is_valid = match (kind, challenge) {
        ("auth-challenge", Some(challenge)) => {
            challenge.len() == CHALLENGE_HEX_LENGTH
                && challenge.bytes().all(|byte| byte.is_ascii_hexdigit())
        }
        ("auth-challenge", None) => false,
        (_, None) => true,
        (_, Some(_)) => false,
    };
    if !HOST_EVENT_TYPES.contains(&kind)
        || !valid_request_id(request_id)
        || !challenge_is_valid
        || (error_code.is_some() && kind != "error")
    {
        return None;
    }
    let mut detail = serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "requestId": request_id,
        "type": kind,
    });
    if let Some(challenge) = challenge {
        detail["challenge"] = serde_json::Value::String(challenge.to_string());
    }
    if let Some(error_code) = error_code {
        detail["errorCode"] = serde_json::Value::String(error_code.to_string());
    }
    Some(detail)
}

/// Setup results are intentionally a different typed envelope from auth
/// challenges. `status` carries an HTTP status only for connectivity checks;
/// `message` carries a bounded user-facing result/error string.
fn emit_setup_event(
    setup_generation: u64,
    kind: &str,
    request_id: &str,
    status: Option<u16>,
    message: Option<&str>,
) {
    let Some(detail) = setup_event_detail(kind, request_id, status, message) else {
        return;
    };
    let Some(detail) = serde_json::to_string(&detail).ok() else {
        return;
    };
    let layer = {
        let state = INSTANCE.lock();
        state.as_ref().and_then(|state| {
            setup_event_is_current(
                state.setup_document_url.as_deref(),
                state.setup_generation,
                setup_generation,
            )
            .then(|| Arc::clone(&state.layer))
        })
    };
    if let Some(layer) = layer {
        post_setup_event_js(
            layer,
            setup_generation,
            format!(
                "window.dispatchEvent(new CustomEvent('jellium:host-event',{{detail:{detail}}}));"
            ),
        );
    }
}

fn setup_event_detail(
    kind: &str,
    request_id: &str,
    status: Option<u16>,
    message: Option<&str>,
) -> Option<serde_json::Value> {
    if !SETUP_EVENT_TYPES.contains(&kind) || !valid_request_id(request_id) {
        return None;
    }
    let message = message.map(|message| {
        message
            .chars()
            .take(SETUP_MESSAGE_MAX_LENGTH)
            .collect::<String>()
    });
    Some(serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "requestId": request_id,
        "type": kind,
        "status": status,
        "message": message,
    }))
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

fn setup_call_generation(message: &BrowserMessage) -> Option<u64> {
    let frame = message.main_frame()?;
    let frame_url = userfree_to_string(&frame.url());
    let state = INSTANCE.lock();
    state.as_ref().and_then(|state| {
        setup_document_url_matches(state.setup_document_url.as_deref(), &frame_url)
            .then_some(state.setup_generation)
    })
}

fn setup_document_url_matches(setup_document_url: Option<&str>, frame_url: &str) -> bool {
    setup_document_url.is_some_and(|setup_url| setup_url == frame_url)
}

fn setup_event_is_current(
    setup_document_url: Option<&str>,
    current_generation: u64,
    event_generation: u64,
) -> bool {
    setup_document_url.is_some() && current_generation == event_generation
}

fn url_origin_matches(frame_url: &str, allowed_origin: &str) -> bool {
    Url::parse(frame_url).is_ok_and(|url| url.origin().ascii_serialization() == allowed_origin)
}

fn valid_item_id(item_id: &str) -> bool {
    !item_id.is_empty()
        && item_id.len() <= ITEM_ID_MAX_LENGTH
        && item_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn valid_request_id(request_id: &str) -> bool {
    !request_id.is_empty()
        && request_id.len() <= REQUEST_ID_MAX_LENGTH
        && request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn valid_ticket(ticket: &str) -> bool {
    ticket.len() == TICKET_LENGTH
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
    if server_url.scheme() != "https"
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

wrap_task! {
    struct SetupEventTask {
        layer: Arc<Inner>,
        setup_generation: u64,
        script: String,
    }
    impl Task {
        fn execute(&self) {
            let current = INSTANCE.lock().as_ref().is_some_and(|state| {
                setup_event_is_current(
                    state.setup_document_url.as_deref(),
                    state.setup_generation,
                    self.setup_generation,
                )
            });
            if current {
                self.layer.exec_js(&self.script);
            }
        }
    }
}

/// Async setup callbacks may race the controlled navigation to the hosted
/// frontend. Recheck setup authority on the UI thread before exposing even a
/// sanitized status/message envelope.
fn post_setup_event_js(layer: Arc<Inner>, setup_generation: u64, script: String) {
    let mut task = SetupEventTask::new(layer, setup_generation, script);
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

/// Swap between the hosted external frontend and the private Jellyfin Web
/// player surface. During playback the web layer must be visible so Jellyfin's
/// Video OSD / media controls can sit above mpv; outside playback it stays
/// hidden so setup/login never covers Foreseer or the video surface.
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
    // Stock server-selection overlay must never cover Foreseer or mpv.
    jfn_overlay_hide();
    unsafe {
        jfn_cef_layer_set_visible(external_ptr, show_external);
        jfn_cef_layer_set_visible(web_ptr, !show_external);
    }
    let web_layer = INSTANCE
        .lock()
        .as_ref()
        .map(|state| Arc::clone(&state.web_layer));
    if let Some(web_layer) = web_layer {
        if show_external {
            // Keep any retained login/setup frame from painting over Foreseer
            // between compositor transactions.
            web_layer.exec_js(
                "document.documentElement.style.setProperty('opacity','0','important');document.documentElement.style.setProperty('background','transparent','important');document.body?.style.setProperty('background','transparent','important');",
            );
        } else {
            // Playback owns the surface: restore opacity so Video OSD controls
            // are visible, keep page chrome transparent around mpv.
            web_layer.exec_js(
                "document.documentElement.style.removeProperty('opacity');document.documentElement.style.setProperty('background','transparent','important');document.body?.style.setProperty('background','transparent','important');",
            );
        }
    }
    // Route input to the visible surface. During playback that is Jellyfin's
    // player OSD; otherwise the hosted external frontend.
    jfn_browsers_set_active(if show_external { external_ptr } else { web_ptr });
    if !show_external {
        // Layer show/hide can leave the Wayland VO on a stale media-sized
        // configure; refresh locked host geometry now.
        if let Some(p) = jfn_platform_abi::try_get() {
            p.mpv_host().reassert_window_size();
        }
    }
}

wrap_task! {
    struct ShowPlayerTask {}
    impl Task {
        fn execute(&self) {
            apply_external_visible(false);
        }
    }
}

pub fn jfn_external_notify_load_starting() {
    if let Some(state) = INSTANCE.lock().as_mut() {
        state.playback_epoch = state.playback_epoch.wrapping_add(1);
    }
}

fn show_player_async() {
    jfn_external_notify_load_starting();
    let mut task = ShowPlayerTask::new();
    let _ = post_task(ThreadId::UI, Some(&mut task));
}

wrap_task! {
    struct RestoreExternalTask {
        epoch: u64,
    }
    impl Task {
        fn execute(&self) {
            let current = INSTANCE.lock().as_ref().map(|s| s.playback_epoch).unwrap_or(0);
            if self.epoch == current {
                apply_external_visible(true);
            }
        }
    }
}

/// Restore the external frontend from a playback-coordinator worker thread.
pub fn jfn_external_restore_async() {
    let epoch = INSTANCE
        .lock()
        .as_ref()
        .map(|s| s.playback_epoch)
        .unwrap_or(0);
    let mut task = RestoreExternalTask::new(epoch);
    let _ = cef::post_task(ThreadId::UI, Some(&mut task));
}

/// Restore the hosted frontend after authoritative native playback terminal
/// events. Called once after the playback coordinator is initialized.
pub fn jfn_external_start_playback_observer() {
    if INSTANCE.lock().is_none() {
        return;
    }
    jfn_playback::register_event_sink(Box::new(|event| match event.kind {
        jfn_playback::PlaybackEventKind::Started => {
            emit_playback_event("playing");
            show_player_async();
        }
        jfn_playback::PlaybackEventKind::Finished => {
            end_external_playback("finished");
        }
        jfn_playback::PlaybackEventKind::Canceled => {
            end_external_playback("canceled");
        }
        jfn_playback::PlaybackEventKind::Error => {
            end_external_playback("error");
        }
        _ => {}
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

/// Clear the active play request, restore Foreseer, then notify the page.
/// Restore runs first so the hosted UI is mapped when the host-event arrives.
fn end_external_playback(kind: &str) {
    let request_id = INSTANCE
        .lock()
        .as_mut()
        .and_then(|state| state.active_request_id.take());
    jfn_external_restore_async();
    if let Some(request_id) = request_id {
        // Post after restore so CEF is less likely to drop the event while the
        // external layer is unmapped.
        emit_event(kind, &request_id);
    }
}

/// Use Jellyfin's JS notification only as an early-failure fallback before
/// native playback has started. After the player surface is shown, terminal
/// teardown is owned by the native coordinator so a late `Stopped` cannot
/// cancel a newer play request.
pub fn jfn_external_on_playback_state(state: &str) {
    if state != "Stopped" {
        return;
    }
    let should_end = INSTANCE
        .lock()
        .as_ref()
        .is_some_and(|state| state.active_request_id.is_some() && state.external_visible);
    if should_end {
        end_external_playback("canceled");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHALLENGE_HEX_LENGTH, HOST_EVENT_TYPES, ITEM_ID_MAX_LENGTH, PROTOCOL_VERSION,
        REQUEST_ID_MAX_LENGTH, SETUP_EVENT_TYPES, SETUP_MESSAGE_MAX_LENGTH, TICKET_LENGTH,
        host_event_detail, setup_document_url_matches, setup_event_detail, setup_event_is_current,
        url_origin_matches, valid_item_id, valid_request_id, valid_ticket,
    };
    use serde_json::Value;

    fn protocol_fixture() -> Value {
        serde_json::from_str(include_str!("../../../protocol/protocol-v1.json"))
            .expect("valid protocol fixture")
    }

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

    #[test]
    fn hosted_origin_cannot_use_setup_configuration_calls() {
        let setup_url = "data:text/html;base64,PGgxPlNldHVwPC9oMT4=";
        assert!(setup_document_url_matches(Some(setup_url), setup_url));
        assert!(!setup_document_url_matches(
            Some(setup_url),
            "https://foreseer.example/library"
        ));
        assert!(!setup_document_url_matches(None, setup_url));
    }

    #[test]
    fn delayed_setup_probe_is_discarded_after_setup_navigation() {
        let setup_url = "data:text/html;base64,PGgxPlNldHVwPC9oMT4=";
        let probe_generation = 7;
        assert!(setup_event_is_current(
            Some(setup_url),
            probe_generation,
            probe_generation
        ));
        // Setup navigation clears its exact-document authority and advances the
        // generation before loading the hosted Foreseer origin.
        assert!(!setup_event_is_current(
            None,
            probe_generation + 1,
            probe_generation
        ));
        assert!(!setup_event_is_current(
            Some(setup_url),
            probe_generation + 1,
            probe_generation
        ));
    }

    #[test]
    fn setup_event_envelope_uses_status_and_message_not_challenge() {
        let fixture = protocol_fixture();
        let detail = setup_event_detail("connectivity-success", "setup-1", Some(204), None)
            .expect("valid setup event");
        assert_eq!(detail["status"], 204);
        assert!(detail.get("message").is_some());
        assert!(detail.get("challenge").is_none());
        assert!(detail.get("errorCode").is_none());
        assert!(setup_event_detail("auth-challenge", "setup-1", None, None).is_none());

        let emitted = setup_event_detail(
            "connectivity-success",
            "setup-connectivity",
            Some(204),
            None,
        )
        .expect("canonical setup event");
        assert_eq!(emitted, fixture["examples"]["setupConnectivitySuccess"]);
    }

    #[test]
    fn protocol_v1_fixture_matches_bridge_methods_events_limits_and_versioning() {
        let fixture = protocol_fixture();
        let source = include_str!("../../web/external-host.js");
        let app_source = include_str!("app.rs");
        assert_eq!(
            fixture["fixtureId"],
            "foreseer-native-protocol-v1-2026-08-08"
        );
        assert_eq!(fixture["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(fixture["host"]["name"], "jellium-desktop");
        assert_eq!(
            fixture["limits"]["requestIdMaxLength"],
            REQUEST_ID_MAX_LENGTH
        );
        assert_eq!(fixture["limits"]["itemIdMaxLength"], ITEM_ID_MAX_LENGTH);
        assert_eq!(fixture["limits"]["ticketLength"], TICKET_LENGTH);
        assert_eq!(
            fixture["limits"]["challengeHexLength"],
            CHALLENGE_HEX_LENGTH
        );
        assert_eq!(
            fixture["limits"]["setupMessageMaxLength"],
            SETUP_MESSAGE_MAX_LENGTH
        );
        assert_eq!(
            fixture["hostEventTypes"],
            serde_json::json!(HOST_EVENT_TYPES)
        );
        assert_eq!(
            fixture["setupEventTypes"],
            serde_json::json!(SETUP_EVENT_TYPES)
        );
        assert_eq!(
            fixture["hostMethods"]["playItem"],
            serde_json::json!(["requestId", "itemId"])
        );
        for (method, arguments) in fixture["hostMethods"]
            .as_object()
            .expect("host methods object")
        {
            let parameters = arguments
                .as_array()
                .expect("method arguments")
                .iter()
                .map(|argument| argument.as_str().expect("argument string"))
                .collect::<Vec<_>>()
                .join(", ");
            assert!(
                source.contains(&format!("{method}({parameters})")),
                "missing protocol method {method}({parameters})"
            );
        }
        for (method, arguments) in fixture["setupMethods"]
            .as_object()
            .expect("setup methods object")
        {
            let parameters = arguments
                .as_array()
                .expect("method arguments")
                .iter()
                .map(|argument| argument.as_str().expect("argument string"))
                .collect::<Vec<_>>()
                .join(", ");
            assert!(
                source.contains(&format!("host.{method} = ({parameters})")),
                "missing setup method {method}({parameters})"
            );
        }
        for capability in fixture["host"]["capabilities"]
            .as_array()
            .expect("capabilities array")
        {
            assert!(source.contains(&format!(
                "'{}'",
                capability.as_str().expect("capability string")
            )));
        }
        assert!(source.contains("protocolVersion: 1"));
        assert!(source.contains("hostName: 'jellium-desktop'"));
        assert!(source.contains("hostVersion: '__HOST_VERSION__'"));
        assert!(app_source.contains("crate::APP_VERSION.to_string()"));
        assert!(app_source.contains("__HOST_VERSION__"));
        assert!(!source.contains("startPositionTicks"));
        assert!(source.contains("Jellyfin owns resume policy"));
        assert!(!source.contains("hostVersion: '0.1.0'"));
    }

    #[test]
    fn protocol_v1_event_builders_enforce_closed_types_and_correlation() {
        let event =
            host_event_detail("playing", "play-b", None, None).expect("closed correlated event");
        assert_eq!(event["protocolVersion"], 1);
        assert_eq!(event["requestId"], "play-b");
        assert_eq!(event["type"], "playing");
        assert!(host_event_detail("access-token", "play-b", None, None).is_none());
        assert!(host_event_detail("playing", "request id", None, None).is_none());
        assert!(
            host_event_detail(
                "auth-challenge",
                "auth-1",
                Some(&"c".repeat(CHALLENGE_HEX_LENGTH)),
                None,
            )
            .is_some()
        );
        assert!(host_event_detail("auth-challenge", "auth-1", Some("too-short"), None).is_none());
    }

    #[test]
    fn protocol_v1_limits_are_enforced_at_the_native_boundary() {
        assert!(valid_request_id(&"a".repeat(REQUEST_ID_MAX_LENGTH)));
        assert!(!valid_request_id(&"a".repeat(REQUEST_ID_MAX_LENGTH + 1)));
        assert!(valid_item_id(&"i".repeat(ITEM_ID_MAX_LENGTH)));
        assert!(!valid_item_id(&"i".repeat(ITEM_ID_MAX_LENGTH + 1)));
        assert!(valid_ticket(&"t".repeat(TICKET_LENGTH)));
        assert!(!valid_ticket(&"t".repeat(TICKET_LENGTH - 1)));
        let long_message = "m".repeat(SETUP_MESSAGE_MAX_LENGTH + 10);
        let setup = setup_event_detail("error", "setup-1", None, Some(&long_message))
            .expect("valid bounded setup event");
        assert_eq!(
            setup["message"].as_str().expect("message").chars().count(),
            SETUP_MESSAGE_MAX_LENGTH
        );
    }
}
