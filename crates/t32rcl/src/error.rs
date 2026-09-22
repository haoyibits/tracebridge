// Error handling ported from lauterbach-trace32-rcl 1.1.5 (MIT, Copyright (c) 2020
// Lauterbach GmbH): _rc/_error.py, _rc/_memory_exceptions.py and the error table
// at the end of _rc/_library.py.

use std::fmt;
use std::io;
use std::time::Duration;

/// Error code TRACE32 uses when a command, function or object access fails
/// (`T32_ERR_FN1`).
pub const T32_ERR_FN1: u8 = 90;

/// Every failure of the RCL client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The TCP connection could not be established, broke, or was closed.
    #[error("{0}")]
    Connect(String),

    /// No answer arrived within the socket timeout.
    #[error("timed out")]
    Timeout,

    /// A PRACTICE script did not return within the time given to `cmm`.
    #[error("PRACTICE script did not finish within {0:?}")]
    ScriptTimeout(Duration),

    /// TRACE32 answered with an error.
    #[error("{0}")]
    Trace32(Trace32Error),

    /// The peer violated the RCL protocol or sent data this client cannot decode.
    #[error("{0}")]
    Protocol(String),
}

impl Error {
    pub fn is_timeout(&self) -> bool {
        matches!(self, Error::Timeout | Error::ScriptTimeout(_))
    }

    pub(crate) fn from_io(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Error::Timeout,
            _ => Error::Connect(error.to_string()),
        }
    }
}

/// What the client was doing when TRACE32 reported an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Any API call without special handling in the Python library.
    Api,
    /// `T32_ExecuteCommand` failed (Python: `CommandError`).
    Command(String),
    /// `T32_ExecuteFunction` failed (Python: `FunctionError`).
    Function(String),
    /// Memory read failed (Python: `MemoryReadAccessError` / `MemoryAccessError`).
    MemoryRead,
    /// Memory write failed (Python: `MemoryWriteAccessError` / `MemoryAccessError`).
    MemoryWrite,
    /// `cmm` failed (Python: `PracticeError`).
    Practice,
    /// PowerView is older than the RCL protocol requires (Python: `ApiVersionError`).
    Version,
    /// A symbol query returned no usable result.
    Symbol,
}

/// An error reported by TRACE32 or derived from its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trace32Error {
    /// The TRACE32 error code, when the answer carried one.
    pub code: Option<u8>,
    pub operation: Operation,
    pub message: String,
}

impl Trace32Error {
    pub(crate) fn new(operation: Operation, message: impl Into<String>) -> Self {
        Trace32Error {
            code: None,
            operation,
            message: message.into(),
        }
    }

    /// Build the error for an error code in an answer, falling back to the
    /// library's default message when the answer did not carry one.
    pub(crate) fn from_code(code: u8, message: Option<String>) -> Self {
        let message = match message {
            Some(message) => message,
            None => match default_message(code) {
                Some(text) => text.to_string(),
                None => format!("TRACE32 error {code}"),
            },
        };
        Trace32Error {
            code: Some(code),
            operation: Operation::Api,
            message,
        }
    }

    pub(crate) fn with_operation(mut self, operation: Operation) -> Self {
        self.operation = operation;
        self
    }
}

impl fmt::Display for Trace32Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.operation {
            Operation::Command(command) if self.message.is_empty() => {
                write!(f, "command failed (command: {command})")
            }
            Operation::Command(command) => write!(f, "{} (command: {command})", self.message),
            Operation::Function(expression) if self.message.is_empty() => {
                write!(f, "function failed: {expression}")
            }
            Operation::MemoryRead | Operation::MemoryWrite => {
                let verb = if self.operation == Operation::MemoryRead {
                    "read"
                } else {
                    "write"
                };
                if self.message.is_empty() {
                    write!(f, "memory {verb} failed")
                } else {
                    write!(f, "memory {verb} failed: {}", self.message)
                }
            }
            Operation::Api if self.message.is_empty() => match self.code {
                Some(code) => write!(f, "TRACE32 error {code}"),
                None => f.write_str("TRACE32 error"),
            },
            _ => f.write_str(&self.message),
        }
    }
}

impl From<Trace32Error> for Error {
    fn from(error: Trace32Error) -> Self {
        Error::Trace32(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Default messages of `error_code_exception_mapping` for the codes that fit
/// in the one-byte status field of an answer.
fn default_message(code: u8) -> Option<&'static str> {
    Some(match code {
        2 => "target running",
        3 => "target not running",
        4 => "target is in reset",
        6 => "access timeout, target running",
        10 => "not implemented",
        14 => "registerset undefined",
        15 => "verify error",
        16 => "bus error",
        22 => "no memory mapped",
        48 => "target reset detected",
        49 => "FDX buffer error",
        57 => "no RTCK detected",
        60 => "no valid license detected",
        64 => "core has no clock/power/reset in SMP",
        67 => "user signal",
        83 => "tried to connect to emu",
        90..=93 => "",
        113 => "113 std failed",
        123 => "access locked",
        128 => "power fail",
        140 => "debug port fail",
        144 => "debug port timeout",
        147 => "no debug device",
        161 => "target reset fail",
        162 => "emulator communication timeout",
        164 => "no RTCK on emulator",
        254 => "T32_Attach() is missing",
        255 => "FATAL ERROR 255",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_code_without_message_uses_table_text() {
        let error = Trace32Error::from_code(16, None);
        assert_eq!(error.to_string(), "bus error");
    }

    #[test]
    fn unknown_code_is_reported_by_number() {
        let error = Trace32Error::from_code(200, None);
        assert_eq!(error.to_string(), "TRACE32 error 200");
    }

    #[test]
    fn command_error_names_the_command() {
        let error = Trace32Error::from_code(T32_ERR_FN1, Some("syntax error".into()))
            .with_operation(Operation::Command("Go".into()));
        assert_eq!(error.to_string(), "syntax error (command: Go)");
    }
}
