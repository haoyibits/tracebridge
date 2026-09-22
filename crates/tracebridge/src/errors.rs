use std::fmt;

/// A user-facing failure; printed as `tracebridge: <message>` (errors.py).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeError(pub String);

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BridgeError {}

pub type Result<T> = std::result::Result<T, BridgeError>;

/// Build a `BridgeError` from a format string.
#[macro_export]
macro_rules! bridge_error {
    ($($arg:tt)*) => {
        $crate::errors::BridgeError(format!($($arg)*))
    };
}

/// Return early with a `BridgeError`.
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::bridge_error!($($arg)*))
    };
}
