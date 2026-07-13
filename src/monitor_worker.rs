use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use crate::monitor_hardware::{ApplyReport, BrightnessUpdate, MonitorController, RefreshResult};

enum MonitorCommand {
    Refresh {
        request_id: u64,
    },
    Apply {
        request_id: u64,
        updates: Vec<BrightnessUpdate>,
    },
    ApplyThenRefresh {
        request_id: u64,
        updates: Vec<BrightnessUpdate>,
    },
}

pub(crate) enum MonitorEvent {
    Started {
        request_id: u64,
    },
    Refreshed {
        request_id: u64,
        apply_report: Option<ApplyReport>,
        result: Result<RefreshResult, String>,
    },
    Applied {
        request_id: u64,
        report: ApplyReport,
    },
}

pub(crate) struct MonitorSendError {
    pub(crate) message: String,
    pub(crate) updates: Vec<BrightnessUpdate>,
}

pub(crate) struct MonitorWorker {
    commands: Sender<MonitorCommand>,
    events: Receiver<MonitorEvent>,
}

impl MonitorWorker {
    pub(crate) fn new() -> Self {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut controller = MonitorController::new();

            while let Ok(command) = command_receiver.recv() {
                let request_id = match &command {
                    MonitorCommand::Refresh { request_id }
                    | MonitorCommand::Apply { request_id, .. }
                    | MonitorCommand::ApplyThenRefresh { request_id, .. } => *request_id,
                };
                if event_sender
                    .send(MonitorEvent::Started { request_id })
                    .is_err()
                {
                    break;
                }

                let event = match command {
                    MonitorCommand::Refresh { request_id } => MonitorEvent::Refreshed {
                        request_id,
                        apply_report: None,
                        result: controller.refresh().map_err(|error| error.to_string()),
                    },
                    MonitorCommand::Apply {
                        request_id,
                        updates,
                    } => MonitorEvent::Applied {
                        request_id,
                        report: controller.apply(updates),
                    },
                    MonitorCommand::ApplyThenRefresh {
                        request_id,
                        updates,
                    } => MonitorEvent::Refreshed {
                        request_id,
                        apply_report: Some(controller.apply(updates)),
                        result: controller.refresh().map_err(|error| error.to_string()),
                    },
                };

                if event_sender.send(event).is_err() {
                    break;
                }
            }
        });

        Self {
            commands: command_sender,
            events: event_receiver,
        }
    }

    pub(crate) fn refresh(&self, request_id: u64) -> Result<(), MonitorSendError> {
        self.send(MonitorCommand::Refresh { request_id })
    }

    pub(crate) fn apply(
        &self,
        request_id: u64,
        updates: Vec<BrightnessUpdate>,
    ) -> Result<(), MonitorSendError> {
        self.send(MonitorCommand::Apply {
            request_id,
            updates,
        })
    }

    pub(crate) fn apply_then_refresh(
        &self,
        request_id: u64,
        updates: Vec<BrightnessUpdate>,
    ) -> Result<(), MonitorSendError> {
        self.send(MonitorCommand::ApplyThenRefresh {
            request_id,
            updates,
        })
    }

    pub(crate) fn try_recv(&self) -> Result<MonitorEvent, TryRecvError> {
        self.events.try_recv()
    }

    fn send(&self, command: MonitorCommand) -> Result<(), MonitorSendError> {
        self.commands.send(command).map_err(|error| {
            let message = error.to_string();
            let updates = match error.0 {
                MonitorCommand::Apply { updates, .. }
                | MonitorCommand::ApplyThenRefresh { updates, .. } => updates,
                MonitorCommand::Refresh { .. } => Vec::new(),
            };
            MonitorSendError { message, updates }
        })
    }
}
