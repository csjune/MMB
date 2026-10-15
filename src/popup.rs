//! The brightness popup's window: showing it near the tray, closing it on
//! outside clicks, and releasing it while it is closed so an idle app keeps
//! no window, renderer buffers or UI tree around.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, PlatformError, Timer, TimerMode};

use crate::MainWindow;
use crate::popup_layout::{place_popup, point_is_inside_popup, resize_popup_to_content};
use crate::windows_integration::{self, GlobalMouseEvent, GlobalMouseWatcher, WorkArea};

const OUTSIDE_CLICK_POLL_INTERVAL: Duration = Duration::from_millis(16);

pub(crate) struct PopupController {
    window: RefCell<Option<MainWindow>>,
    work_area: Cell<Option<WorkArea>>,
    position_epoch: Rc<Cell<u64>>,
    mouse_watcher: GlobalMouseWatcher,
    outside_click_timer: Timer,
    last_outside_hide_click_id: Cell<Option<u64>>,
}

impl PopupController {
    pub(crate) fn new(mouse_watcher: GlobalMouseWatcher) -> Rc<Self> {
        Rc::new(Self {
            window: RefCell::new(None),
            work_area: Cell::new(None),
            position_epoch: Rc::new(Cell::new(0)),
            mouse_watcher,
            outside_click_timer: Timer::default(),
            last_outside_hide_click_id: Cell::new(None),
        })
    }

    /// Runs `action` with the window, if one exists.
    pub(crate) fn with(&self, action: impl FnOnce(&MainWindow)) {
        if let Some(window) = self.window.borrow().as_ref() {
            action(window);
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.window
            .borrow()
            .as_ref()
            .is_some_and(|window| window.window().is_visible())
    }

    /// Call when the tray icon is clicked. Returns false when the same click
    /// already closed the popup as a click outside it, so the click must not
    /// reopen it.
    pub(crate) fn should_toggle_on_tray_click(self: &Rc<Self>) -> bool {
        self.poll_outside_click();
        !should_suppress_tray_toggle(
            self.last_outside_hide_click_id.replace(None),
            self.mouse_watcher.latest_click_id(),
        )
    }

    /// Shows the popup near the cursor, sized for `monitor_count` rows,
    /// creating its window with `create` if there is none.
    pub(crate) fn open(
        self: &Rc<Self>,
        monitor_count: usize,
        create: impl FnOnce() -> Result<MainWindow, PlatformError>,
    ) -> Result<(), PlatformError> {
        if self.window.borrow().is_none() {
            let window = create()?;
            self.window.replace(Some(window));
        }

        self.work_area
            .set(windows_integration::work_area_near_cursor());
        {
            let window_ref = self.window.borrow();
            let window = window_ref.as_ref().expect("popup window was just created");
            let popup_height = resize_popup_to_content(window, monitor_count, self.work_area.get());
            window.show()?;
            self.place(window, popup_height);
        }
        self.start_outside_click_watcher();
        Ok(())
    }

    pub(crate) fn hide(self: &Rc<Self>) {
        self.with(|window| {
            window.hide().ok();
        });
        self.closed();
    }

    /// Call once the window has been hidden, whoever hid it.
    pub(crate) fn closed(self: &Rc<Self>) {
        self.stop_outside_click_watcher();
        // Abandon pending position corrections.
        self.next_position_epoch();

        // Callers may still be borrowing the window, and Slint may still be
        // inside one of its callbacks, so release it on the next turn of the
        // event loop instead of now.
        let popup = Rc::downgrade(self);
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(popup) = popup.upgrade() {
                popup.release_if_hidden();
            }
        });
    }

    /// Resizes the window for `monitor_count` rows and keeps an open popup
    /// anchored to the corner of the work area.
    pub(crate) fn relayout(&self, monitor_count: usize) {
        self.with(|window| {
            let popup_height = resize_popup_to_content(window, monitor_count, self.work_area.get());
            if window.window().is_visible() {
                self.place(window, popup_height);
            }
        });
    }

    pub(crate) fn poll_outside_click(self: &Rc<Self>) {
        let mut hidden_by = None;
        {
            let window_ref = self.window.borrow();
            let Some(window) = window_ref
                .as_ref()
                .filter(|window| window.window().is_visible())
            else {
                drop(window_ref);
                self.stop_outside_click_watcher();
                return;
            };

            while let Ok(event) = self.mouse_watcher.try_recv() {
                match event {
                    GlobalMouseEvent::ButtonDown { click_id, x, y } => {
                        if !point_is_inside_popup(window, x, y) {
                            hidden_by = Some(click_id);
                            break;
                        }
                    }
                }
            }
        }

        if let Some(click_id) = hidden_by {
            self.hide();
            self.last_outside_hide_click_id.set(Some(click_id));
        }
    }

    fn release_if_hidden(&self) {
        let hidden = self
            .window
            .borrow()
            .as_ref()
            .is_some_and(|window| !window.window().is_visible());
        if hidden {
            self.window.replace(None);
        }
    }

    fn place(&self, window: &MainWindow, popup_height: f32) {
        let position_epoch = self.next_position_epoch();
        place_popup(
            window,
            popup_height,
            self.work_area.get(),
            &self.position_epoch,
            position_epoch,
        );
    }

    fn next_position_epoch(&self) -> u64 {
        let next = self.position_epoch.get().wrapping_add(1).max(1);
        self.position_epoch.set(next);
        next
    }

    fn start_outside_click_watcher(self: &Rc<Self>) {
        self.last_outside_hide_click_id.set(None);
        self.mouse_watcher.set_watching_clicks(true);
        if !self.mouse_watcher.needs_polling() {
            return;
        }

        let popup = Rc::downgrade(self);
        self.outside_click_timer.start(
            TimerMode::Repeated,
            OUTSIDE_CLICK_POLL_INTERVAL,
            move || {
                if let Some(popup) = popup.upgrade() {
                    popup.poll_outside_click();
                }
            },
        );
    }

    fn stop_outside_click_watcher(&self) {
        self.outside_click_timer.stop();
        self.mouse_watcher.set_watching_clicks(false);
    }
}

fn should_suppress_tray_toggle(hidden_click_id: Option<u64>, latest_click_id: u64) -> bool {
    hidden_click_id.is_some_and(|click_id| click_id == latest_click_id)
}

#[cfg(test)]
mod tests {
    use super::should_suppress_tray_toggle;

    #[test]
    fn tray_toggle_is_only_suppressed_for_the_click_that_hid_the_popup() {
        assert!(should_suppress_tray_toggle(Some(12), 12));
        assert!(!should_suppress_tray_toggle(Some(12), 13));
        assert!(!should_suppress_tray_toggle(None, 12));
    }
}
