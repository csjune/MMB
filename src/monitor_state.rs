use std::collections::HashMap;
use std::rc::Rc;

use slint::{Model, VecModel};

use crate::{MonitorEntry, monitor_hardware};
use monitor_hardware::MonitorId;

pub(crate) struct MonitorState {
    controller: monitor_hardware::MonitorController,
    model: Rc<VecModel<MonitorEntry>>,
    pending: HashMap<MonitorId, i32>,
}

impl MonitorState {
    pub(crate) fn new() -> Self {
        Self {
            controller: monitor_hardware::MonitorController::new(),
            model: Rc::new(VecModel::from(Vec::new())),
            pending: HashMap::new(),
        }
    }

    pub(crate) fn model(&self) -> Rc<VecModel<MonitorEntry>> {
        Rc::clone(&self.model)
    }

    pub(crate) fn refresh(&mut self) {
        match self.controller.refresh() {
            Ok(()) => {
                let entries: Vec<MonitorEntry> = self
                    .controller
                    .snapshots()
                    .into_iter()
                    .map(|monitor| MonitorEntry {
                        id: monitor.id.to_ui(),
                        name: monitor.name.into(),
                        brightness: monitor.brightness,
                    })
                    .collect();

                self.pending.clear();
                self.model.set_vec(entries);
            }
            Err(error) => {
                eprintln!("failed to refresh monitors: {error}");
                self.pending.clear();
                self.model.set_vec(Vec::new());
            }
        }
    }

    pub(crate) fn update_brightness(&mut self, monitor_id: i32, value: i32, sync_all: bool) {
        let value = value.clamp(0, 100);

        if sync_all {
            for row in 0..self.model.row_count() {
                self.set_row_brightness(row, value);
            }
        } else if let Some(row) = self.row_for_monitor(monitor_id) {
            self.set_row_brightness(row, value);
        }
    }

    pub(crate) fn apply_pending(&mut self) {
        let updates = std::mem::take(&mut self.pending);

        for (id, value) in updates {
            if let Err(error) = self.controller.set_brightness(id, value) {
                eprintln!("failed to apply brightness for monitor {id}: {error}");
            }
        }
    }

    pub(crate) fn has_monitors(&self) -> bool {
        self.model.row_count() > 0
    }

    pub(crate) fn monitor_count(&self) -> usize {
        self.model.row_count()
    }

    pub(crate) fn brightness_for_monitor(&self, monitor_id: i32) -> Option<i32> {
        self.row_for_monitor(monitor_id)
            .and_then(|row| self.model.row_data(row))
            .map(|entry| entry.brightness)
    }

    fn row_for_monitor(&self, monitor_id: i32) -> Option<usize> {
        (0..self.model.row_count()).find(|&row| {
            self.model
                .row_data(row)
                .is_some_and(|entry| entry.id == monitor_id)
        })
    }

    fn set_row_brightness(&mut self, row: usize, value: i32) {
        if let Some(mut entry) = self.model.row_data(row) {
            entry.brightness = value;
            let monitor_id = MonitorId::from_ui(entry.id);
            self.model.set_row_data(row, entry);

            if let Some(monitor_id) = monitor_id {
                self.pending.insert(monitor_id, value);
            }
        }
    }
}

pub(crate) fn brightness_after_scroll(current: i32, delta: i32) -> i32 {
    if delta > 0 {
        ((current / 5) + 1) * 5
    } else {
        (((current + 4) / 5) - 1) * 5
    }
    .clamp(0, 100)
}

#[cfg(test)]
mod tests {
    use super::brightness_after_scroll;

    #[test]
    fn scroll_snaps_to_the_next_five_percent_step() {
        assert_eq!(brightness_after_scroll(73, 5), 75);
        assert_eq!(brightness_after_scroll(73, -5), 70);
        assert_eq!(brightness_after_scroll(75, 5), 80);
        assert_eq!(brightness_after_scroll(75, -5), 70);
    }

    #[test]
    fn scroll_clamps_to_the_brightness_range() {
        assert_eq!(brightness_after_scroll(100, 5), 100);
        assert_eq!(brightness_after_scroll(0, -5), 0);
    }
}
