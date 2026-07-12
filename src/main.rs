#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod monitor_hardware;
mod monitor_state;
mod windows_integration;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

#[cfg(windows)]
use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
use slint::{
    CloseRequestResponse, ComponentHandle, Image, LogicalSize, ModelRc, PhysicalPosition, Timer,
    TimerMode,
};

use monitor_state::{MonitorState, brightness_after_scroll};

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
    apply_timer: Timer,
    outside_click_timer: Timer,
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
            apply_timer: Timer::default(),
            outside_click_timer: Timer::default(),
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
        self.outside_click_timer
            .start(TimerMode::Repeated, Duration::from_millis(50), move || {
                if let Some(app) = app.upgrade() {
                    app.poll_outside_click();
                }
            });

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
        drop(state);
        popup
            .window()
            .on_close_requested(|| CloseRequestResponse::HideWindow);

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
                app.with_popup(|popup| {
                    app.refresh_popup(popup);
                });
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
            popup.hide().ok();
            return Ok(());
        }

        let current_dark_mode = windows_integration::windows_main_dark_mode();
        self.dark_mode.set(current_dark_mode);
        popup.set_dark_mode(current_dark_mode);
        self.update_tray_icon(current_dark_mode);

        let popup_height = self.refresh_popup(popup);
        popup.show()?;
        position_popup(popup, popup_height);
        stabilize_popup_position(popup, popup_height);
        Ok(())
    }

    fn update_brightness(self: &Rc<Self>, monitor_id: i32, value: i32) {
        let sync_all = self
            .popup
            .borrow()
            .as_ref()
            .is_some_and(MainWindow::get_sync_all);
        let has_monitors = {
            let mut state = self.monitor_state.borrow_mut();
            state.update_brightness(monitor_id, value, sync_all);
            state.has_monitors()
        };
        self.with_popup(|popup| popup.set_has_monitors(has_monitors));
        self.schedule_apply();
    }

    fn scroll_brightness(self: &Rc<Self>, monitor_id: i32, delta: i32) {
        let current = self
            .monitor_state
            .borrow()
            .brightness_for_monitor(monitor_id)
            .unwrap_or(50);
        self.update_brightness(monitor_id, brightness_after_scroll(current, delta));
    }

    fn schedule_apply(self: &Rc<Self>) {
        let app: Weak<Self> = Rc::downgrade(self);
        self.apply_timer
            .start(TimerMode::SingleShot, Duration::from_secs(1), move || {
                if let Some(app) = app.upgrade() {
                    app.monitor_state.borrow_mut().apply_pending();
                }
            });
    }

    fn refresh_popup(&self, popup: &MainWindow) -> f32 {
        self.monitor_state.borrow_mut().refresh();
        let state = self.monitor_state.borrow();
        popup.set_monitors(ModelRc::new(state.model()));
        popup.set_has_monitors(state.has_monitors());
        let popup_height = resize_popup_to_content(popup, state.monitor_count());
        drop(state);
        position_popup(popup, popup_height);
        popup_height
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

    fn poll_outside_click(&self) {
        let left_button_is_down = windows_integration::left_mouse_button_down();
        let left_button_went_down = left_button_is_down && !self.left_button_was_down.get();
        let left_button_went_up = !left_button_is_down && self.left_button_was_down.get();

        self.with_popup(|popup| {
            if !popup.window().is_visible() {
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
                popup.hide().ok();
            }
        });

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
