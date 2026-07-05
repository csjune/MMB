#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod monitor_hardware;
mod monitor_state;
mod windows_integration;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::{
    CloseRequestResponse, ComponentHandle, Image, LogicalSize, ModelRc, PhysicalPosition, Timer,
    TimerMode, VecModel,
};

use monitor_state::{brightness_after_scroll, MonitorState};

slint::include_modules!();

const POPUP_MARGIN: i32 = 12;
const POPUP_WIDTH: f32 = 348.0;
const POPUP_MIN_HEIGHT: f32 = 148.0;
const POPUP_MAX_HEIGHT: f32 = 560.0;
const POPUP_CHROME_HEIGHT: f32 = 75.0;
const POPUP_EMPTY_BODY_HEIGHT: f32 = 104.0;
const MONITOR_ROW_HEIGHT: f32 = 70.0;
const MONITOR_ROW_SPACING: f32 = 12.0;
const APP_ICON_ICO: &[u8] = include_bytes!("../assets/app.ico");
const TRAY_ICON_LIGHT_ICO: &[u8] = include_bytes!("../assets/tray-light.ico");
const TRAY_ICON_DARK_ICO: &[u8] = include_bytes!("../assets/tray-dark.ico");

fn main() -> Result<(), slint::PlatformError> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()?;

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

    let model = Rc::new(VecModel::<MonitorEntry>::from(Vec::new()));
    let state = Rc::new(RefCell::new(MonitorState::new(model)));
    let apply_timer = Rc::new(Timer::default());
    let popup = Rc::new(RefCell::new(None::<MainWindow>));
    let dark_mode = Rc::new(Cell::new(initial_dark_mode));
    let outside_click_timer = Timer::default();
    let left_button_was_down = Rc::new(Cell::new(windows_integration::left_mouse_button_down()));
    let click_started_inside_popup = Rc::new(Cell::new(false));

    {
        let popup = Rc::clone(&popup);
        let left_button_was_down = Rc::clone(&left_button_was_down);
        let click_started_inside_popup = Rc::clone(&click_started_inside_popup);
        outside_click_timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
            let left_button_is_down = windows_integration::left_mouse_button_down();
            let left_button_went_down = left_button_is_down && !left_button_was_down.get();
            let left_button_went_up = !left_button_is_down && left_button_was_down.get();

            if let Some(popup) = popup.borrow().as_ref() {
                if popup.window().is_visible() {
                    if left_button_went_down {
                        click_started_inside_popup.set(cursor_is_inside_popup(&popup));
                    }

                    if left_button_went_up
                        && !click_started_inside_popup.get()
                        && !cursor_is_inside_popup(&popup)
                    {
                        popup.hide().ok();
                    }
                }
            }

            left_button_was_down.set(left_button_is_down);
        });
    }

    {
        let popup = Rc::clone(&popup);
        let app_icon = app_icon.clone();
        let state = Rc::clone(&state);
        let model = state.borrow().model();
        let apply_timer = Rc::clone(&apply_timer);
        let dark_mode = Rc::clone(&dark_mode);
        let tray_for_toggle = tray.clone_strong();
        let tray_light_icon = tray_light_icon.clone();
        let tray_dark_icon = tray_dark_icon.clone();
        let left_button_was_down = Rc::clone(&left_button_was_down);
        tray.on_toggle_window(move || {
            if let Err(error) = toggle_popup(
                &popup,
                app_icon.clone(),
                Rc::clone(&model),
                Rc::clone(&state),
                Rc::clone(&apply_timer),
                Rc::clone(&dark_mode),
                tray_for_toggle.clone_strong(),
                tray_light_icon.clone(),
                tray_dark_icon.clone(),
            ) {
                eprintln!("failed to toggle popup: {error}");
            }
            left_button_was_down.set(windows_integration::left_mouse_button_down());
        });
    }

    tray.on_quit_requested(|| {
        slint::quit_event_loop().ok();
    });

    tray.show()?;
    slint::run_event_loop()
}

fn schedule_apply(timer: &Timer, state: Rc<RefCell<MonitorState>>) {
    timer.start(TimerMode::SingleShot, Duration::from_secs(1), move || {
        state.borrow_mut().apply_pending();
    });
}

fn create_popup(
    app_icon: Image,
    model: Rc<VecModel<MonitorEntry>>,
    state: Rc<RefCell<MonitorState>>,
    apply_timer: Rc<Timer>,
    dark_mode: Rc<Cell<bool>>,
    tray: TrayIcon,
    tray_light_icon: Image,
    tray_dark_icon: Image,
) -> Result<MainWindow, slint::PlatformError> {
    let popup = MainWindow::new()?;

    popup.set_app_icon(app_icon);
    popup.set_monitors(ModelRc::new(model));
    popup.set_dark_mode(dark_mode.get());
    popup
        .window()
        .on_close_requested(|| CloseRequestResponse::HideWindow);
    windows_integration::hide_window_from_taskbar(popup.window());

    {
        let state = Rc::clone(&state);
        let apply_timer = Rc::clone(&apply_timer);
        let popup_weak = popup.as_weak();
        popup.on_brightness_changed(move |monitor_id, value| {
            if let Some(popup) = popup_weak.upgrade() {
                let sync_all = popup.get_sync_all();
                state
                    .borrow_mut()
                    .update_brightness(monitor_id, value.round() as i32, sync_all);
                popup.set_has_monitors(state.borrow().has_monitors());
                schedule_apply(&apply_timer, Rc::clone(&state));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let apply_timer = Rc::clone(&apply_timer);
        let popup_weak = popup.as_weak();
        popup.on_brightness_scrolled(move |monitor_id, delta| {
            if let Some(popup) = popup_weak.upgrade() {
                let current = state
                    .borrow()
                    .brightness_for_monitor(monitor_id)
                    .unwrap_or(50);
                let sync_all = popup.get_sync_all();
                state.borrow_mut().update_brightness(
                    monitor_id,
                    brightness_after_scroll(current, delta),
                    sync_all,
                );
                popup.set_has_monitors(state.borrow().has_monitors());
                schedule_apply(&apply_timer, Rc::clone(&state));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let popup_weak = popup.as_weak();
        popup.on_refresh_requested(move || {
            if let Some(popup) = popup_weak.upgrade() {
                refresh_popup(&popup, Rc::clone(&state));
            }
        });
    }

    {
        let dark_mode = Rc::clone(&dark_mode);
        let popup_weak = popup.as_weak();
        popup.on_theme_toggle_requested(move || {
            let next_dark_mode = windows_integration::next_windows_dark_mode();

            match windows_integration::set_windows_dark_mode(next_dark_mode) {
                Ok(()) => {
                    dark_mode.set(next_dark_mode);
                    tray.set_app_icon(tray_icon_for_dark_mode(
                        next_dark_mode,
                        &tray_light_icon,
                        &tray_dark_icon,
                    ));

                    if let Some(popup) = popup_weak.upgrade() {
                        popup.set_dark_mode(next_dark_mode);
                    }
                }
                Err(error) => {
                    eprintln!("failed to change Windows theme: {error}");
                }
            }
        });
    }

    Ok(popup)
}

fn refresh_popup(popup: &MainWindow, state: Rc<RefCell<MonitorState>>) -> f32 {
    state.borrow_mut().refresh();
    let state_ref = state.borrow();
    popup.set_monitors(ModelRc::new(state_ref.model()));
    popup.set_has_monitors(state_ref.has_monitors());
    let popup_height = resize_popup_to_content(popup, state_ref.monitor_count());
    drop(state_ref);
    position_popup(popup, popup_height);
    popup_height
}

fn toggle_popup(
    popup: &Rc<RefCell<Option<MainWindow>>>,
    app_icon: Image,
    model: Rc<VecModel<MonitorEntry>>,
    state: Rc<RefCell<MonitorState>>,
    apply_timer: Rc<Timer>,
    dark_mode: Rc<Cell<bool>>,
    tray: TrayIcon,
    tray_light_icon: Image,
    tray_dark_icon: Image,
) -> Result<(), slint::PlatformError> {
    if popup.borrow().is_none() {
        *popup.borrow_mut() = Some(create_popup(
            app_icon,
            model,
            Rc::clone(&state),
            apply_timer,
            Rc::clone(&dark_mode),
            tray.clone_strong(),
            tray_light_icon.clone(),
            tray_dark_icon.clone(),
        )?);
    }

    let popup = popup.borrow();
    let popup = popup.as_ref().expect("popup was just created");

    if popup.window().is_visible() {
        popup.hide().ok();
        return Ok(());
    }

    let current_dark_mode = windows_integration::windows_main_dark_mode();
    dark_mode.set(current_dark_mode);
    popup.set_dark_mode(current_dark_mode);
    tray.set_app_icon(tray_icon_for_dark_mode(
        current_dark_mode,
        &tray_light_icon,
        &tray_dark_icon,
    ));
    let popup_height = refresh_popup(popup, state);
    popup.show()?;
    windows_integration::hide_window_from_taskbar(popup.window());
    position_popup(popup, popup_height);
    schedule_popup_position_correction(popup, popup_height, 0);
    schedule_popup_position_correction(popup, popup_height, 50);
    schedule_popup_position_correction(popup, popup_height, 200);
    Ok(())
}

fn resize_popup_to_content(popup: &MainWindow, monitor_count: usize) -> f32 {
    let popup_height =
        clamped_popup_height_for_work_area(popup_height_for_monitor_count(monitor_count));
    popup.set_body_height(popup_height - POPUP_CHROME_HEIGHT);
    popup
        .window()
        .set_size(LogicalSize::new(POPUP_WIDTH, popup_height));
    popup_height
}

fn popup_height_for_monitor_count(monitor_count: usize) -> f32 {
    let body_height = if monitor_count == 0 {
        POPUP_EMPTY_BODY_HEIGHT
    } else {
        let row_count = monitor_count as f32;
        row_count * MONITOR_ROW_HEIGHT + (row_count - 1.0) * MONITOR_ROW_SPACING
    };

    (POPUP_CHROME_HEIGHT + body_height).clamp(POPUP_MIN_HEIGHT, POPUP_MAX_HEIGHT)
}

fn clamped_popup_height_for_work_area(popup_height: f32) -> f32 {
    let Some(area) = windows_integration::work_area_near_cursor() else {
        return popup_height;
    };

    let scale_factor = area.scale_factor.max(1.0);
    let available_height =
        ((area.bottom - area.top - POPUP_MARGIN * 2) as f32 / scale_factor).max(POPUP_MIN_HEIGHT);

    popup_height.min(available_height)
}

fn position_popup(popup: &MainWindow, popup_height: f32) {
    let size = popup.window().size();

    if let Some(area) = windows_integration::work_area_near_cursor() {
        let scale_factor = area
            .scale_factor
            .max(popup.window().scale_factor())
            .max(1.0);
        let width = (POPUP_WIDTH * scale_factor).ceil() as i32;
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
