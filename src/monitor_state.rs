use std::collections::HashMap;
use std::rc::Rc;

use slint::{Model, VecModel};

use crate::MonitorEntry;
use monbcon::{ApplyReport, BrightnessUpdate, MonitorId, MonitorSnapshot};

const TOOLTIP_NAME_MAX_CHARS: usize = 24;

pub(crate) struct MonitorState {
    generation: u64,
    model: Rc<VecModel<MonitorEntry>>,
    confirmed: HashMap<MonitorId, i32>,
    pending: HashMap<MonitorId, i32>,
}

impl MonitorState {
    pub(crate) fn new() -> Self {
        Self {
            generation: 0,
            model: Rc::new(VecModel::from(Vec::new())),
            confirmed: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    pub(crate) fn model(&self) -> Rc<VecModel<MonitorEntry>> {
        Rc::clone(&self.model)
    }

    pub(crate) fn replace_snapshots(&mut self, generation: u64, snapshots: Vec<MonitorSnapshot>) {
        let confirmed = snapshots
            .iter()
            .map(|monitor| (monitor.id.clone(), monitor.brightness))
            .collect();
        let entries: Vec<MonitorEntry> = snapshots
            .into_iter()
            .map(|monitor| MonitorEntry {
                id: monitor.id.as_str().into(),
                name: monitor.name.into(),
                brightness: monitor.brightness,
            })
            .collect();

        self.generation = generation;
        self.confirmed = confirmed;
        self.pending.clear();
        self.model.set_vec(entries);
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

    /// Scrolls every monitor at once, as the tray icon has no single slider.
    /// With sync enabled all monitors follow the first one; otherwise each
    /// monitor steps from its own value.
    pub(crate) fn scroll_all(&mut self, delta: i32, sync_all: bool) {
        let Some(first) = self.model.row_data(0) else {
            return;
        };
        let synced_value = brightness_after_scroll(first.brightness, delta);

        for row in 0..self.model.row_count() {
            let value = if sync_all {
                synced_value
            } else {
                let Some(entry) = self.model.row_data(row) else {
                    continue;
                };
                brightness_after_scroll(entry.brightness, delta)
            };
            self.set_row_brightness(row, value);
        }
    }

    /// Short brightness description for the tray tooltip, which Windows
    /// truncates to 127 characters.
    pub(crate) fn brightness_summary(&self) -> String {
        let entries: Vec<MonitorEntry> = self.model.iter().collect();
        let Some(first) = entries.first() else {
            return String::new();
        };

        if entries
            .iter()
            .all(|entry| entry.brightness == first.brightness)
        {
            return format!("Brightness: {}%", first.brightness);
        }

        entries
            .iter()
            .map(|entry| {
                format!(
                    "{}: {}%",
                    elide(&entry.name, TOOLTIP_NAME_MAX_CHARS),
                    entry.brightness
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) fn take_pending(&mut self) -> Vec<BrightnessUpdate> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|(id, value)| BrightnessUpdate {
                generation: self.generation,
                id,
                value,
            })
            .collect()
    }

    pub(crate) fn restore_unsent(&mut self, updates: &[BrightnessUpdate]) {
        for update in updates {
            if update.generation != self.generation || self.pending.contains_key(&update.id) {
                continue;
            }
            let Some(confirmed) = self.confirmed.get(&update.id).copied() else {
                continue;
            };
            let Some(row) = self.row_for_monitor(update.id.as_str()) else {
                continue;
            };
            let Some(mut entry) = self.model.row_data(row) else {
                continue;
            };
            if entry.brightness == update.value {
                entry.brightness = confirmed;
                self.model.set_row_data(row, entry);
            }
        }
    }

    pub(crate) fn reconcile_apply_report(&mut self, report: ApplyReport) -> Vec<String> {
        let mut errors = Vec::new();

        for outcome in report.outcomes {
            if outcome.generation != self.generation {
                continue;
            }

            let Some(error) = outcome.error else {
                if let Some(effective) = outcome.effective {
                    self.confirmed.insert(outcome.id, effective);
                }
                continue;
            };

            errors.push(format!(
                "failed to apply brightness for monitor {}: {error}",
                outcome.id
            ));

            if self.pending.contains_key(&outcome.id) {
                continue;
            }

            let Some(row) = self.row_for_monitor(outcome.id.as_str()) else {
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

    pub(crate) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
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
            let monitor_id = MonitorId::new(entry.id.as_str());
            self.model.set_row_data(row, entry);
            self.pending.insert(monitor_id, value);
        }
    }
}

fn elide(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut elided: String = text.chars().take(max_chars - 1).collect();
    elided.push('…');
    elided
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
    use monbcon::{ApplyOutcome, ApplyReport, MonitorId, MonitorSnapshot};

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

    fn state_with(brightness: &[(&str, i32)]) -> MonitorState {
        let mut state = MonitorState::new();
        state.replace_snapshots(
            1,
            brightness
                .iter()
                .map(|&(name, brightness)| MonitorSnapshot {
                    id: MonitorId::new(format!("wmi:{name}")),
                    name: name.into(),
                    brightness,
                })
                .collect(),
        );
        state
    }

    fn brightness_values(state: &MonitorState) -> Vec<i32> {
        state.model().iter().map(|entry| entry.brightness).collect()
    }

    #[test]
    fn tray_scroll_with_sync_moves_every_monitor_to_the_first_monitors_step() {
        let mut state = state_with(&[("A", 40), ("B", 73)]);
        state.scroll_all(120, true);
        assert_eq!(brightness_values(&state), vec![45, 45]);
        assert_eq!(state.take_pending().len(), 2);
    }

    #[test]
    fn tray_scroll_without_sync_steps_each_monitor_from_its_own_value() {
        let mut state = state_with(&[("A", 40), ("B", 73)]);
        state.scroll_all(-120, false);
        assert_eq!(brightness_values(&state), vec![35, 70]);
    }

    #[test]
    fn tray_scroll_without_monitors_does_nothing() {
        let mut state = MonitorState::new();
        state.scroll_all(120, true);
        assert!(!state.has_pending());
    }

    #[test]
    fn brightness_summary_collapses_equal_values() {
        assert_eq!(MonitorState::new().brightness_summary(), "");
        assert_eq!(
            state_with(&[("A", 70), ("B", 70)]).brightness_summary(),
            "Brightness: 70%"
        );
    }

    #[test]
    fn brightness_summary_lists_monitors_with_different_values() {
        assert_eq!(
            state_with(&[("A very long monitor name here", 40), ("B", 75)]).brightness_summary(),
            "A very long monitor nam…: 40%\nB: 75%"
        );
    }

    #[test]
    fn failed_apply_restores_the_previous_value() {
        let id = MonitorId::new("wmi:test");
        let mut state = MonitorState::new();
        state.replace_snapshots(
            1,
            vec![MonitorSnapshot {
                id: id.clone(),
                name: "Test".into(),
                brightness: 40,
            }],
        );
        state.update_brightness(id.as_str(), 75, false);
        state.take_pending();

        let errors = state.reconcile_apply_report(ApplyReport {
            outcomes: vec![ApplyOutcome {
                generation: 1,
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
        state.replace_snapshots(
            1,
            vec![MonitorSnapshot {
                id: id.clone(),
                name: "Test".into(),
                brightness: 40,
            }],
        );
        state.update_brightness(id.as_str(), 75, false);
        state.take_pending();
        state.update_brightness(id.as_str(), 80, false);

        state.reconcile_apply_report(ApplyReport {
            outcomes: vec![ApplyOutcome {
                generation: 1,
                id,
                requested: 75,
                effective: Some(40),
                error: Some("test failure".into()),
            }],
        });

        assert_eq!(state.model().row_data(0).unwrap().brightness, 80);
    }

    #[test]
    fn unsent_apply_restores_the_last_confirmed_value() {
        let id = MonitorId::new("wmi:test");
        let mut state = MonitorState::new();
        state.replace_snapshots(
            1,
            vec![MonitorSnapshot {
                id: id.clone(),
                name: "Test".into(),
                brightness: 40,
            }],
        );
        state.update_brightness(id.as_str(), 75, false);
        let updates = state.take_pending();

        state.restore_unsent(&updates);

        assert_eq!(state.model().row_data(0).unwrap().brightness, 40);
    }

    #[test]
    fn stale_apply_report_does_not_modify_a_new_generation() {
        let id = MonitorId::new("wmi:test");
        let mut state = MonitorState::new();
        state.replace_snapshots(
            2,
            vec![MonitorSnapshot {
                id: id.clone(),
                name: "Test".into(),
                brightness: 60,
            }],
        );

        state.reconcile_apply_report(ApplyReport {
            outcomes: vec![ApplyOutcome {
                generation: 1,
                id,
                requested: 60,
                effective: Some(20),
                error: Some("stale failure".into()),
            }],
        });

        assert_eq!(state.model().row_data(0).unwrap().brightness, 60);
    }
}
