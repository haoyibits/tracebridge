//! Pure Rust client for the Lauterbach TRACE32 Remote API (RCL) over TCP
//! (`RCL=NETTCP`).
//!
//! This is a port of the subset of `lauterbach-trace32-rcl` 1.1.5 (MIT,
//! Copyright (c) 2020 Lauterbach GmbH) that tracebridge needs: `rcl.py`
//! (`Debugger`), `_rc/hlinknet.py`, `_rc/_library.py`, `_rc/_address.py`,
//! `_rc/_symbol.py` and the used parts of `_rc/_memory.py` and
//! `_rc/_functions.py`. Every request is byte-for-byte what the Python library
//! sends; see CLAUDE.md for the message inventory.

mod address;
mod api;
mod error;
mod eval;
mod link;
mod symbol;

use std::thread;
use std::time::{Duration, Instant};

pub use address::Address;
pub use error::{Error, Operation, Result, T32_ERR_FN1, Trace32Error};
pub use eval::{Value, parse_int};
pub use symbol::Symbol;

use link::{Link, MessageId};

/// Version string sent in `VERSION.PYRCL(...)`, identical to the Python library.
pub const PYRCL_VERSION: &str = "1.1.5";
/// `MIN_BASE_POWERVIEW` in rcl.py.
pub const MIN_BASE_POWERVIEW: i128 = 125398;
/// `MIN_BUILD_POWERVIEW` in rcl.py.
pub const MIN_BUILD_POWERVIEW: i128 = 126615;

/// How long `cmm` sleeps between two `PRACTICE.SD()` polls.
const CMM_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// A connection to TRACE32 PowerView (`lauterbach.trace32.rcl.Debugger`).
#[derive(Debug)]
pub struct Debugger {
    link: Link,
    message_id: MessageId,
    node: String,
    port: u16,
    timeout: Duration,
}

impl Debugger {
    /// `rcl.connect(node=..., port=..., protocol="TCP", packlen=1024, timeout=...)`.
    ///
    /// Connects, attaches (retrying once after a timeout on a new connection, as
    /// `Debugger.connect` does) and checks the PowerView version. `timeout`
    /// applies to the connection attempt and to every later receive; zero means
    /// no timeout.
    pub fn connect(node: &str, port: u16, timeout: Duration) -> Result<Debugger> {
        let mut debugger = Debugger {
            link: Link::connect(node, port, timeout)?,
            message_id: MessageId::default(),
            node: node.to_string(),
            port,
            timeout,
        };
        match debugger.call(api::RAPI_CMD_ATTACH, 1, &[], None) {
            Err(Error::Timeout) => {
                debugger.link = Link::connect(node, port, timeout)?;
                debugger.call(api::RAPI_CMD_ATTACH, 1, &[], None)?;
            }
            other => {
                other?;
            }
        }
        debugger.check_powerview_version()?;
        Ok(debugger)
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Change the receive timeout for later requests (zero means none).
    pub fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        self.link.set_timeout(timeout)?;
        self.timeout = timeout;
        Ok(())
    }

    /// `Debugger.check_powerview_version`.
    fn check_powerview_version(&mut self) -> Result<()> {
        let build = self.fnc_int("SOFTWARE.BUILD()")?;
        let base = self.fnc_int("SOFTWARE.BUILD.BASE()")?;
        if base < MIN_BASE_POWERVIEW || build < MIN_BUILD_POWERVIEW {
            return Err(Trace32Error::new(
                Operation::Version,
                format!(
                    "Minimum required software version: {MIN_BUILD_POWERVIEW}:{MIN_BASE_POWERVIEW}, \
                     current version {build}:{base} (build:base)"
                ),
            )
            .into());
        }
        // TRACE32 checks the client version; the Python library ignores the result.
        self.fnc(&format!("VERSION.PYRCL({PYRCL_VERSION})"))?;
        Ok(())
    }

    /// Send one request and return the answer payload after the status and id.
    fn call(
        &mut self,
        rapi_cmd: u8,
        opt_arg: u8,
        payload: &[u8],
        force_length: Option<usize>,
    ) -> Result<Vec<u8>> {
        let id = self.message_id.next();
        let data = api::encode_request(rapi_cmd, opt_arg, id, payload, force_length)?;
        self.link.transmit(&data)?;
        let answer = self.link.receive(self.message_id.current())?;
        Ok(api::check_response(&answer)?.to_vec())
    }

    /// `Debugger.cmd`: execute a TRACE32 command (`T32_ExecuteCommand`).
    pub fn cmd(&mut self, command: &str) -> Result<()> {
        let payload = api::practice_payload(command);
        self.call(
            api::RAPI_CMD_EXECUTE_PRACTICE,
            api::EXECUTE_COMMAND,
            &payload,
            None,
        )
        .map_err(|error| match error {
            Error::Trace32(error) if error.code == Some(T32_ERR_FN1) => error
                .with_operation(Operation::Command(command.into()))
                .into(),
            other => other,
        })?;
        Ok(())
    }

    /// `Debugger.print`: `ECHO "<text>"` in the PowerView message line and AREA.
    pub fn print(&mut self, text: &str) -> Result<()> {
        self.cmd(&format!("ECHO \"{text}\""))
    }

    /// `Debugger.fnc`: evaluate a PRACTICE function (`T32_ExecuteFunction`).
    pub fn fnc(&mut self, expression: &str) -> Result<Value> {
        let payload = api::practice_payload(expression);
        let result = self
            .call(
                api::RAPI_CMD_EXECUTE_PRACTICE,
                api::EXECUTE_FUNCTION,
                &payload,
                None,
            )
            .map_err(|error| match error {
                Error::Trace32(error) if error.code == Some(T32_ERR_FN1) => error
                    .with_operation(Operation::Function(expression.into()))
                    .into(),
                other => other,
            })?;
        let (result_type, text) = eval::split_result(&result)?;
        eval::decode(result_type, &text, expression)
    }

    fn fnc_int(&mut self, expression: &str) -> Result<i128> {
        match self.fnc(expression)? {
            Value::Int(value) => Ok(value),
            other => Err(Error::Protocol(format!(
                "{expression} returned {other:?}, expected a number"
            ))),
        }
    }

    fn fnc_bool(&mut self, expression: &str) -> Result<bool> {
        match self.fnc(expression)? {
            Value::Bool(value) => Ok(value),
            other => Err(Error::Protocol(format!(
                "{expression} returned {other:?}, expected a boolean"
            ))),
        }
    }

    /// `fnc.system_up()`: `SYStem.Up()`.
    pub fn system_up(&mut self) -> Result<bool> {
        self.fnc_bool("SYStem.Up()")
    }

    /// `fnc.state_run()`: `STATE.RUN()`.
    pub fn state_run(&mut self) -> Result<bool> {
        self.fnc_bool("STATE.RUN()")
    }

    /// `Debugger.cmm`: run `DO <script>` and wait until the PRACTICE stack
    /// returns to its previous depth. `None` waits forever; a zero duration
    /// does not wait at all.
    pub fn cmm(&mut self, script: &str, timeout: Option<Duration>) -> Result<()> {
        let depth_before = self.fnc_int("PRACTICE.SD()")?;
        let start = Instant::now();
        self.cmd(&format!("DO {script}"))
            .map_err(|error| match error {
                Error::Trace32(error) if matches!(error.operation, Operation::Command(_)) => {
                    error.with_operation(Operation::Practice).into()
                }
                other => other,
            })?;
        if timeout.is_some_and(|limit| limit.is_zero()) {
            return Ok(());
        }
        loop {
            let depth = self.fnc_int("PRACTICE.SD()")?;
            if depth < depth_before {
                return Err(
                    Trace32Error::new(Operation::Practice, "Practice stack depth error").into(),
                );
            }
            if depth == depth_before {
                return Ok(());
            }
            if let Some(limit) = timeout {
                if start.elapsed() > limit {
                    return Err(Error::ScriptTimeout(limit));
                }
            }
            thread::sleep(CMM_POLL_INTERVAL);
        }
    }

    /// `memory.read(address, length=n)`.
    pub fn memory_read(&mut self, address: &Address, length: usize) -> Result<Vec<u8>> {
        let mut result = Vec::with_capacity(length);
        while result.len() < length {
            let chunk = (length - result.len()).min(api::READ_CHUNK_SIZE);
            let (payload, force) = api::memory_read_payload(address, result.len() as u64, chunk);
            let data = self
                .call(
                    api::RAPI_CMD_DEVICE_SPECIFIC,
                    api::RAPI_DSCMD_MEMORY_OBJ_READ,
                    &payload,
                    Some(force),
                )
                .map_err(|error| memory_error(error, Operation::MemoryRead))?;
            if data.len() < chunk {
                return Err(Error::Protocol(format!(
                    "memory read returned {} of {chunk} bytes",
                    data.len()
                )));
            }
            result.extend_from_slice(&data[..chunk]);
        }
        Ok(result)
    }

    /// `memory.write(address, data)`.
    pub fn memory_write(&mut self, address: &Address, data: &[u8]) -> Result<()> {
        self.write_memory(address, data, None)
    }

    /// `memory.write_uint32(address, value)`: little-endian, access width 4.
    pub fn memory_write_u32(&mut self, address: &Address, value: u32) -> Result<()> {
        self.write_memory(address, &value.to_le_bytes(), Some(4))
    }

    fn write_memory(&mut self, address: &Address, data: &[u8], width: Option<u16>) -> Result<()> {
        for (index, chunk) in data.chunks(api::WRITE_CHUNK_SIZE).enumerate() {
            let offset = (index * api::WRITE_CHUNK_SIZE) as u64;
            let (payload, force) = api::memory_write_payload(address, offset, chunk, width);
            self.call(
                api::RAPI_CMD_DEVICE_SPECIFIC,
                api::RAPI_DSCMD_MEMORY_OBJ_WRITE,
                &payload,
                Some(force),
            )
            .map_err(|error| memory_error(error, Operation::MemoryWrite))?;
        }
        Ok(())
    }

    /// `symbol.query_by_name(name=...)`.
    pub fn symbol_query_by_name(&mut self, name: &str) -> Result<Symbol> {
        let payload = api::symbol_query_payload(&symbol::serialize_name_query(name));
        let answer = self.call(
            api::RAPI_CMD_DEVICE_SPECIFIC,
            api::RAPI_DSCMD_SYMBOL_QUERYOBJ,
            &payload,
            None,
        )?;
        symbol::deserialize(&answer)
    }

    /// `symbol.query_by_name(name).address.value`.
    pub fn symbol_address(&mut self, name: &str) -> Result<u64> {
        match self.symbol_query_by_name(name)?.address {
            Some(address) => Ok(address.value),
            None => Err(Trace32Error::new(
                Operation::Symbol,
                format!("symbol {name} has no address"),
            )
            .into()),
        }
    }

    /// `Debugger.disconnect`: close the socket; no message is sent.
    pub fn disconnect(self) {}
}

/// The Python memory service turns every TRACE32 error into a memory access
/// error; connection problems pass through unchanged.
fn memory_error(error: Error, operation: Operation) -> Error {
    match error {
        Error::Trace32(error) => {
            let message = if error.code == Some(T32_ERR_FN1) {
                "wrong parameters".to_string()
            } else {
                error.to_string()
            };
            Trace32Error {
                code: error.code,
                operation,
                message,
            }
            .into()
        }
        other => other,
    }
}
