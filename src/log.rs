//! Diagnostics. Release builds have no console, so messages also go to the
//! debugger output, where a tool such as Sysinternals DebugView shows them.
//! Nothing is written to disk, and nothing is kept when no one is listening.

/// Logs a formatted message, like `eprintln!`.
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::write(&format!($($arg)*))
    };
}

pub(crate) use log;

pub(crate) fn write(message: &str) {
    eprintln!("{message}");
    crate::windows_integration::debug_output(&format!("MMB: {message}\n"));
}
