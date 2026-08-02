use cef::*;
use std::os::raw::c_int;
use std::sync::Arc;

use crate::client::Inner;
use crate::client_impl::os_ffi::OsKeyEvent;
use jfn_platform_abi::event_flags::{EVENTFLAG_ALT_DOWN, EVENTFLAG_CONTROL_DOWN};

// Windows VK codes carried by CefKeyEvent.windows_key_code.
const VK_OEM_PLUS: i32 = 0xBB;
const VK_OEM_MINUS: i32 = 0xBD;
const VK_ADD: i32 = 0x6B;
const VK_SUBTRACT: i32 = 0x6D;
const VK_0: i32 = 0x30;
const VK_NUMPAD0: i32 = 0x60;

fn action_modifier() -> u32 {
    jfn_platform_abi::try_get()
        .map(|p| p.display().action_modifier_flag())
        .unwrap_or(EVENTFLAG_CONTROL_DOWN)
}

fn has_action_modifier(modifiers: u32) -> bool {
    (modifiers & action_modifier()) != 0 && (modifiers & EVENTFLAG_ALT_DOWN) == 0
}

fn is_paste_shortcut(e: &KeyEvent) -> bool {
    let kt: sys::cef_key_event_type_t = e.type_.into();
    if kt != sys::cef_key_event_type_t::KEYEVENT_RAWKEYDOWN {
        return false;
    }
    if !has_action_modifier(e.modifiers) {
        return false;
    }
    e.windows_key_code == b'V' as i32
}

/// Ctrl/Cmd + / - / 0 (and keypad) → CEF page zoom. Matches Chromium desktop.
fn zoom_shortcut(e: &KeyEvent) -> Option<ZoomCommand> {
    let kt: sys::cef_key_event_type_t = e.type_.into();
    if kt != sys::cef_key_event_type_t::KEYEVENT_RAWKEYDOWN {
        return None;
    }
    if !has_action_modifier(e.modifiers) {
        return None;
    }
    match e.windows_key_code {
        VK_OEM_PLUS | VK_ADD => Some(ZoomCommand::IN),
        VK_OEM_MINUS | VK_SUBTRACT => Some(ZoomCommand::OUT),
        VK_0 | VK_NUMPAD0 => Some(ZoomCommand::RESET),
        _ => None,
    }
}

wrap_keyboard_handler! {
    pub struct JfnKeyboardHandlerBuilder {
        inner: Arc<Inner>,
    }

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            browser: Option<&mut Browser>,
            event: Option<&KeyEvent>,
            _os_event: OsKeyEvent<'_>,
            _is_keyboard_shortcut: Option<&mut c_int>,
        ) -> c_int {
            let Some(e) = event else { return 0 };
            if is_paste_shortcut(e) {
                return if self.inner.try_paste() { 1 } else { 0 };
            }
            if let Some(cmd) = zoom_shortcut(e) {
                let Some(b) = browser else {
                    return 0;
                };
                let Some(host) = b.host() else {
                    return 0;
                };
                if host.can_zoom(cmd) != 0 {
                    host.zoom(cmd);
                }
                // Consume so the page does not also handle the key.
                return 1;
            }
            0
        }
    }
}
