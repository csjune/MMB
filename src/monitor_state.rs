use std::collections::HashMap;
use std::rc::Rc;

use slint::{Model, VecModel};

use crate::MonitorEntry;
use crate::monitor_hardware::{ApplyReport, BrightnessUpdate, MonitorId, MonitorSnapshot};

pub(crate) struct MonitorState {
    model: Rc<VecModel<MonitorEntry>>,
    pending: HashMap<MonitorId, i32>,
}

impl MonitorState {
    pub(crate) fn new() -> Self {
        Self {
            model: Rc::new(VecModel::from(Vec::new())),
            pending: HashMap::new(),
        }
    }

    pub(crate) fn model(&self) -> Rc<VecModel<MonitorEntry>> {
        Rc::clone(&self.model)
    }

    pub(crate) fn replace_snapshots(&mut self, snapshots: Vec<MonitorSnapshot>) {
        let entries: Vec<MonitorEntry> = snapshots
            .into_iter()
            .map(|monitor| MonitorEntry {
                id: monitor.id.to_ui().into(),
                name: monitor.name.into(),
                brightness: monitor.brightness,
            })
            .collect();

        self.pending.clear();
        self.model.set_vec(entries);
    }

    pub(crate) fn discard_pending(&mut self) {
        self.pending.clear();
    }

    pub(crate) fn update_brightness(&mut self, monitor_id: &str, value: i32, sync_all: bool) {
        let value = value.clamp(0, 100);

        if sync_all {
            for row in 0..self.model.row_count() {
                self.set_row_brightness(row, value);
            }
        } else if let Some(row) = self.row_for_monitor(monitor_id) {
            self.set_row_brightness(row, value);
        }
    }

    pub(crate) fn take_pending(&mut self) -> Vec<BrightnessUpdate> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|(id, value)| BrightnessUpdate { id, value })
            .collect()
    }

    pub(crate) fn reconcile_apply_report(&mut self, report: ApplyReport) -> Vec<String> {
        let mut errors = Vec::new();

        for outcome in report.outcomes {
            let Some(error) = outcome.error else {
                continue;
            };

            errors.push(format!(
                "failed to apply brightness for monitor {}: {error}",
                outcome.id
            ));

            if self.pending.contains_key(&outcome.id) {
                continue;
            }

            let Some(row) = self.row_for_monitor(outcome.id.to_ui()) else {
                continue;
            };
            let Some(mut entry) = self.model.row_data(row) else {
                continue;
            };

            if entry.brightness == outcome.requested
                && let Some(effective) = outcome.effective
            {
                entry.brightness = effective;
                self.model.set_row_data(row, entry);
            }
        }

        errors
    }

    pub(crate) fn has_monitors(&self) -> bool {
        self.model.row_count() > 0
    }

    pub(crate) fn monitor_count(&self) -> usize {
        self.model.row_count()
    }

    pub(crate) fn brightness_for_monitor(&self, monitor_id: &str) -> Option<i32> {
        self.row_for_monitor(monitor_id)
            .and_then(|row| self.model.row_data(row))
            .map(|entry| entry.brightness)
    }

    fn row_for_monitor(&self, monitor_id: &str) -> Option<usize> {
        (0..self.model.row_count()).find(|&row| {
            self.model
                .row_data(row)
                .is_some_and(|entry| entry.id.as_str() == monitor_id)
        })
    }

    fn set_row_brightness(&mut self, row: usize, value: i32) {
        if let Some(mut entry) = self.model.row_data(row) {
            entry.brightness = value;
            let monitor_id = MonitorId::from_ui(entry.id.as_str());
            self.model.set_row_data(row, entry);
            self.pending.insert(monitor_id, value);
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
    use slint::Model;

    use super::{MonitorState, brightness_after_scroll};
    use crate::monitor_hardware::{ApplyOutcome, ApplyReport, MonitorId, MonitorSnapshot};

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

    #[test]
    fn failed_apply_restores_the_previous_value() {
        let id = MonitorId::new("wmi:test");
        let mut state = MonitorState::new();
        state.replace_snapshots(vec![MonitorSnapshot {
            id: id.clone(),
            name: "Test".into(),
            brightness: 40,
        }]);
        state.update_brightness(id.to_ui(), 75, false);
        state.take_pending();

        let errors = state.reconcile_apply_report(ApplyReport {
            outcomes: vec![ApplyOutcome {
                id,
                requested: 75,
                effective: Some(40),
                error: Some("test failure".into()),
            }],
        });

        assert_eq!(errors.len(), 1);
        assert_eq!(state.model().row_data(0).unwrap().brightness, 40);
    }

    #[test]
    fn failed_old_apply_does_not_overwrite_a_newer_change() {
        let id = MonitorId::new("wmi:test");
        let mut state = MonitorState::new();
        state.replace_snapshots(vec![MonitorSnapshot {
            id: id.clone(),
            name: "Test".into(),
            brightness: 40,
        }]);
        state.update_brightness(id.to_ui(), 75, false);
        state.take_pending();
        state.update_brightness(id.to_ui(), 80, false);

        state.reconcile_apply_report(ApplyReport {
            outcomes: vec![ApplyOutcome {
                id,
                requested: 75,
                effective: Some(40),
                error: Some("test failure".into()),
            }],
        });

        assert_eq!(state.model().row_data(0).unwrap().brightness, 80);
    }
}
