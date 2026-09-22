//! RCL connections (remote.py).

use std::time::Duration;

use t32rcl::Debugger;

use crate::bridge_error;
use crate::config::Config;
use crate::errors::Result;

/// Default connection timeout of `connect_debugger`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The debugger operations the target sequences use; implemented by
/// `t32rcl::Debugger` and by recording test doubles.
pub trait Rcl {
    fn cmd(&mut self, command: &str) -> t32rcl::Result<()>;
    fn cmm(&mut self, script: &str, timeout: Option<Duration>) -> t32rcl::Result<()>;
    fn print(&mut self, text: &str) -> t32rcl::Result<()>;
    fn system_up(&mut self) -> t32rcl::Result<bool>;
    fn state_run(&mut self) -> t32rcl::Result<bool>;
}

impl Rcl for Debugger {
    fn cmd(&mut self, command: &str) -> t32rcl::Result<()> {
        Debugger::cmd(self, command)
    }
    fn cmm(&mut self, script: &str, timeout: Option<Duration>) -> t32rcl::Result<()> {
        Debugger::cmm(self, script, timeout)
    }
    fn print(&mut self, text: &str) -> t32rcl::Result<()> {
        Debugger::print(self, text)
    }
    fn system_up(&mut self) -> t32rcl::Result<bool> {
        Debugger::system_up(self)
    }
    fn state_run(&mut self) -> t32rcl::Result<bool> {
        Debugger::state_run(self)
    }
}

/// `connect_debugger`: connect to PowerView on the configured RCL port.
pub fn connect_debugger(config: &Config, timeout: Duration) -> Result<Debugger> {
    Debugger::connect("localhost", config.rcl_port, timeout).map_err(|error| {
        bridge_error!(
            "cannot connect to TRACE32 on RCL port {}: {error}",
            config.rcl_port
        )
    })
}

/// `reset_and_stop`: break and reset the target into `SYStem.Mode Up`.
pub fn reset_and_stop(config: &Config) -> Result<()> {
    let mut debugger = connect_debugger(config, CONNECT_TIMEOUT)?;
    reset_and_stop_with(&mut debugger)
}

pub fn reset_and_stop_with(debugger: &mut impl Rcl) -> Result<()> {
    debugger
        .cmd("Break")
        .and_then(|()| debugger.cmd("SYStem.Mode Up"))
        .map_err(|error| bridge_error!("TRACE32 reset failed: {error}"))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Records every call; `fail_on` makes commands with that prefix fail.
    #[derive(Default)]
    pub struct Recorder {
        pub calls: Vec<String>,
        pub system_up: bool,
        pub state_run: bool,
        pub fail_on: Option<String>,
    }

    impl Recorder {
        pub fn commands(&self) -> Vec<&str> {
            self.calls
                .iter()
                .filter_map(|call| call.strip_prefix("cmd "))
                .collect()
        }
    }

    fn failure(text: &str) -> t32rcl::Error {
        t32rcl::Error::Protocol(format!("{text} failed"))
    }

    impl Rcl for Recorder {
        fn cmd(&mut self, command: &str) -> t32rcl::Result<()> {
            self.calls.push(format!("cmd {command}"));
            match &self.fail_on {
                Some(prefix) if command.starts_with(prefix.as_str()) => Err(failure(command)),
                _ => Ok(()),
            }
        }
        fn cmm(&mut self, script: &str, timeout: Option<Duration>) -> t32rcl::Result<()> {
            self.calls.push(format!("cmm {script} {timeout:?}"));
            Ok(())
        }
        fn print(&mut self, text: &str) -> t32rcl::Result<()> {
            self.calls.push(format!("print {text}"));
            Ok(())
        }
        fn system_up(&mut self) -> t32rcl::Result<bool> {
            self.calls.push("system_up".into());
            Ok(self.system_up)
        }
        fn state_run(&mut self) -> t32rcl::Result<bool> {
            self.calls.push("state_run".into());
            Ok(self.state_run)
        }
    }

    // test_remote.py: test_reset_breaks_then_resets_to_up
    #[test]
    fn reset_breaks_then_resets_to_up() {
        let mut recorder = Recorder::default();
        reset_and_stop_with(&mut recorder).unwrap();
        assert_eq!(recorder.commands(), ["Break", "SYStem.Mode Up"]);
    }

    #[test]
    fn reset_failure_is_reported() {
        let mut recorder = Recorder {
            fail_on: Some("Break".into()),
            ..Default::default()
        };
        let error = reset_and_stop_with(&mut recorder).unwrap_err();
        assert_eq!(error.0, "TRACE32 reset failed: Break failed");
    }
}
