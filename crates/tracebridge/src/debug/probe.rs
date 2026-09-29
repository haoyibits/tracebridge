//! What the debug commands need from PowerView, behind a trait so that the
//! decoders and the check logic run against a fake in tests.
//!
//! Everything is read through PowerView-side evaluation of PRACTICE functions,
//! so addresses and access classes mean what they mean on the PowerView
//! command line. The raw RCL memory API is used only for bulk reads of plain
//! memory (`verify`): it addresses coprocessor registers differently (a CP15
//! per-file address must be multiplied by 4), which once led to reading the
//! wrong register.

use std::fmt;

use t32rcl::{Address, Debugger, Value};

/// The RCL operations of a debug session.
pub trait Probe {
    /// Evaluate a PRACTICE function (`T32_ExecuteFunction`).
    fn fnc(&mut self, expression: &str) -> t32rcl::Result<Value>;
    /// Execute a PRACTICE command (`T32_ExecuteCommand`).
    fn cmd(&mut self, command: &str) -> t32rcl::Result<()>;
    /// Bulk memory read through the RCL memory API.
    fn read_memory(&mut self, address: &Address, length: usize) -> t32rcl::Result<Vec<u8>>;
}

impl Probe for Debugger {
    fn fnc(&mut self, expression: &str) -> t32rcl::Result<Value> {
        Debugger::fnc(self, expression)
    }
    fn cmd(&mut self, command: &str) -> t32rcl::Result<()> {
        Debugger::cmd(self, command)
    }
    fn read_memory(&mut self, address: &Address, length: usize) -> t32rcl::Result<Vec<u8>> {
        Debugger::memory_read(self, address, length)
    }
}

/// A failed debug command. `lost` means the RCL connection is gone and the
/// session needs `reconnect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugError {
    pub message: String,
    pub lost: bool,
}

impl DebugError {
    pub fn new(message: impl Into<String>) -> Self {
        DebugError {
            message: message.into(),
            lost: false,
        }
    }

    /// Put `context: ` in front of the message.
    pub fn context(mut self, context: impl fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }
}

impl fmt::Display for DebugError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<t32rcl::Error> for DebugError {
    fn from(error: t32rcl::Error) -> Self {
        // A broken, closed or out-of-sync link; timeouts and undecodable
        // results leave the connection usable.
        let lost = matches!(error, t32rcl::Error::Connect(_));
        DebugError {
            message: error.to_string(),
            lost,
        }
    }
}

pub type DResult<T> = std::result::Result<T, DebugError>;

/// Return early with a `DebugError`.
macro_rules! fail {
    ($($arg:tt)*) => {
        return Err($crate::debug::probe::DebugError::new(format!($($arg)*)))
    };
}
pub(crate) use fail;

/// A PRACTICE string literal: quotes are doubled.
pub fn practice_string(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

/// A value as an unsigned number: integers, booleans (1/0) and address
/// strings such as `SD:0x20000000`.
pub fn value_as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Int(value) => Some(*value as u64),
        Value::Bool(value) => Some(u64::from(*value)),
        Value::Text(text) => TargetAddress::parse(text).map(|address| address.value),
        _ => None,
    }
}

/// A value for display.
pub fn format_value(value: &Value) -> String {
    match value {
        Value::Int(value) if (0..=0xFFFF_FFFF).contains(value) => format!("0x{value:08X}"),
        Value::Int(value) if *value >= 0 => format!("0x{value:X}"),
        Value::Int(value) => value.to_string(),
        Value::Bool(value) => if *value { "TRUE" } else { "FALSE" }.to_string(),
        Value::Float(value) => value.to_string(),
        Value::Text(text) => text.clone(),
        Value::TimeRange(values) => format!("{values:?}"),
        Value::Empty => "(empty)".to_string(),
    }
}

pub fn value_to_json(value: &Value) -> serde_json::Value {
    use serde_json::json;
    match value {
        Value::Int(value) => {
            if (0..=0xFFFF_FFFF).contains(value) {
                json!({"type": "int", "value": *value as u64, "hex": format!("0x{value:08X}")})
            } else if *value >= 0 {
                json!({"type": "int", "value": *value as u64, "hex": format!("0x{value:X}")})
            } else {
                json!({"type": "int", "value": *value as i64})
            }
        }
        Value::Bool(value) => json!({"type": "bool", "value": value}),
        Value::Float(value) => json!({"type": "float", "value": value}),
        Value::Text(text) => json!({"type": "text", "value": text}),
        Value::TimeRange(values) => json!({"type": "time_range", "value": values}),
        Value::Empty => json!({"type": "empty", "value": null}),
    }
}

pub fn hex32(value: u64) -> String {
    if value <= 0xFFFF_FFFF {
        format!("0x{value:08X}")
    } else {
        format!("0x{value:X}")
    }
}

pub fn eval(probe: &mut dyn Probe, expression: &str) -> DResult<Value> {
    Ok(probe.fnc(expression)?)
}

pub fn eval_u64(probe: &mut dyn Probe, expression: &str) -> DResult<u64> {
    let value = eval(probe, expression)?;
    value_as_u64(&value).ok_or_else(|| {
        DebugError::new(format!(
            "{expression} returned {}, not a number",
            format_value(&value)
        ))
    })
}

pub fn eval_bool(probe: &mut dyn Probe, expression: &str) -> DResult<bool> {
    match eval(probe, expression)? {
        Value::Bool(value) => Ok(value),
        Value::Int(value) => Ok(value != 0),
        other => Err(DebugError::new(format!(
            "{expression} returned {}, not a boolean",
            format_value(&other)
        ))),
    }
}

pub fn eval_text(probe: &mut dyn Probe, expression: &str) -> DResult<String> {
    Ok(match eval(probe, expression)? {
        Value::Text(text) => text,
        Value::Empty => String::new(),
        other => format_value(&other),
    })
}

/// A classified address as TRACE32 prints it: `C15:0x4025`, `AD:0x70F40000`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetAddress {
    /// Access class; empty when there is none.
    pub class: String,
    pub value: u64,
}

impl TargetAddress {
    /// `<class>:<number>` with a `0x` hexadecimal or decimal number (TRACE32
    /// also marks decimals with a trailing dot). The class is letters and
    /// digits only, so register names and PER paths never parse.
    pub fn parse(text: &str) -> Option<TargetAddress> {
        let text = text.trim();
        let (class, number) = text.rsplit_once(':')?;
        if !class.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let value = if let Some(hex) = number
            .strip_prefix("0x")
            .or_else(|| number.strip_prefix("0X"))
        {
            u64::from_str_radix(hex, 16).ok()?
        } else {
            let digits = number.strip_suffix('.').unwrap_or(number);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits.parse().ok()?
        };
        Some(TargetAddress {
            class: class.to_string(),
            value,
        })
    }

    /// Coprocessor registers (`C15:`, `C14:` and their variants) are core
    /// registers: they can only be read while the core is halted.
    pub fn is_coprocessor(&self) -> bool {
        let class = self.class.to_ascii_uppercase();
        class.ends_with("C15") || class.ends_with("C14")
    }

    /// The same class at `value + offset`, for PRACTICE expressions.
    pub fn offset(&self, offset: u64) -> TargetAddress {
        TargetAddress {
            class: self.class.clone(),
            value: self.value.wrapping_add(offset),
        }
    }
}

impl fmt::Display for TargetAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.class.is_empty() {
            write!(f, "0x{:X}", self.value)
        } else {
            write!(f, "{}:0x{:X}", self.class, self.value)
        }
    }
}

/// Evaluate an address-valued function, e.g. `PER.ADDRESS(...)` or
/// `sYmbol.BEGIN(...)`.
pub fn eval_address(probe: &mut dyn Probe, expression: &str) -> DResult<TargetAddress> {
    let value = eval(probe, expression)?;
    if let Value::Text(text) = &value {
        if let Some(address) = TargetAddress::parse(text) {
            return Ok(address);
        }
    }
    // Unknown formatting: ask TRACE32 for the plain offset.
    let offset = eval_u64(probe, &format!("ADDRESS.OFFSET({expression})"))?;
    let class = match &value {
        Value::Text(text) => text
            .rsplit_once(':')
            .map(|(class, _)| class.trim().to_string())
            .unwrap_or_default(),
        _ => String::new(),
    };
    Ok(TargetAddress {
        class,
        value: offset,
    })
}

/// Debugger mode and run state, for the prompt and for staleness checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebuggerState {
    pub mode: u64,
    /// `None` when STATE.RUN() is not available (e.g. system down).
    pub running: Option<bool>,
}

impl DebuggerState {
    pub fn read(probe: &mut dyn Probe) -> DResult<DebuggerState> {
        let mode = eval_u64(probe, "SYStem.Mode()")?;
        let running = match probe.fnc("STATE.RUN()") {
            Ok(Value::Bool(value)) => Some(value),
            Ok(_) => None,
            Err(error) => {
                let error = DebugError::from(error);
                if error.lost {
                    return Err(error);
                }
                None
            }
        };
        Ok(DebuggerState { mode, running })
    }

    pub fn is_up(&self) -> bool {
        matches!(self.mode, 11 | 12)
    }

    /// `up, halted`, `up, running`, `down`.
    pub fn label(&self) -> String {
        let mode = super::decode::system_mode_label(self.mode);
        match (self.mode, self.running) {
            (0, _) | (_, None) => mode,
            (_, Some(true)) => format!("{mode}, running"),
            (_, Some(false)) => format!("{mode}, halted"),
        }
    }
}

/// The `PER.Set.CONDitions` snapshot. The PER functions cannot evaluate the
/// IF conditions of a PER file (a per file may wrap all core registers in
/// one); the command snapshots them. It is taken before the first PER
/// function and again when the debugger mode or run state has changed since,
/// after a state-changing command, and on every use while the core runs.
#[derive(Debug, Default)]
pub struct PerSnapshot {
    taken: Option<DebuggerState>,
}

impl PerSnapshot {
    pub fn invalidate(&mut self) {
        self.taken = None;
    }

    pub fn ensure(&mut self, probe: &mut dyn Probe) -> DResult<()> {
        let state = DebuggerState::read(probe)?;
        if self.taken == Some(state) && state.running != Some(true) {
            return Ok(());
        }
        if let Err(error) = probe.cmd("PER.Set.CONDitions") {
            let error = DebugError::from(error);
            if error.lost {
                return Err(error);
            }
            eprintln!(
                "tracebridge: warning: PER.Set.CONDitions failed ({error}); registers inside \
                 IF conditions of the PER file may not resolve"
            );
        }
        self.taken = Some(state);
        Ok(())
    }
}

/// `true` when the core is running. Errors (e.g. system down) count as not
/// running; the read that follows reports the real problem.
pub fn core_running(probe: &mut dyn Probe) -> DResult<bool> {
    match probe.fnc("STATE.RUN()") {
        Ok(Value::Bool(value)) => Ok(value),
        Ok(_) => Ok(false),
        Err(error) => {
            let error = DebugError::from(error);
            if error.lost { Err(error) } else { Ok(false) }
        }
    }
}

pub const HALT_HINT: &str = "halt it with 'tracebridge debug break' first";

/// The error for a core-register read while the core runs.
pub fn running_error(what: &str) -> DebugError {
    DebugError::new(format!(
        "{what} can only be read while the core is halted, and the core is running; {HALT_HINT}"
    ))
}

/// `sym+0x12` for an address in code, or `None` when TRACE32 knows no symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRef {
    /// TRACE32's symbol path, e.g. `\\app\Global\main`.
    pub path: String,
    pub offset: Option<u64>,
}

impl SymbolRef {
    /// The last element of the symbol path.
    pub fn short_name(&self) -> &str {
        self.path
            .rsplit('\\')
            .find(|part| !part.is_empty())
            .unwrap_or(&self.path)
    }

    pub fn describe(&self) -> String {
        match self.offset {
            Some(0) | None => self.short_name().to_string(),
            Some(offset) => format!("{}+0x{offset:X}", self.short_name()),
        }
    }
}

/// Map a code address to its symbol with `sYmbol.NAME()` and the offset from
/// `sYmbol.BEGIN()` of that symbol. Failures mean "no symbol".
pub fn symbolize(probe: &mut dyn Probe, address: u64) -> DResult<Option<SymbolRef>> {
    let name = match probe.fnc(&format!("sYmbol.NAME(P:0x{address:X})")) {
        Ok(Value::Text(text)) => text.trim().to_string(),
        Ok(_) => return Ok(None),
        Err(error) => {
            let error = DebugError::from(error);
            if error.lost {
                return Err(error);
            }
            return Ok(None);
        }
    };
    // No symbol: empty, or the address printed back.
    if name.is_empty() || TargetAddress::parse(&name).is_some() {
        return Ok(None);
    }
    if name.contains('+') {
        return Ok(Some(SymbolRef {
            path: name,
            offset: None,
        }));
    }
    let offset = match probe.fnc(&format!("ADDRESS.OFFSET(sYmbol.BEGIN({name}))")) {
        Ok(value) => value_as_u64(&value).and_then(|begin| address.checked_sub(begin)),
        Err(error) => {
            let error = DebugError::from(error);
            if error.lost {
                return Err(error);
            }
            None
        }
    };
    Ok(Some(SymbolRef { path: name, offset }))
}

/// The address range of a symbol (`sYmbol.BEGIN` .. `sYmbol.END`, inclusive).
pub fn symbol_range(probe: &mut dyn Probe, name: &str) -> DResult<(TargetAddress, u64)> {
    let begin = eval_address(probe, &format!("sYmbol.BEGIN({name})"))
        .map_err(|error| symbol_error(error, name))?;
    let end = eval_address(probe, &format!("sYmbol.END({name})"))
        .map_err(|error| symbol_error(error, name))?;
    Ok((begin, end.value))
}

/// The first address of a symbol.
pub fn symbol_address(probe: &mut dyn Probe, name: &str) -> DResult<TargetAddress> {
    eval_address(probe, &format!("sYmbol.BEGIN({name})")).map_err(|error| symbol_error(error, name))
}

fn symbol_error(error: DebugError, name: &str) -> DebugError {
    if error.lost {
        error
    } else {
        DebugError::new(format!("symbol {name} not found ({})", error.message))
    }
}

/// A register found in the PER file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerRegister {
    /// The path that resolved, as passed to the PER functions.
    pub path: String,
    pub address: TargetAddress,
}

/// The PER paths to try for a user-supplied name: `.NAME` searches the whole
/// PER file; a name with dots may be `REG.FIELD` or a full path.
pub fn per_candidates(name: &str) -> Vec<String> {
    let name = name.trim();
    if name.starts_with('.') {
        vec![name.to_string()]
    } else if name.starts_with('"') {
        vec![name.to_string(), format!(".{name}")]
    } else if name.contains('.') {
        vec![format!(".{name}"), name.to_string()]
    } else {
        vec![format!(".{name}")]
    }
}

/// True for a path with more than one element (a field or a full path).
pub fn has_several_elements(path: &str) -> bool {
    let mut quoted = false;
    let mut dots = 0;
    for (index, c) in path.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '.' if !quoted && index > 0 => dots += 1,
            _ => {}
        }
    }
    dots > 0
}

/// Resolve a register name with `PER.ADDRESS()`. The error says "not found"
/// or "ambiguous" with TRACE32's own message.
pub fn resolve_register(probe: &mut dyn Probe, name: &str) -> DResult<PerRegister> {
    let mut first_error: Option<DebugError> = None;
    for path in per_candidates(name) {
        match eval_address(probe, &format!("PER.ADDRESS({})", practice_string(&path))) {
            Ok(address) => return Ok(PerRegister { path, address }),
            Err(error) if error.lost => return Err(error),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    let message = first_error.map(|e| e.message).unwrap_or_default();
    let kind = if message.to_ascii_lowercase().contains("ambig") {
        "ambiguous"
    } else {
        "not found"
    };
    let detail = if message.is_empty() {
        String::new()
    } else {
        format!(" (TRACE32: {message})")
    };
    Err(DebugError::new(format!(
        "{name}: {kind} in the PER file{detail}"
    )))
}

/// The value of a resolved register (`PER.VALUE`).
pub fn read_register(probe: &mut dyn Probe, register: &PerRegister) -> DResult<u64> {
    eval_u64(
        probe,
        &format!("PER.VALUE({})", practice_string(&register.path)),
    )
}

/// The BITFLD choice of a field (`PER.VALUE.STRING`), when there is one.
pub fn read_choice(probe: &mut dyn Probe, register: &PerRegister) -> DResult<Option<String>> {
    if !has_several_elements(&register.path) {
        return Ok(None);
    }
    match probe.fnc(&format!(
        "PER.VALUE.STRING({})",
        practice_string(&register.path)
    )) {
        Ok(Value::Text(text)) if !text.trim().is_empty() => Ok(Some(text.trim().to_string())),
        Ok(_) => Ok(None),
        Err(error) => {
            let error = DebugError::from(error);
            if error.lost { Err(error) } else { Ok(None) }
        }
    }
}

/// `Data.Long(<address>)`: 32 bits with PowerView's addressing.
pub fn read_long(probe: &mut dyn Probe, address: &TargetAddress) -> DResult<u64> {
    eval_u64(probe, &format!("Data.Long({address})"))
}

#[cfg(test)]
pub mod fake {
    //! A scripted PowerView for unit tests.

    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    pub struct FakeProbe {
        /// Answers of `fnc`, by exact expression.
        pub values: HashMap<String, Value>,
        /// Every call: `fnc <expr>`, `cmd <command>`, `read <address> <length>`.
        pub log: Vec<String>,
        /// Byte memory for `read_memory`, keyed by address (access class ignored).
        pub memory: HashMap<u64, u8>,
        /// Commands that fail.
        pub failing_commands: Vec<String>,
        /// Every call fails as if the connection had been lost.
        pub disconnected: bool,
    }

    fn lost() -> t32rcl::Error {
        t32rcl::Error::Connect("Connection closed by peer".into())
    }

    impl FakeProbe {
        pub fn with(values: &[(&str, Value)]) -> FakeProbe {
            FakeProbe {
                values: values
                    .iter()
                    .map(|(key, value)| (key.to_string(), value.clone()))
                    .collect(),
                ..Default::default()
            }
        }

        pub fn set(&mut self, expression: &str, value: Value) {
            self.values.insert(expression.to_string(), value);
        }

        pub fn commands(&self) -> Vec<&str> {
            self.log
                .iter()
                .filter_map(|line| line.strip_prefix("cmd "))
                .collect()
        }

        pub fn write(&mut self, address: u64, data: &[u8]) {
            for (index, byte) in data.iter().enumerate() {
                self.memory.insert(address + index as u64, *byte);
            }
        }
    }

    impl Probe for FakeProbe {
        fn fnc(&mut self, expression: &str) -> t32rcl::Result<Value> {
            self.log.push(format!("fnc {expression}"));
            if self.disconnected {
                return Err(lost());
            }
            self.values
                .get(expression)
                .cloned()
                .ok_or_else(|| t32rcl::Error::Trace32(fake_error(expression)))
        }

        fn cmd(&mut self, command: &str) -> t32rcl::Result<()> {
            self.log.push(format!("cmd {command}"));
            if self.disconnected {
                return Err(lost());
            }
            if self.failing_commands.iter().any(|c| command.starts_with(c)) {
                return Err(t32rcl::Error::Trace32(fake_error(command)));
            }
            Ok(())
        }

        fn read_memory(&mut self, address: &Address, length: usize) -> t32rcl::Result<Vec<u8>> {
            let access = address.access.clone().unwrap_or_default();
            self.log
                .push(format!("read {access}:0x{:X} {length}", address.value));
            Ok((0..length as u64)
                .map(|i| *self.memory.get(&(address.value + i)).unwrap_or(&0xEE))
                .collect())
        }
    }

    /// A TRACE32 function error as the RCL client reports it.
    fn fake_error(what: &str) -> t32rcl::Trace32Error {
        t32rcl::Trace32Error {
            code: Some(t32rcl::T32_ERR_FN1),
            operation: t32rcl::Operation::Api,
            message: format!("unknown: {what}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeProbe;
    use super::*;

    #[test]
    fn parses_trace32_addresses() {
        assert_eq!(
            TargetAddress::parse("C15:0x4025"),
            Some(TargetAddress {
                class: "C15".into(),
                value: 0x4025
            })
        );
        assert_eq!(
            TargetAddress::parse(" AD:0x70F40000 ").unwrap().to_string(),
            "AD:0x70F40000"
        );
        assert_eq!(TargetAddress::parse("D:1024.").unwrap().value, 1024);
        assert_eq!(TargetAddress::parse("HSCTLR"), None);
        assert_eq!(TargetAddress::parse(".TIMER_0.CTRL"), None);
        assert_eq!(TargetAddress::parse("\"A B\".C:0x1"), None);
        assert!(TargetAddress::parse("C15:0x4025").unwrap().is_coprocessor());
        assert!(TargetAddress::parse("EC15:0x1").unwrap().is_coprocessor());
        assert!(!TargetAddress::parse("AD:0x0").unwrap().is_coprocessor());
    }

    #[test]
    fn per_candidates_follow_the_path_rules() {
        assert_eq!(per_candidates("HSR"), [".HSR"]);
        assert_eq!(per_candidates(".HSR"), [".HSR"]);
        assert_eq!(per_candidates("HSCTLR.C"), [".HSCTLR.C", "HSCTLR.C"]);
        assert_eq!(
            per_candidates("\"A B\".C.CR"),
            ["\"A B\".C.CR", ".\"A B\".C.CR"]
        );
        assert!(!has_several_elements(".HSR"));
        assert!(has_several_elements(".HSCTLR.C"));
        assert!(!has_several_elements("\"A.B\""));
    }

    #[test]
    fn practice_strings_double_quotes() {
        assert_eq!(practice_string(".HSR"), "\".HSR\"");
        assert_eq!(practice_string("\"A B\".CR"), "\"\"\"A B\"\".CR\"");
    }

    #[test]
    fn resolve_register_reports_not_found_and_ambiguous() {
        let mut probe =
            FakeProbe::with(&[("PER.ADDRESS(\".HSR\")", Value::Text("C15:0x4025".into()))]);
        let register = resolve_register(&mut probe, "HSR").unwrap();
        assert_eq!(register.path, ".HSR");
        assert_eq!(register.address.to_string(), "C15:0x4025");

        let error = resolve_register(&mut probe, "NOPE").unwrap_err();
        assert!(
            error.message.starts_with("NOPE: not found in the PER file"),
            "{error}"
        );
        assert!(!error.lost);
    }

    #[test]
    fn symbolize_uses_name_and_begin() {
        let mut probe = FakeProbe::with(&[
            (
                "sYmbol.NAME(P:0x1010)",
                Value::Text("\\\\app\\Global\\main".into()),
            ),
            (
                "ADDRESS.OFFSET(sYmbol.BEGIN(\\\\app\\Global\\main))",
                Value::Int(0x1000),
            ),
        ]);
        let symbol = symbolize(&mut probe, 0x1010).unwrap().unwrap();
        assert_eq!(symbol.describe(), "main+0x10");
        assert_eq!(symbolize(&mut probe, 0x2000).unwrap(), None);
    }

    #[test]
    fn eval_address_falls_back_to_address_offset() {
        let mut probe = FakeProbe::with(&[
            ("sYmbol.BEGIN(x)", Value::Text("SD:0x20000000".into())),
            ("sYmbol.BEGIN(y)", Value::Text("P:0x0800:0010 odd".into())),
            ("ADDRESS.OFFSET(sYmbol.BEGIN(y))", Value::Int(0x08000010)),
        ]);
        assert_eq!(
            eval_address(&mut probe, "sYmbol.BEGIN(x)").unwrap().value,
            0x2000_0000
        );
        assert_eq!(
            eval_address(&mut probe, "sYmbol.BEGIN(y)").unwrap().value,
            0x0800_0010
        );
    }
}
