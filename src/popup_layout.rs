use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, LogicalSize, PhysicalPosition, Timer};

use crate::windows_integration::WorkArea;
use crate::{MainWindow, PopupMetrics};

const POPUP_MARGIN: i32 = 12;
const POPUP_POSITION_CORRECTION_DELAYS_MS: [u64; 3] = [0, 50, 200];

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

/// Sizes the popup to fit `monitor_count` rows and returns its logical height.
pub(crate) fn resize_popup_to_content(
    popup: &MainWindow,
    monitor_count: usize,
    work_area: Option<WorkArea>,
) -> f32 {
    let metrics = PopupLayoutMetrics::from_popup(popup);
    let popup_height = clamped_popup_height_for_work_area(
        popup_height_for_monitor_count(metrics, monitor_count),
        metrics.min_height,
        work_area,
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

fn clamped_popup_height_for_work_area(
    popup_height: f32,
    min_height: f32,
    work_area: Option<WorkArea>,
) -> f32 {
    let Some(area) = work_area else {
        return popup_height;
    };

    let scale_factor = area.scale_factor.max(1.0);
    let available_height =
        ((area.bottom - area.top - POPUP_MARGIN * 2) as f32 / scale_factor).max(min_height);
    popup_height.min(available_height)
}

/// Moves the popup to the bottom-right corner of the work area, then repeats
/// that a few times while the window manager settles. Corrections are
/// skipped once `position_epoch` no longer equals `expected_epoch`.
pub(crate) fn place_popup(
    popup: &MainWindow,
    popup_height: f32,
    work_area: Option<WorkArea>,
    position_epoch: &Rc<Cell<u64>>,
    expected_epoch: u64,
) {
    position_popup(popup, popup_height, work_area);
    for delay_ms in POPUP_POSITION_CORRECTION_DELAYS_MS {
        let popup_weak = popup.as_weak();
        let position_epoch = Rc::clone(position_epoch);
        Timer::single_shot(Duration::from_millis(delay_ms), move || {
            let Some(popup) = popup_weak.upgrade() else {
                return;
            };

            if position_epoch.get() == expected_epoch && popup.window().is_visible() {
                position_popup(&popup, popup_height, work_area);
            }
        });
    }
}

fn position_popup(popup: &MainWindow, popup_height: f32, work_area: Option<WorkArea>) {
    let size = popup.window().size();

    if let Some(area) = work_area {
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

fn clamp_to_work_area(value: i32, start: i32, end: i32, size: i32) -> i32 {
    let min = start + POPUP_MARGIN;
    let max = end - size - POPUP_MARGIN;

    if max < min {
        min
    } else {
        value.clamp(min, max)
    }
}

pub(crate) fn point_is_inside_popup(popup: &MainWindow, x: i32, y: i32) -> bool {
    let position = popup.window().position();
    let size = popup.window().size();

    x >= position.x
        && x < position.x + size.width as i32
        && y >= position.y
        && y < position.y + size.height as i32
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
