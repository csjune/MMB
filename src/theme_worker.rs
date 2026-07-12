use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use crate::windows_integration;

enum ThemeCommand {
    Toggle,
}

pub(crate) enum ThemeEvent {
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
        thread::spawn(move || {
            while let Ok(ThemeCommand::Toggle) = command_receiver.recv() {
                let next_dark_mode = windows_integration::next_windows_dark_mode();
                let result = windows_integration::set_windows_dark_mode(next_dark_mode)
                    .map(|()| windows_integration::windows_main_dark_mode())
                    .map_err(|error| error.to_string());
                if event_sender.send(ThemeEvent::Toggled(result)).is_err() {
                    break;
                }
            }
        });

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
