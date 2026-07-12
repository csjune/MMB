use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use crate::monitor_hardware::{ApplyReport, BrightnessUpdate, MonitorController, RefreshResult};

enum MonitorCommand {
    Refresh {
        request_id: u64,
    },
    Apply {
        request_id: u64,
        updates: Vec<BrightnessUpdate>,
    },
    Shutdown,
}

pub(crate) enum MonitorEvent {
    Refreshed {
        request_id: u64,
        result: Result<RefreshResult, String>,
    },
    Applied {
        request_id: u64,
        report: ApplyReport,
    },
}

pub(crate) struct MonitorWorker {
    commands: Sender<MonitorCommand>,
    events: Receiver<MonitorEvent>,
    thread: Option<JoinHandle<()>>,
}

impl MonitorWorker {
    pub(crate) fn new() -> Self {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut controller = MonitorController::new();

            while let Ok(command) = command_receiver.recv() {
                let event = match command {
                    MonitorCommand::Refresh { request_id } => MonitorEvent::Refreshed {
                        request_id,
                        result: controller.refresh().map_err(|error| error.to_string()),
                    },
                    MonitorCommand::Apply {
                        request_id,
                        updates,
                    } => MonitorEvent::Applied {
                        request_id,
                        report: controller.apply(updates),
                    },
                    MonitorCommand::Shutdown => break,
                };

                if event_sender.send(event).is_err() {
                    break;
                }
            }
        });

        Self {
            commands: command_sender,
            events: event_receiver,
            thread: Some(thread),
        }
    }

    pub(crate) fn refresh(&self, request_id: u64) -> Result<(), String> {
        self.commands
            .send(MonitorCommand::Refresh { request_id })
            .map_err(|error| error.to_string())
    }

    pub(crate) fn apply(
        &self,
        request_id: u64,
        updates: Vec<BrightnessUpdate>,
    ) -> Result<(), String> {
        self.commands
            .send(MonitorCommand::Apply {
                request_id,
                updates,
            })
            .map_err(|error| error.to_string())
    }

    pub(crate) fn try_recv(&self) -> Result<MonitorEvent, TryRecvError> {
        self.events.try_recv()
    }
}

impl Drop for MonitorWorker {
    fn drop(&mut self) {
        let _ = self.commands.send(MonitorCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
