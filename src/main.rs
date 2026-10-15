#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod icons;
mod monitor_requests;
mod monitor_state;
mod monitor_worker;
mod notify;
mod popup;
mod popup_layout;
mod theme_worker;
mod windows_integration;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::process;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

#[cfg(windows)]
use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, SharedString, Timer, TimerMode};

use icons::Icons;
use monbcon::{ApplyReport, BrightnessUpdate, RefreshResult};
use monitor_requests::{MonitorRequests, Recovery, RefreshDecision};
use monitor_state::{MonitorState, brightness_after_scroll};
use monitor_worker::{MonitorEvent, MonitorWorker};
use notify::Notify;
use popup::PopupController;
use theme_worker::{ThemeEvent, ThemeWorker};

slint::include_modules!();

thread_local! {
    /// The UI thread's controller, for callbacks that background threads
    /// post to the event loop.
    static APP: RefCell<Weak<AppController>> = const { RefCell::new(Weak::new()) };
}

/// Runs `action` with the controller on the UI thread. Callable from any thread.
fn run_on_app(action: impl FnOnce(&Rc<AppController>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = APP.with(|app| app.borrow().upgrade()) {
            action(&app);
        }
    });
}

/// Builds a UI callback that holds only a weak reference to the controller
/// and does nothing once the controller is gone.
macro_rules! app_callback {
    ($app:expr, |$this:ident $(, $arg:ident)*| $body:expr) => {{
        let app = Rc::downgrade($app);
        move |$($arg),*| {
            if let Some($this) = app.upgrade() {
                $body
            }
        }
    }};
}

/// A notifier that background threads call to run `action` on the UI thread.
fn app_notifier(action: fn(&Rc<AppController>)) -> Notify {
    Arc::new(move || run_on_app(action))
}

fn main() {
    if let Some(exit_code) = theme_worker::run_theme_helper_if_requested() {
        process::exit(exit_code);
    }

    let _single_instance = match windows_integration::acquire_single_instance() {
        Ok(Some(guard)) => guard,
        Ok(None) => return,
        Err(error) => {
            windows_integration::show_error_message(
                "MMB couldn't start",
                &format!("MMB couldn't enforce single-instance mode.\n\n{error}"),
            );
            process::exit(1);
        }
    };

    if let Err(error) = run_app() {
        windows_integration::show_error_message(
            "MMB couldn't start",
            &format!("MMB couldn't start.\n\n{error}"),
        );
        process::exit(1);
    }
}

fn run_app() -> Result<(), Box<dyn Error>> {
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
    app.request_refresh();
    slint::run_event_loop()?;
    Ok(())
}

struct AppController {
    popup: Rc<PopupController>,
    monitor_state: RefCell<MonitorState>,
    monitor_worker: RefCell<MonitorWorker>,
    theme_worker: ThemeWorker,
    apply_timer: Timer,
    monitor_timeout_timer: Timer,
    requests: RefCell<MonitorRequests>,
    refreshing: Cell<bool>,
    sync_all: Cell<bool>,
    status_message: RefCell<SharedString>,
    theme_change_in_flight: Cell<bool>,
    dark_mode: Cell<bool>,
    tray: TrayIcon,
    icons: Icons,
}

impl AppController {
    fn new() -> Result<Rc<Self>, Box<dyn Error>> {
        let icons = Icons::load();
        let initial_dark_mode = windows_integration::windows_main_dark_mode()?;
        windows_integration::set_process_menu_dark_mode(initial_dark_mode);
        let tray = TrayIcon::new()?;
        tray.set_app_version(env!("CARGO_PKG_VERSION").into());
        tray.set_app_icon(icons.tray(initial_dark_mode));

        let mouse_watcher = windows_integration::GlobalMouseWatcher::new(
            app_notifier(|app| app.popup.poll_outside_click()),
            Box::new(|x, y, delta| {
                // Called on the hook thread; the precise hit test talks to the
                // shell, so it runs on the UI thread instead.
                run_on_app(move |app| {
                    if windows_integration::point_is_over_tray_icon(x, y) {
                        app.scroll_all_brightness(delta);
                    }
                });
            }),
        )
        .unwrap_or_else(|error| {
            eprintln!("failed to install outside-click watcher: {error}; using polling fallback");
            windows_integration::GlobalMouseWatcher::polling()
        });
        let app = Rc::new(Self {
            popup: PopupController::new(mouse_watcher),
            monitor_state: RefCell::new(MonitorState::new()),
            monitor_worker: RefCell::new(MonitorWorker::new(app_notifier(
                AppController::drain_monitor_events,
            ))),
            theme_worker: ThemeWorker::new(app_notifier(|app| app.drain_theme_events())),
            apply_timer: Timer::default(),
            monitor_timeout_timer: Timer::default(),
            requests: RefCell::new(MonitorRequests::new()),
            refreshing: Cell::new(false),
            sync_all: Cell::new(true),
            status_message: RefCell::new(SharedString::default()),
            theme_change_in_flight: Cell::new(false),
            dark_mode: Cell::new(initial_dark_mode),
            tray,
            icons,
        });
        // Posted callbacks only run once the event loop starts, so none of
        // the notifiers above can fire before this is set.
        APP.with(|slot| *slot.borrow_mut() = Rc::downgrade(&app));
        app.install_handlers();
        Ok(app)
    }

    fn install_handlers(self: &Rc<Self>) {
        self.tray.on_toggle_window(app_callback!(self, |app| {
            if !app.popup.should_toggle_on_tray_click() {
                return;
            }
            if let Err(error) = app.toggle_popup() {
                eprintln!("failed to toggle popup: {error}");
            }
        }));

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
        popup.set_app_icon(self.icons.app.clone());
        popup.set_monitors(ModelRc::new(state.model()));
        popup.set_has_monitors(state.has_monitors());
        popup.set_dark_mode(self.dark_mode.get());
        popup.set_refreshing(self.refreshing.get());
        popup.set_sync_all(self.sync_all.get());
        popup.set_theme_changing(self.theme_change_in_flight.get());
        popup.set_status_message(self.status_message.borrow().clone());
        drop(state);
        let app = Rc::downgrade(self);
        popup.window().on_close_requested(move || {
            if let Some(app) = app.upgrade() {
                app.popup.closed();
            }
            CloseRequestResponse::HideWindow
        });

        popup.on_brightness_changed(app_callback!(self, |app, monitor_id, value| {
            app.update_brightness(monitor_id, value.round() as i32)
        }));
        popup.on_brightness_scrolled(app_callback!(self, |app, monitor_id, delta| {
            app.scroll_brightness(monitor_id, delta)
        }));
        popup.on_sync_all_changed(app_callback!(self, |app, sync_all| {
            app.sync_all.set(sync_all)
        }));
        popup.on_refresh_requested(app_callback!(self, |app| app.request_refresh()));
        popup.on_theme_toggle_requested(app_callback!(self, |app| app.toggle_windows_theme()));

        Ok(popup)
    }

    fn toggle_popup(self: &Rc<Self>) -> Result<(), slint::PlatformError> {
        if self.popup.is_open() {
            self.popup.hide();
            return Ok(());
        }

        match windows_integration::windows_main_dark_mode() {
            Ok(current_dark_mode) => self.apply_windows_theme(current_dark_mode),
            Err(error) => windows_integration::show_error_message(
                "MMB",
                &format!("Couldn't read the current Windows theme.\n\n{error}"),
            ),
        }

        let monitor_count = self.monitor_state.borrow().monitor_count();
        self.popup.open(monitor_count, || self.create_popup())
    }

    fn update_brightness(self: &Rc<Self>, monitor_id: SharedString, value: i32) {
        let sync_all = self.sync_all.get();
        self.update_monitor_state(|state| {
            state.update_brightness(monitor_id.as_str(), value, sync_all)
        });
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

    fn scroll_all_brightness(self: &Rc<Self>, delta: i32) {
        // A refresh replaces every row and drops pending changes, and the
        // popup hides its sliders meanwhile, so ignore the wheel as well.
        if self.refreshing.get() {
            return;
        }
        let sync_all = self.sync_all.get();
        self.update_monitor_state(|state| state.scroll_all(delta, sync_all));
        self.apply_immediately();
    }

    /// Changes the monitor state and keeps the tray tooltip in step with it.
    fn update_monitor_state<R>(&self, update: impl FnOnce(&mut MonitorState) -> R) -> R {
        let result = update(&mut self.monitor_state.borrow_mut());
        let summary = self.monitor_state.borrow().brightness_summary();
        self.tray.set_brightness_summary(summary.into());
        result
    }

    /// Applies pending brightness changes once the sliders have been still
    /// for a second.
    fn schedule_apply(self: &Rc<Self>) {
        self.apply_timer.start(
            TimerMode::SingleShot,
            Duration::from_secs(1),
            app_callback!(self, |app| {
                if !app.requests.borrow().is_stalled() {
                    app.send_pending_apply();
                }
            }),
        );
    }

    /// Applies pending brightness changes now, or as soon as the worker
    /// finishes what it is doing, skipping the slider delay.
    fn apply_immediately(self: &Rc<Self>) {
        self.apply_timer.stop();
        if self.requests.borrow_mut().apply_now_or_when_idle() {
            self.send_pending_apply();
        }
    }

    fn send_pending_apply(self: &Rc<Self>) {
        let updates = self.monitor_state.borrow_mut().take_pending();
        if !updates.is_empty() {
            self.request_apply(updates);
        }
    }

    fn request_refresh(self: &Rc<Self>) {
        let decision = self.requests.borrow_mut().request_refresh();
        match decision {
            RefreshDecision::Send { request_id } => self.send_refresh(request_id),
            RefreshDecision::Coalesced => {}
            RefreshDecision::Deferred => self.set_status_message("Monitor service is still busy."),
        }
    }

    /// Sends a refresh, carrying any brightness changes not applied yet.
    fn send_refresh(self: &Rc<Self>, request_id: u64) {
        self.apply_timer.stop();
        let updates = self.monitor_state.borrow_mut().take_pending();
        self.set_refreshing(true);
        self.set_status_message("");

        let tracked_updates = updates.clone();
        let queued = if updates.is_empty() {
            self.monitor_worker.borrow().refresh(request_id)
        } else {
            self.monitor_worker
                .borrow()
                .apply_then_refresh(request_id, updates)
        };
        match queued {
            Ok(()) => self
                .requests
                .borrow_mut()
                .track(request_id, tracked_updates),
            Err(error) => {
                eprintln!("failed to queue monitor refresh: {}", error.message);
                self.update_monitor_state(|state| state.restore_unsent(&error.updates));
                self.requests.borrow_mut().cancel_refresh();
                self.set_refreshing(false);
                self.set_status_message("Couldn't refresh monitors.");
            }
        }
    }

    fn request_apply(self: &Rc<Self>, updates: Vec<BrightnessUpdate>) {
        let request_id = self.requests.borrow_mut().begin_apply();
        let tracked_updates = updates.clone();
        match self.monitor_worker.borrow().apply(request_id, updates) {
            Ok(()) => self
                .requests
                .borrow_mut()
                .track(request_id, tracked_updates),
            Err(error) => {
                eprintln!("failed to queue brightness update: {}", error.message);
                self.update_monitor_state(|state| state.restore_unsent(&error.updates));
                self.set_status_message("Couldn't change brightness.");
            }
        }
    }

    fn drain_monitor_events(self: &Rc<Self>) {
        loop {
            let event = self.monitor_worker.borrow().try_recv();
            match event {
                Ok(MonitorEvent::Started { request_id }) => {
                    self.requests
                        .borrow_mut()
                        .mark_started(request_id, Instant::now());
                }
                Ok(MonitorEvent::Refreshed {
                    request_id,
                    apply_report,
                    result,
                }) => {
                    self.handle_refresh_result(request_id, apply_report, result);
                    self.finish_worker_request(request_id);
                }
                Ok(MonitorEvent::Applied { request_id, report }) => {
                    self.handle_apply_report(report);
                    self.finish_worker_request(request_id);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    eprintln!("monitor worker disconnected");
                    self.restart_monitor_worker("Monitor service stopped.");
                    return;
                }
            }
        }

        self.arm_monitor_timeout();
    }

    /// Wakes up when the oldest running worker request would time out.
    fn arm_monitor_timeout(self: &Rc<Self>) {
        let deadline = self.requests.borrow().next_deadline();
        let Some(deadline) = deadline else {
            self.monitor_timeout_timer.stop();
            return;
        };

        self.monitor_timeout_timer.start(
            TimerMode::SingleShot,
            deadline.saturating_duration_since(Instant::now()),
            app_callback!(self, |app| app.check_monitor_timeout()),
        );
    }

    fn check_monitor_timeout(self: &Rc<Self>) {
        let timed_out = self.requests.borrow_mut().check_timeout(Instant::now());
        if timed_out {
            self.set_refreshing(false);
            self.set_status_message("Monitor service timed out.");
        }
        self.arm_monitor_timeout();
    }

    fn handle_refresh_result(
        self: &Rc<Self>,
        request_id: u64,
        apply_report: Option<ApplyReport>,
        result: Result<RefreshResult, String>,
    ) {
        if !self.requests.borrow().is_latest_refresh(request_id) {
            return;
        }

        if let Some(report) = apply_report {
            self.handle_apply_report(report);
        }
        match result {
            Ok(result) => {
                let has_warnings = !result.warnings.is_empty();
                for warning in result.warnings {
                    eprintln!("{warning}");
                }
                self.update_monitor_state(|state| {
                    state.replace_snapshots(result.generation, result.snapshots)
                });
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

        let (has_monitors, monitor_count) = {
            let state = self.monitor_state.borrow();
            (state.has_monitors(), state.monitor_count())
        };
        self.popup
            .with(|popup| popup.set_has_monitors(has_monitors));
        self.popup.relayout(monitor_count);

        let follow_up = self.requests.borrow_mut().complete_refresh();
        match follow_up {
            Some(request_id) => self.send_refresh(request_id),
            None => self.set_refreshing(false),
        }
    }

    fn handle_apply_report(&self, report: ApplyReport) {
        let errors = self.update_monitor_state(|state| state.reconcile_apply_report(report));
        if errors.is_empty() {
            self.set_status_message("");
        } else {
            for error in errors {
                eprintln!("{error}");
            }
            self.set_status_message("Couldn't change brightness.");
        }
    }

    fn finish_worker_request(self: &Rc<Self>, request_id: u64) {
        let recovery = self.requests.borrow_mut().finish(request_id);
        match recovery {
            Some(Recovery::Refresh) => self.request_refresh(),
            Some(Recovery::ResumeApply) if self.monitor_state.borrow().has_pending() => {
                self.schedule_apply();
            }
            Some(Recovery::ResumeApply) | None => {}
        }
        if self.requests.borrow_mut().take_apply_when_idle() {
            self.apply_immediately();
        }
    }

    fn restart_monitor_worker(self: &Rc<Self>, status_message: &str) {
        let updates = self.requests.borrow_mut().reset();
        self.update_monitor_state(|state| state.restore_unsent(&updates));
        self.monitor_worker.replace(MonitorWorker::new(app_notifier(
            AppController::drain_monitor_events,
        )));
        self.monitor_timeout_timer.stop();
        self.set_refreshing(false);
        self.set_status_message(status_message);
        self.request_refresh();
    }

    fn set_status_message(&self, message: &str) {
        let message: SharedString = message.into();
        self.status_message.replace(message.clone());
        self.popup.with(|popup| popup.set_status_message(message));
    }

    fn set_refreshing(&self, refreshing: bool) {
        self.refreshing.set(refreshing);
        self.popup.with(|popup| popup.set_refreshing(refreshing));
    }

    fn toggle_windows_theme(self: &Rc<Self>) {
        if self.theme_change_in_flight.replace(true) {
            return;
        }
        self.popup.with(|popup| popup.set_theme_changing(true));
        self.set_status_message("");

        if let Err(error) = self.theme_worker.toggle() {
            eprintln!("failed to queue Windows theme change: {error}");
            self.finish_theme_change();
            self.set_status_message("Couldn't change Windows theme.");
        }
    }

    fn drain_theme_events(&self) {
        loop {
            match self.theme_worker.try_recv() {
                Ok(ThemeEvent::Changed(Ok(dark_mode))) => {
                    self.apply_windows_theme(dark_mode);
                }
                Ok(ThemeEvent::Changed(Err(error))) => {
                    eprintln!("Windows theme watcher stopped: {error}");
                }
                Ok(ThemeEvent::Toggled(Ok(dark_mode))) => {
                    self.apply_windows_theme(dark_mode);
                    self.finish_theme_change();
                }
                Ok(ThemeEvent::Toggled(Err(error))) => {
                    eprintln!("failed to change Windows theme: {error}");
                    if let Ok(dark_mode) = windows_integration::windows_main_dark_mode() {
                        self.apply_windows_theme(dark_mode);
                    }
                    self.finish_theme_change();
                    self.set_status_message("Couldn't change Windows theme.");
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    eprintln!("theme worker disconnected");
                    self.finish_theme_change();
                    self.set_status_message("Theme service stopped.");
                    break;
                }
            }
        }
    }

    fn finish_theme_change(&self) {
        self.theme_change_in_flight.set(false);
        self.popup.with(|popup| popup.set_theme_changing(false));
    }

    fn apply_windows_theme(&self, dark_mode: bool) {
        if self.dark_mode.replace(dark_mode) == dark_mode {
            return;
        }
        windows_integration::set_process_menu_dark_mode(dark_mode);
        self.update_tray_icon(dark_mode);
        self.popup.with(|popup| popup.set_dark_mode(dark_mode));
    }

    fn update_tray_icon(&self, dark_mode: bool) {
        self.tray.set_app_icon(self.icons.tray(dark_mode));
    }
}
