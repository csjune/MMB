use std::env;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::windows_integration;

const THEME_HELPER_ARGUMENT: &str = "--mmb-theme-helper";
const THEME_HELPER_TIMEOUT: Duration = Duration::from_secs(10);
const THEME_HELPER_POLL_INTERVAL: Duration = Duration::from_millis(25);

enum ThemeCommand {
    Toggle,
}

pub(crate) enum ThemeEvent {
    Changed(Result<bool, String>),
    Toggled(Result<bool, String>),
}

pub(crate) struct ThemeWorker {
    commands: Sender<ThemeCommand>,
    events: Receiver<ThemeEvent>,
}

impl ThemeWorker {
    pub(crate) fn new() -> Self {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let watcher_event_sender = event_sender.clone();
        thread::spawn(move || {
            while let Ok(ThemeCommand::Toggle) = command_receiver.recv() {
                let result = windows_integration::next_windows_dark_mode()
                    .map_err(|error| error.to_string())
                    .and_then(run_theme_helper)
                    .and_then(|()| {
                        windows_integration::windows_main_dark_mode()
                            .map_err(|error| error.to_string())
                    });
                if event_sender.send(ThemeEvent::Toggled(result)).is_err() {
                    break;
                }
            }
        });
        spawn_theme_watcher(watcher_event_sender);

        Self {
            commands: command_sender,
            events: event_receiver,
        }
    }

    pub(crate) fn toggle(&self) -> Result<(), String> {
        self.commands
            .send(ThemeCommand::Toggle)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn try_recv(&self) -> Result<ThemeEvent, TryRecvError> {
        self.events.try_recv()
    }
}

fn spawn_theme_watcher(event_sender: Sender<ThemeEvent>) {
    #[cfg(windows)]
    thread::spawn(move || {
        let watcher = match windows_integration::WindowsThemeWatcher::new() {
            Ok(watcher) => watcher,
            Err(error) => {
                let _ = event_sender.send(ThemeEvent::Changed(Err(error.to_string())));
                return;
            }
        };

        loop {
            let result = watcher
                .wait_for_change()
                .and_then(|()| windows_integration::windows_main_dark_mode());
            let stop = result.is_err();
            if event_sender
                .send(ThemeEvent::Changed(
                    result.map_err(|error| error.to_string()),
                ))
                .is_err()
                || stop
            {
                break;
            }
        }
    });

    #[cfg(not(windows))]
    drop(event_sender);
}

pub(crate) fn run_theme_helper_if_requested() -> Option<i32> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let dark_mode = match parse_theme_helper_arguments(&arguments)? {
        Ok(dark_mode) => dark_mode,
        Err(()) => return Some(2),
    };

    Some(
        windows_integration::set_windows_dark_mode(dark_mode)
            .map(|()| 0)
            .unwrap_or(1),
    )
}

fn parse_theme_helper_arguments(arguments: &[String]) -> Option<Result<bool, ()>> {
    if arguments.first().map(String::as_str) != Some(THEME_HELPER_ARGUMENT) {
        return None;
    }

    Some(match arguments {
        [_, mode] if mode == "dark" => Ok(true),
        [_, mode] if mode == "light" => Ok(false),
        _ => Err(()),
    })
}

fn run_theme_helper(dark_mode: bool) -> Result<(), String> {
    let executable = env::current_exe()
        .map_err(|error| format!("failed to locate the theme helper executable: {error}"))?;
    let mut child = Command::new(executable)
        .arg(THEME_HELPER_ARGUMENT)
        .arg(if dark_mode { "dark" } else { "light" })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to start the theme helper: {error}"))?;
    let deadline = Instant::now() + THEME_HELPER_TIMEOUT;

    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!("theme helper exited with status {status}"));
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(THEME_HELPER_POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("theme helper timed out".into());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("failed to wait for the theme helper: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_theme_helper_arguments;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn theme_helper_arguments_are_strictly_scoped() {
        assert_eq!(parse_theme_helper_arguments(&arguments(&[])), None);
        assert_eq!(
            parse_theme_helper_arguments(&arguments(&["--unrelated"])),
            None
        );
        assert_eq!(
            parse_theme_helper_arguments(&arguments(&["--mmb-theme-helper", "dark"])),
            Some(Ok(true))
        );
        assert_eq!(
            parse_theme_helper_arguments(&arguments(&["--mmb-theme-helper", "light"])),
            Some(Ok(false))
        );
        assert_eq!(
            parse_theme_helper_arguments(&arguments(&["--mmb-theme-helper"])),
            Some(Err(()))
        );
    }
}
