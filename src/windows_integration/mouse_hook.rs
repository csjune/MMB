use std::ptr;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{GetLastError, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, MSG, MSLLHOOKSTRUCT, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP,
};

static EVENT_SENDER: OnceLock<SyncSender<GlobalMouseEvent>> = OnceLock::new();

#[derive(Clone, Copy)]
pub enum GlobalMouseEvent {
    LeftDown { x: i32, y: i32 },
    LeftUp { x: i32, y: i32 },
}

pub struct GlobalMouseWatcher {
    events: Receiver<GlobalMouseEvent>,
}

impl GlobalMouseWatcher {
    pub fn new() -> Self {
        let (event_sender, event_receiver) = mpsc::sync_channel(64);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            if EVENT_SENDER.set(event_sender).is_err() {
                let _ = ready_sender.send(Err("mouse hook was already initialized".into()));
                return;
            }

            let module = unsafe { GetModuleHandleW(ptr::null()) };
            let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0) };
            if hook.is_null() {
                let error = unsafe { GetLastError() };
                let _ = ready_sender.send(Err(format!(
                    "SetWindowsHookExW failed (win32 error {error})"
                )));
                return;
            }
            let _ = ready_sender.send(Ok(()));

            let mut message = MSG::default();
            while unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {}
            unsafe {
                UnhookWindowsHookEx(hook);
            }
        });

        match ready_receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("failed to install outside-click watcher: {error}"),
            Err(error) => eprintln!("outside-click watcher did not initialize: {error}"),
        }

        Self {
            events: event_receiver,
        }
    }

    pub fn try_recv(&self) -> Result<GlobalMouseEvent, TryRecvError> {
        self.events.try_recv()
    }

    pub fn drain(&self) {
        while self.events.try_recv().is_ok() {}
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && matches!(wparam as u32, WM_LBUTTONDOWN | WM_LBUTTONUP) {
        let data = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        let event = if wparam as u32 == WM_LBUTTONDOWN {
            GlobalMouseEvent::LeftDown {
                x: data.pt.x,
                y: data.pt.y,
            }
        } else {
            GlobalMouseEvent::LeftUp {
                x: data.pt.x,
                y: data.pt.y,
            }
        };
        if let Some(sender) = EVENT_SENDER.get() {
            let _ = sender.try_send(event);
        }
    }

    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}
