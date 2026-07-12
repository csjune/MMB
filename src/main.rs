#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod monitor_hardware;
mod monitor_state;
mod monitor_worker;
mod windows_integration;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

#[cfg(windows)]
use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
use slint::{
    CloseRequestResponse, ComponentHandle, Image, LogicalSize, ModelRc, PhysicalPosition,
    SharedString, Timer, TimerMode,
};

use monitor_hardware::{ApplyReport, RefreshResult};
use monitor_state::{MonitorState, brightness_after_scroll};
use monitor_worker::{MonitorEvent, MonitorWorker};

slint::include_modules!();

const POPUP_MARGIN: i32 = 12;
const POPUP_POSITION_CORRECTION_DELAYS_MS: [u64; 3] = [0, 50, 200];
const APP_ICON_ICO: &[u8] = include_bytes!("../assets/app.ico");
const TRAY_ICON_LIGHT_ICO: &[u8] = include_bytes!("../assets/tray-light.ico");
const TRAY_ICON_DARK_ICO: &[u8] = include_bytes!("../assets/tray-dark.ico");

fn main() -> Result<(), slint::PlatformError> {
    let backend = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into());
    #[cfg(windows)]
    let backend = backend.with_winit_window_attributes_hook(|attributes| {
        attributes
            .with_skip_taskbar(true)
            .with_undecorated_shadow(true)
    });
    backend.select()?;

    let app = AppController::new()?;
    app.show_tray()?;
    slint::run_event_loop()
}

struct AppController {
    popup: RefCell<Option<MainWindow>>,
    monitor_state: RefCell<MonitorState>,
    monitor_worker: MonitorWorker,
    apply_timer: Timer,
    monitor_event_timer: Timer,
    outside_click_timer: Timer,
    next_request_id: Cell<u64>,
    latest_refresh_id: Cell<u64>,
    pending_worker_requests: Cell<usize>,
    dark_mode: Cell<bool>,
    tray: TrayIcon,
    app_icon: Image,
    tray_light_icon: Image,
    tray_dark_icon: Image,
    left_button_was_down: Cell<bool>,
    click_started_inside_popup: Cell<bool>,
}

impl AppController {
    fn new() -> Result<Rc<Self>, slint::PlatformError> {
        let app_icon = build_icon(APP_ICON_ICO);
        let tray_light_icon = build_icon(TRAY_ICON_LIGHT_ICO);
        let tray_dark_icon = build_icon(TRAY_ICON_DARK_ICO);
        let initial_dark_mode = windows_integration::windows_main_dark_mode();
        let tray = TrayIcon::new()?;
        tray.set_app_icon(tray_icon_for_dark_mode(
            initial_dark_mode,
            &tray_light_icon,
            &tray_dark_icon,
        ));

        let app = Rc::new(Self {
            popup: RefCell::new(None),
            monitor_state: RefCell::new(MonitorState::new()),
            monitor_worker: MonitorWorker::new(),
            apply_timer: Timer::default(),
            monitor_event_timer: Timer::default(),
            outside_click_timer: Timer::default(),
            next_request_id: Cell::new(1),
            latest_refresh_id: Cell::new(0),
            pending_worker_requests: Cell::new(0),
            dark_mode: Cell::new(initial_dark_mode),
            tray,
            app_icon,
            tray_light_icon,
            tray_dark_icon,
            left_button_was_down: Cell::new(windows_integration::left_mouse_button_down()),
            click_started_inside_popup: Cell::new(false),
        });
        app.install_handlers();
        Ok(app)
    }

    fn install_handlers(self: &Rc<Self>) {
        let app = Rc::downgrade(self);
        self.tray.on_toggle_window(move || {
            let Some(app) = app.upgrade() else {
                return;
            };

            if let Err(error) = app.toggle_popup() {
                eprintln!("failed to toggle popup: {error}");
            }
            app.left_button_was_down
                .set(windows_integration::left_mouse_button_down());
        });

        self.tray.on_quit_requested(|| {
            slint::quit_event_loop().ok();
        });
    }

    fn show_tray(&self) -> Result<(), slint::PlatformError> {
        self.tray.show()
    }

    fn create_popup(self: &Rc<Self>) -> Result<MainWindow, slint::PlatformError> {
        let popup = MainWindow::new()?;
        let state = self.monitor_state.borrow();
        popup.set_app_icon(self.app_icon.clone());
        popup.set_monitors(ModelRc::new(state.model()));
        popup.set_has_monitors(state.has_monitors());
        popup.set_dark_mode(self.dark_mode.get());
        popup.set_refreshing(false);
        popup.set_status_message(SharedString::default());
        drop(state);
        let app = Rc::downgrade(self);
        popup.window().on_close_requested(move || {
            if let Some(app) = app.upgrade() {
                app.stop_outside_click_watcher();
            }
            CloseRequestResponse::HideWindow
        });

        let app = Rc::downgrade(self);
        popup.on_brightness_changed(move |monitor_id, value| {
            if let Some(app) = app.upgrade() {
                app.update_brightness(monitor_id, value.round() as i32);
            }
        });

        let app = Rc::downgrade(self);
        popup.on_brightness_scrolled(move |monitor_id, delta| {
            if let Some(app) = app.upgrade() {
                app.scroll_brightness(monitor_id, delta);
            }
        });

        let app = Rc::downgrade(self);
        popup.on_refresh_requested(move || {
            if let Some(app) = app.upgrade() {
                app.request_refresh();
            }
        });

        let app = Rc::downgrade(self);
        popup.on_theme_toggle_requested(move || {
            if let Some(app) = app.upgrade() {
                app.toggle_windows_theme();
            }
        });

        Ok(popup)
    }

    fn toggle_popup(self: &Rc<Self>) -> Result<(), slint::PlatformError> {
        if self.popup.borrow().is_none() {
            self.popup.replace(Some(self.create_popup()?));
        }

        let popup_ref = self.popup.borrow();
        let popup = popup_ref.as_ref().expect("popup was just created");
        if popup.window().is_visible() {
            self.hide_popup(popup);
            return Ok(());
        }

        let current_dark_mode = windows_integration::windows_main_dark_mode();
        self.dark_mode.set(current_dark_mode);
        popup.set_dark_mode(current_dark_mode);
        self.update_tray_icon(current_dark_mode);

        let popup_height = self.resize_popup_from_state(popup);
        popup.show()?;
        position_popup(popup, popup_height);
        stabilize_popup_position(popup, popup_height);
        self.start_outside_click_watcher();
        self.request_refresh();
        Ok(())
    }

    fn update_brightness(self: &Rc<Self>, monitor_id: SharedString, value: i32) {
        let sync_all = self
            .popup
            .borrow()
            .as_ref()
            .is_some_and(MainWindow::get_sync_all);
        let has_monitors = {
            let mut state = self.monitor_state.borrow_mut();
            state.update_brightness(monitor_id.as_str(), value, sync_all);
            state.has_monitors()
        };
        self.with_popup(|popup| popup.set_has_monitors(has_monitors));
        self.schedule_apply();
    }

    fn scroll_brightness(self: &Rc<Self>, monitor_id: SharedString, delta: i32) {
        let current = self
            .monitor_state
            .borrow()
            .brightness_for_monitor(monitor_id.as_str())
            .unwrap_or(50);
        self.update_brightness(monitor_id, brightness_after_scroll(current, delta));
    }

    fn schedule_apply(self: &Rc<Self>) {
        let app: Weak<Self> = Rc::downgrade(self);
        self.apply_timer
            .start(TimerMode::SingleShot, Duration::from_secs(1), move || {
                if let Some(app) = app.upgrade() {
                    let updates = app.monitor_state.borrow_mut().take_pending();
                    if !updates.is_empty() {
                        app.request_apply(updates);
                    }
                }
            });
    }

    fn resize_popup_from_state(&self, popup: &MainWindow) -> f32 {
        let state = self.monitor_state.borrow();
        popup.set_has_monitors(state.has_monitors());
        resize_popup_to_content(popup, state.monitor_count())
    }

    fn request_refresh(self: &Rc<Self>) {
        self.apply_timer.stop();
        self.monitor_state.borrow_mut().discard_pending();
        self.set_refreshing(true);
        self.set_status_message("");

        let request_id = self.next_request_id();
        self.latest_refresh_id.set(request_id);
        match self.monitor_worker.refresh(request_id) {
            Ok(()) => self.track_worker_request(),
            Err(error) => {
                eprintln!("failed to queue monitor refresh: {error}");
                self.set_refreshing(false);
                self.set_status_message("Couldn't refresh monitors.");
            }
        }
    }

    fn request_apply(self: &Rc<Self>, updates: Vec<monitor_hardware::BrightnessUpdate>) {
        let request_id = self.next_request_id();
        match self.monitor_worker.apply(request_id, updates) {
            Ok(()) => self.track_worker_request(),
            Err(error) => {
                eprintln!("failed to queue brightness update: {error}");
                self.set_status_message("Couldn't change brightness.");
            }
        }
    }

    fn next_request_id(&self) -> u64 {
        let request_id = self.next_request_id.get();
        self.next_request_id.set(request_id.wrapping_add(1).max(1));
        request_id
    }

    fn track_worker_request(self: &Rc<Self>) {
        let pending = self.pending_worker_requests.get() + 1;
        self.pending_worker_requests.set(pending);
        if pending != 1 {
            return;
        }

        let app = Rc::downgrade(self);
        self.monitor_event_timer
            .start(TimerMode::Repeated, Duration::from_millis(25), move || {
                if let Some(app) = app.upgrade() {
                    app.poll_monitor_events();
                }
            });
    }

    fn poll_monitor_events(self: &Rc<Self>) {
        loop {
            match self.monitor_worker.try_recv() {
                Ok(MonitorEvent::Refreshed { request_id, result }) => {
                    self.handle_refresh_result(request_id, result);
                    self.finish_worker_request();
                }
                Ok(MonitorEvent::Applied { request_id, report }) => {
                    let _ = request_id;
                    self.handle_apply_report(report);
                    self.finish_worker_request();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    eprintln!("monitor worker disconnected");
                    self.pending_worker_requests.set(0);
                    self.monitor_event_timer.stop();
                    self.set_refreshing(false);
                    self.set_status_message("Monitor service stopped.");
                    break;
                }
            }
        }
    }

    fn handle_refresh_result(&self, request_id: u64, result: Result<RefreshResult, String>) {
        if request_id != self.latest_refresh_id.get() {
            return;
        }

        self.set_refreshing(false);
        match result {
            Ok(result) => {
                let has_warnings = !result.warnings.is_empty();
                for warning in result.warnings {
                    eprintln!("{warning}");
                }
                self.monitor_state
                    .borrow_mut()
                    .replace_snapshots(result.snapshots);
                self.set_status_message(if has_warnings {
                    "Some monitors couldn't be refreshed."
                } else {
                    ""
                });
            }
            Err(error) => {
                eprintln!("failed to refresh monitors: {error}");
                self.set_status_message("Couldn't refresh monitors.");
            }
        }

        self.with_popup(|popup| {
            let popup_height = self.resize_popup_from_state(popup);
            if popup.window().is_visible() {
                position_popup(popup, popup_height);
                stabilize_popup_position(popup, popup_height);
            }
        });
    }

    fn handle_apply_report(&self, report: ApplyReport) {
        let errors = self
            .monitor_state
            .borrow_mut()
            .reconcile_apply_report(report);
        if errors.is_empty() {
            self.set_status_message("");
        } else {
            for error in errors {
                eprintln!("{error}");
            }
            self.set_status_message("Couldn't change brightness.");
        }
    }

    fn finish_worker_request(&self) {
        let pending = self.pending_worker_requests.get().saturating_sub(1);
        self.pending_worker_requests.set(pending);
        if pending == 0 {
            self.monitor_event_timer.stop();
        }
    }

    fn set_status_message(&self, message: &str) {
        self.with_popup(|popup| popup.set_status_message(message.into()));
    }

    fn set_refreshing(&self, refreshing: bool) {
        self.with_popup(|popup| popup.set_refreshing(refreshing));
    }

    fn toggle_windows_theme(&self) {
        let next_dark_mode = windows_integration::next_windows_dark_mode();

        match windows_integration::set_windows_dark_mode(next_dark_mode) {
            Ok(()) => {
                self.dark_mode.set(next_dark_mode);
                self.update_tray_icon(next_dark_mode);
                self.with_popup(|popup| popup.set_dark_mode(next_dark_mode));
            }
            Err(error) => eprintln!("failed to change Windows theme: {error}"),
        }
    }

    fn update_tray_icon(&self, dark_mode: bool) {
        self.tray.set_app_icon(tray_icon_for_dark_mode(
            dark_mode,
            &self.tray_light_icon,
            &self.tray_dark_icon,
        ));
    }

    fn start_outside_click_watcher(self: &Rc<Self>) {
        self.left_button_was_down
            .set(windows_integration::left_mouse_button_down());
        self.click_started_inside_popup.set(false);

        let app = Rc::downgrade(self);
        self.outside_click_timer
            .start(TimerMode::Repeated, Duration::from_millis(50), move || {
                if let Some(app) = app.upgrade() {
                    app.poll_outside_click();
                }
            });
    }

    fn stop_outside_click_watcher(&self) {
        self.outside_click_timer.stop();
        self.click_started_inside_popup.set(false);
    }

    fn hide_popup(&self, popup: &MainWindow) {
        popup.hide().ok();
        self.stop_outside_click_watcher();
    }

    fn poll_outside_click(&self) {
        let left_button_is_down = windows_integration::left_mouse_button_down();
        let left_button_went_down = left_button_is_down && !self.left_button_was_down.get();
        let left_button_went_up = !left_button_is_down && self.left_button_was_down.get();

        let popup_ref = self.popup.borrow();
        let Some(popup) = popup_ref.as_ref() else {
            self.stop_outside_click_watcher();
            return;
        };
        if !popup.window().is_visible() {
            self.stop_outside_click_watcher();
            return;
        }

        if left_button_went_down {
            self.click_started_inside_popup
                .set(cursor_is_inside_popup(popup));
        }

        if left_button_went_up
            && !self.click_started_inside_popup.get()
            && !cursor_is_inside_popup(popup)
        {
            self.hide_popup(popup);
        }

        self.left_button_was_down.set(left_button_is_down);
    }

    fn with_popup(&self, action: impl FnOnce(&MainWindow)) {
        if let Some(popup) = self.popup.borrow().as_ref() {
            action(popup);
        }
    }
}

#[derive(Clone, Copy)]
struct PopupLayoutMetrics {
    width: f32,
    min_height: f32,
    max_height: f32,
    chrome_height: f32,
    empty_body_height: f32,
    monitor_row_height: f32,
    monitor_row_spacing: f32,
}

impl PopupLayoutMetrics {
    fn from_popup(popup: &MainWindow) -> Self {
        let metrics = popup.global::<PopupMetrics>();
        Self {
            width: metrics.get_window_width(),
            min_height: metrics.get_min_window_height(),
            max_height: metrics.get_max_window_height(),
            chrome_height: metrics.get_chrome_height(),
            empty_body_height: metrics.get_empty_body_height(),
            monitor_row_height: metrics.get_monitor_row_height(),
            monitor_row_spacing: metrics.get_monitor_row_spacing(),
        }
    }
}

fn resize_popup_to_content(popup: &MainWindow, monitor_count: usize) -> f32 {
    let metrics = PopupLayoutMetrics::from_popup(popup);
    let popup_height = clamped_popup_height_for_work_area(
        popup_height_for_monitor_count(metrics, monitor_count),
        metrics.min_height,
    );
    popup.set_body_height(popup_height - metrics.chrome_height);
    popup
        .window()
        .set_size(LogicalSize::new(metrics.width, popup_height));
    popup_height
}

fn popup_height_for_monitor_count(metrics: PopupLayoutMetrics, monitor_count: usize) -> f32 {
    let body_height = if monitor_count == 0 {
        metrics.empty_body_height
    } else {
        let row_count = monitor_count as f32;
        row_count * metrics.monitor_row_height + (row_count - 1.0) * metrics.monitor_row_spacing
    };

    (metrics.chrome_height + body_height).clamp(metrics.min_height, metrics.max_height)
}

fn clamped_popup_height_for_work_area(popup_height: f32, min_height: f32) -> f32 {
    let Some(area) = windows_integration::work_area_near_cursor() else {
        return popup_height;
    };

    let scale_factor = area.scale_factor.max(1.0);
    let available_height =
        ((area.bottom - area.top - POPUP_MARGIN * 2) as f32 / scale_factor).max(min_height);
    popup_height.min(available_height)
}

fn position_popup(popup: &MainWindow, popup_height: f32) {
    let size = popup.window().size();

    if let Some(area) = windows_integration::work_area_near_cursor() {
        let scale_factor = area
            .scale_factor
            .max(popup.window().scale_factor())
            .max(1.0);
        let popup_width = PopupLayoutMetrics::from_popup(popup).width;
        let width = (popup_width * scale_factor).ceil() as i32;
        let height = (popup_height * scale_factor).ceil() as i32;
        let width = width.max(size.width as i32).max(1);
        let height = height.max(size.height as i32).max(1);
        let target_x = area.right - width - POPUP_MARGIN;
        let target_y = area.bottom - height - POPUP_MARGIN;

        popup.window().set_position(PhysicalPosition {
            x: clamp_to_work_area(target_x, area.left, area.right, width),
            y: clamp_to_work_area(target_y, area.top, area.bottom, height),
        });
    }
}

fn stabilize_popup_position(popup: &MainWindow, popup_height: f32) {
    for delay_ms in POPUP_POSITION_CORRECTION_DELAYS_MS {
        schedule_popup_position_correction(popup, popup_height, delay_ms);
    }
}

fn schedule_popup_position_correction(popup: &MainWindow, popup_height: f32, delay_ms: u64) {
    let popup_weak = popup.as_weak();

    Timer::single_shot(Duration::from_millis(delay_ms), move || {
        let Some(popup) = popup_weak.upgrade() else {
            return;
        };

        if popup.window().is_visible() {
            position_popup(&popup, popup_height);
        }
    });
}

fn clamp_to_work_area(value: i32, start: i32, end: i32, size: i32) -> i32 {
    let min = start + POPUP_MARGIN;
    let max = end - size - POPUP_MARGIN;

    if max < min {
        min
    } else {
        value.clamp(min, max)
    }
}

fn cursor_is_inside_popup(popup: &MainWindow) -> bool {
    let position = popup.window().position();
    let size = popup.window().size();

    windows_integration::cursor_is_in_rect(
        position.x,
        position.y,
        position.x + size.width as i32,
        position.y + size.height as i32,
    )
}

fn build_icon(icon_data: &'static [u8]) -> Image {
    slint::private_unstable_api::re_exports::load_image_from_dynamic_data(icon_data, "ico")
        .expect("embedded application icon should be a valid ICO image")
}

fn tray_icon_for_dark_mode(dark_mode: bool, light_icon: &Image, dark_icon: &Image) -> Image {
    if dark_mode {
        dark_icon.clone()
    } else {
        light_icon.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{PopupLayoutMetrics, clamp_to_work_area, popup_height_for_monitor_count};

    fn popup_metrics() -> PopupLayoutMetrics {
        PopupLayoutMetrics {
            width: 348.0,
            min_height: 148.0,
            max_height: 560.0,
            chrome_height: 75.0,
            empty_body_height: 104.0,
            monitor_row_height: 70.0,
            monitor_row_spacing: 12.0,
        }
    }

    #[test]
    fn popup_height_tracks_monitor_rows_and_clamps_to_limits() {
        let metrics = popup_metrics();
        assert_eq!(popup_height_for_monitor_count(metrics, 0), 179.0);
        assert_eq!(popup_height_for_monitor_count(metrics, 1), 148.0);
        assert_eq!(popup_height_for_monitor_count(metrics, 2), 227.0);
        assert_eq!(popup_height_for_monitor_count(metrics, 6), 555.0);
        assert_eq!(popup_height_for_monitor_count(metrics, 7), 560.0);
    }

    #[test]
    fn popup_position_stays_inside_the_work_area_margin() {
        assert_eq!(clamp_to_work_area(900, 0, 1000, 100), 888);
        assert_eq!(clamp_to_work_area(-50, 0, 1000, 100), 12);
        assert_eq!(clamp_to_work_area(400, 0, 1000, 100), 400);
    }
}
