use std::cell::Cell;
use std::fmt;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{GetLastError, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetCursorPos, GetMessageW, MSG, MSLLHOOKSTRUCT, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN,
};

use super::tray::point_is_in_notification_area;

const MOUSE_BUTTONS: [i32; 3] = [VK_LBUTTON as i32, VK_RBUTTON as i32, VK_MBUTTON as i32];

static HOOK_STATE: OnceLock<HookState> = OnceLock::new();

/// Receives `(x, y, wheel_delta)` for wheel events over the notification
/// area. It runs on the hook thread, so it must return quickly.
pub type NotificationAreaWheelHandler = Box<dyn Fn(i32, i32, i32) + Send + Sync>;

struct HookState {
    events: SyncSender<GlobalMouseEvent>,
    next_click_id: AtomicU64,
    latest_click_id: Arc<AtomicU64>,
    on_notification_area_wheel: NotificationAreaWheelHandler,
}

#[derive(Clone, Copy)]
pub enum GlobalMouseEvent {
    ButtonDown { click_id: u64, x: i32, y: i32 },
}

pub struct GlobalMouseWatcher {
    source: MouseSource,
}

enum MouseSource {
    Hook {
        events: Receiver<GlobalMouseEvent>,
        latest_click_id: Arc<AtomicU64>,
    },
    Polling(PollingMouseWatcher),
}

#[derive(Debug)]
pub struct MouseWatcherError(String);

impl fmt::Display for MouseWatcherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for MouseWatcherError {}

struct PollingMouseWatcher {
    pressed: Cell<[bool; MOUSE_BUTTONS.len()]>,
    next_click_id: Cell<u64>,
    latest_click_id: Cell<u64>,
}

impl GlobalMouseWatcher {
    pub fn new(
        on_notification_area_wheel: NotificationAreaWheelHandler,
    ) -> Result<Self, MouseWatcherError> {
        let (event_sender, event_receiver) = mpsc::sync_channel(64);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let latest_click_id = Arc::new(AtomicU64::new(0));
        let hook_latest_click_id = Arc::clone(&latest_click_id);
        thread::Builder::new()
            .name("mmb-mouse-hook".into())
            .spawn(move || {
                let module = unsafe { GetModuleHandleW(ptr::null()) };
                let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0) };
                if hook.is_null() {
                    let error = unsafe { GetLastError() };
                    let _ = ready_sender.send(Err(format!(
                        "SetWindowsHookExW failed (win32 error {error})"
                    )));
                    return;
                }

                if HOOK_STATE
                    .set(HookState {
                        events: event_sender,
                        next_click_id: AtomicU64::new(0),
                        latest_click_id: hook_latest_click_id,
                        on_notification_area_wheel,
                    })
                    .is_err()
                {
                    unsafe {
                        UnhookWindowsHookEx(hook);
                    }
                    let _ = ready_sender.send(Err("mouse hook was already initialized".into()));
                    return;
                }
                if ready_sender.send(Ok(())).is_err() {
                    unsafe {
                        UnhookWindowsHookEx(hook);
                    }
                    return;
                }

                let mut message = MSG::default();
                while unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {}
                unsafe {
                    UnhookWindowsHookEx(hook);
                }
            })
            .map_err(|error| MouseWatcherError(format!("failed to start mouse hook: {error}")))?;

        match ready_receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(())) => Ok(Self {
                source: MouseSource::Hook {
                    events: event_receiver,
                    latest_click_id,
                },
            }),
            Ok(Err(error)) => Err(MouseWatcherError(error)),
            Err(error) => Err(MouseWatcherError(format!(
                "mouse hook did not initialize: {error}"
            ))),
        }
    }

    pub fn polling() -> Self {
        Self {
            source: MouseSource::Polling(PollingMouseWatcher::new()),
        }
    }

    pub fn try_recv(&self) -> Result<GlobalMouseEvent, TryRecvError> {
        match &self.source {
            MouseSource::Hook { events, .. } => events.try_recv(),
            MouseSource::Polling(watcher) => watcher.try_event(),
        }
    }

    pub fn drain(&self) {
        match &self.source {
            MouseSource::Hook { events, .. } => while events.try_recv().is_ok() {},
            MouseSource::Polling(watcher) => while watcher.try_event().is_ok() {},
        }
    }

    pub fn latest_click_id(&self) -> u64 {
        match &self.source {
            MouseSource::Hook {
                latest_click_id, ..
            } => latest_click_id.load(Ordering::Relaxed),
            MouseSource::Polling(watcher) => watcher.latest_click_id.get(),
        }
    }
}

impl PollingMouseWatcher {
    fn new() -> Self {
        Self {
            pressed: Cell::new(MOUSE_BUTTONS.map(button_is_pressed)),
            next_click_id: Cell::new(0),
            latest_click_id: Cell::new(0),
        }
    }

    fn try_event(&self) -> Result<GlobalMouseEvent, TryRecvError> {
        let mut previous = self.pressed.get();

        for (index, virtual_key) in MOUSE_BUTTONS.into_iter().enumerate() {
            let state = unsafe { GetAsyncKeyState(virtual_key) };
            let pressed = state < 0;
            let click_observed = button_click_observed(previous[index], state);
            previous[index] = pressed;
            self.pressed.set(previous);

            if click_observed {
                let mut point = POINT::default();
                if unsafe { GetCursorPos(&mut point) } == 0 {
                    continue;
                }

                let click_id = self.next_click_id.get().wrapping_add(1).max(1);
                self.next_click_id.set(click_id);
                self.latest_click_id.set(click_id);
                return Ok(GlobalMouseEvent::ButtonDown {
                    click_id,
                    x: point.x,
                    y: point.y,
                });
            }
        }

        Err(TryRecvError::Empty)
    }
}

fn button_is_pressed(virtual_key: i32) -> bool {
    (unsafe { GetAsyncKeyState(virtual_key) }) < 0
}

fn button_click_observed(was_pressed: bool, state: i16) -> bool {
    let pressed = state < 0;
    let pressed_since_last_sample = state as u16 & 1 != 0;
    (pressed && !was_pressed) || pressed_since_last_sample
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam as u32 == WM_MOUSEWHEEL {
        let data = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        let delta = wheel_delta(data.mouseData);
        if delta != 0
            && let Some(state) = HOOK_STATE.get()
            && point_is_in_notification_area(data.pt.x, data.pt.y)
        {
            (state.on_notification_area_wheel)(data.pt.x, data.pt.y, delta);
        }
    } else if code >= 0
        && matches!(
            wparam as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN
        )
    {
        let data = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        if let Some(state) = HOOK_STATE.get() {
            let click_id = state
                .next_click_id
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1)
                .max(1);
            state.latest_click_id.store(click_id, Ordering::Relaxed);
            let event = GlobalMouseEvent::ButtonDown {
                click_id,
                x: data.pt.x,
                y: data.pt.y,
            };
            let _ = state.events.try_send(event);
        }
    }

    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

/// The wheel delta lives in the high word of `mouseData`; positive values
/// mean the wheel was rotated away from the user.
fn wheel_delta(mouse_data: u32) -> i32 {
    (mouse_data >> 16) as u16 as i16 as i32
}

#[cfg(test)]
mod tests {
    use super::{button_click_observed, wheel_delta};

    #[test]
    fn wheel_delta_reads_the_signed_high_word() {
        assert_eq!(wheel_delta(120 << 16), 120);
        assert_eq!(wheel_delta(((-120i16) as u16 as u32) << 16), -120);
        assert_eq!(wheel_delta(0x0000_ffff), 0);
    }

    #[test]
    fn polling_detects_pressed_and_completed_clicks() {
        assert!(button_click_observed(false, i16::MIN));
        assert!(button_click_observed(false, 1));
        assert!(!button_click_observed(true, i16::MIN));
        assert!(!button_click_observed(false, 0));
    }
}
