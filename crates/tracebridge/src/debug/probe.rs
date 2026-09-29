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
use std::path::{Path, PathBuf};

use t32rcl::{Address, Debugger, Value};

use super::perfile;

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
        } else if self.is_coprocessor() {
            // As PER files write them: c15:0x000E.
            write!(f, "{}:0x{:04X}", self.class, self.value)
        } else {
            write!(f, "{}:0x{:X}", self.class, self.value)
        }
    }
}

/// The command-line form of an address that PER.ADDRESS() returned, given its
/// access class and `ADDRESS.OFFSET()`.
///
/// PER.ADDRESS() of a `C15:` or `C14:` register holds the per-file/command-line
/// address times 4 (HVBAR, `c15:0x400C` in the PER file, gives offset
/// 0x10030; DBGDSCREXT, `c14:0x0220`, gives 0x880), and its text form cannot
/// be pasted back. `reg` checks the result against the value it read.
pub fn command_line_address(class: &str, offset: u64) -> Option<TargetAddress> {
    let class = class.trim().to_string();
    if class.eq_ignore_ascii_case("C15") || class.eq_ignore_ascii_case("C14") {
        return (offset % 4 == 0).then_some(TargetAddress {
            class,
            value: offset / 4,
        });
    }
    Some(TargetAddress {
        class,
        value: offset,
    })
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

/// What the PER functions need: a default PER file and the
/// `PER.Set.CONDitions` snapshot.
///
/// A PowerView started without `PER.ReProgram` has no default PER file, and
/// every PER function fails with "No default peripheral file". PER.FILENAME()
/// does not show this: it names the CPU's PER file before and after. So the
/// error itself triggers `PER.ReProgram` without arguments, which loads the
/// CPU's default PER file from the system directory, at most once per
/// connection; the failed call is then retried. Like `PER.Set.CONDitions`, it
/// changes debugger state only.
///
/// The PER functions cannot evaluate the IF conditions of a PER file (a per
/// file may wrap all core registers in one); `PER.Set.CONDitions` snapshots
/// them. It is taken before the first PER function and again when the
/// debugger mode or run state has changed since, after a state-changing
/// command, and on every use while the core runs.
#[derive(Debug, Default)]
pub struct PerSnapshot {
    taken: Option<DebuggerState>,
    /// `PER.ReProgram` already ran on this connection.
    reprogrammed: bool,
    /// The TRACE32 system directory, where a bare PER file name lives.
    system_dir: Option<PathBuf>,
    /// The PER file on disk, for the text scan behind error messages.
    file: Option<PathBuf>,
}

/// TRACE32's error when no PER file has been loaded with PER.ReProgram.
pub fn is_missing_per_file(message: &str) -> bool {
    message.contains("No default peripheral file")
}

impl PerSnapshot {
    pub fn new(system_dir: Option<PathBuf>) -> Self {
        PerSnapshot {
            system_dir,
            ..Default::default()
        }
    }

    pub fn invalidate(&mut self) {
        self.taken = None;
    }

    /// A new connection: PowerView may have been restarted.
    pub fn reconnected(&mut self) {
        self.taken = None;
        self.reprogrammed = false;
        self.file = None;
    }

    /// The PER file PowerView uses, when it could be found on disk.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Run `PER.ReProgram` after `error`, once per connection. Returns true
    /// when it ran, so the failed call should be retried.
    pub fn reprogram_after(&mut self, probe: &mut dyn Probe, error: &str) -> DResult<bool> {
        if self.reprogrammed || !is_missing_per_file(error) {
            return Ok(false);
        }
        self.reprogrammed = true;
        eprintln!(
            "tracebridge: no default PER file loaded; running PER.ReProgram (loads the CPU's \
             default PER file; debugger state only, the target is not touched)"
        );
        if let Err(error) = probe.cmd("PER.ReProgram") {
            let error = DebugError::from(error);
            if error.lost {
                return Err(error);
            }
            eprintln!("tracebridge: warning: PER.ReProgram failed ({error})");
            return Ok(false);
        }
        // The old snapshot, if any, belongs to no PER file.
        self.taken = None;
        Ok(true)
    }

    fn set_conditions(&mut self, probe: &mut dyn Probe) -> DResult<()> {
        let mut result = probe.cmd("PER.Set.CONDitions").map_err(DebugError::from);
        if let Err(error) = &result {
            if !error.lost && self.reprogram_after(probe, &error.message)? {
                result = probe.cmd("PER.Set.CONDitions").map_err(DebugError::from);
            }
        }
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.lost => Err(error),
            Err(error) => {
                eprintln!(
                    "tracebridge: warning: PER.Set.CONDitions failed ({error}); registers \
                     inside IF conditions of the PER file may not resolve"
                );
                Ok(())
            }
        }
    }

    pub fn ensure(&mut self, probe: &mut dyn Probe) -> DResult<()> {
        let state = DebuggerState::read(probe)?;
        if self.taken == Some(state) && state.running != Some(true) {
            return Ok(());
        }
        self.set_conditions(probe)?;
        if let Some(Value::Text(name)) = optional_value(probe, "PER.FILENAME()")? {
            self.file = perfile::locate(&name, self.system_dir.as_deref());
        }
        self.taken = Some(state);
        Ok(())
    }
}

/// A function result, `None` when TRACE32 cannot evaluate it.
fn optional_value(probe: &mut dyn Probe, expression: &str) -> DResult<Option<Value>> {
    match probe.fnc(expression) {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            let error = DebugError::from(error);
            if error.lost { Err(error) } else { Ok(None) }
        }
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

/// How far past a symbol without a known size an address may lie and still be
/// shown as `symbol+offset`.
pub const SYMBOL_SLACK: u64 = 0x100;

/// A function result as a number, `None` when TRACE32 cannot evaluate it.
fn optional_u64(probe: &mut dyn Probe, expression: &str) -> DResult<Option<u64>> {
    match probe.fnc(expression) {
        Ok(value) => Ok(value_as_u64(&value)),
        Err(error) => {
            let error = DebugError::from(error);
            if error.lost { Err(error) } else { Ok(None) }
        }
    }
}

/// Map a code address to its symbol with `sYmbol.NAME()` and the offset from
/// `sYmbol.BEGIN()` of that symbol. sYmbol.NAME() names the nearest symbol
/// below the address even when it is far away (a boot ROM address next to
/// the last symbol of the image), so the address must lie within
/// `sYmbol.BEGIN()..=sYmbol.END()`, or within [`SYMBOL_SLACK`] bytes when
/// the symbol has no size. Otherwise, and on failures, there is no symbol.
pub fn symbolize(probe: &mut dyn Probe, address: u64) -> DResult<Option<SymbolRef>> {
    let Some(symbol) = nearest_symbol(probe, address)? else {
        return Ok(None);
    };
    let Some(offset) = symbol.offset else {
        // TRACE32 wrote `name+offset` itself, or the start is unknown: keep
        // it only when the offset it printed is small.
        let small = symbol
            .path
            .rsplit_once('+')
            .and_then(|(_, offset)| crate::pycompat::parse_int_auto(offset))
            .is_some_and(|offset| (0..SYMBOL_SLACK as i128).contains(&offset));
        return Ok(small.then_some(symbol));
    };
    let begin = address - offset;
    let end = optional_u64(
        probe,
        &format!("ADDRESS.OFFSET(sYmbol.END({}))", symbol.path),
    )?;
    let inside = match end {
        Some(end) if end > begin => address <= end,
        _ => offset < SYMBOL_SLACK,
    };
    Ok(inside.then_some(symbol))
}

fn nearest_symbol(probe: &mut dyn Probe, address: u64) -> DResult<Option<SymbolRef>> {
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
    let offset = optional_u64(probe, &format!("ADDRESS.OFFSET(sYmbol.BEGIN({name}))"))?
        .and_then(|begin| address.checked_sub(begin));
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

/// Where a register's address comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSource {
    /// PER.ADDRESS(), converted to the command-line form.
    PerAddress,
    /// The text scan of the PER file, when PER.ADDRESS() fails (PAR_256).
    PerFile,
}

/// A register found in the PER file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerRegister {
    /// The path that resolved, as passed to the PER functions.
    pub path: String,
    /// The address as the PowerView command line reads it.
    pub address: TargetAddress,
    /// PER.ADDRESS()'s own text, which for C14/C15 is not a command-line
    /// address.
    pub raw: String,
    /// PER.ADDRESS()'s offset.
    pub offset: Option<u64>,
    pub source: AddressSource,
    /// The register that holds this field, when the name is `REG.FIELD` and
    /// the address came from the PER file.
    pub register_path: Option<String>,
    /// Shown with the value, e.g. how a duplicate name was resolved.
    pub note: Option<String>,
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

/// `PER.ADDRESS(path)` as a register: its text, offset and command-line form.
fn per_address(probe: &mut dyn Probe, path: &str) -> DResult<PerRegister> {
    let function = format!("PER.ADDRESS({})", practice_string(path));
    let raw = eval_text(probe, &function)?;
    let offset = match optional_u64(probe, &format!("ADDRESS.OFFSET({function})"))? {
        Some(offset) => offset,
        None => TargetAddress::parse(&raw)
            .map(|address| address.value)
            .ok_or_else(|| DebugError::new(format!("{function} returned {raw:?}")))?,
    };
    let class = raw
        .rsplit_once(':')
        .map(|(class, _)| class.trim())
        .unwrap_or("");
    // An offset that cannot be converted keeps TRACE32's text as it is.
    let address = command_line_address(class, offset)
        .or_else(|| TargetAddress::parse(&raw))
        .unwrap_or(TargetAddress {
            class: class.to_string(),
            value: offset,
        });
    Ok(PerRegister {
        path: path.to_string(),
        address,
        raw: raw.trim().to_string(),
        offset: Some(offset),
        source: AddressSource::PerAddress,
        register_path: None,
        note: None,
    })
}

/// PER.ADDRESS() for each candidate path; the first error message otherwise.
fn try_paths(probe: &mut dyn Probe, name: &str) -> DResult<Result<PerRegister, String>> {
    let mut first_error: Option<String> = None;
    for path in per_candidates(name) {
        match per_address(probe, &path) {
            Ok(register) => return Ok(Ok(register)),
            Err(error) if error.lost => return Err(error),
            Err(error) => {
                first_error.get_or_insert(error.message);
            }
        }
    }
    Ok(Err(first_error.unwrap_or_default()))
}

/// The address all candidates share.
fn common_address(candidates: &[perfile::Candidate]) -> Option<TargetAddress> {
    let first = candidates.first()?.address.clone()?;
    candidates
        .iter()
        .all(|c| c.address.as_ref() == Some(&first))
        .then_some(first)
}

/// A register whose address comes from the PER file scan; PER.VALUE() still
/// reads it through `path`.
fn from_per_file(
    name: &str,
    candidates: &[perfile::Candidate],
    path: String,
    note: Option<String>,
) -> Option<PerRegister> {
    let address = common_address(candidates)?;
    let (_, field) = perfile::register_and_field(name)?;
    let register_path = field.and_then(|_| parent_path(&path).map(str::to_string));
    Some(PerRegister {
        path,
        address,
        raw: "(address from the PER file)".into(),
        offset: None,
        source: AddressSource::PerFile,
        register_path,
        note,
    })
}

/// An ambiguous name whose definitions all describe the same register:
/// the same address, and for a field the same field definition. It is read
/// through the first full path, with a note.
fn resolve_duplicate(
    probe: &mut dyn Probe,
    name: &str,
    candidates: &[perfile::Candidate],
) -> DResult<Option<PerRegister>> {
    if candidates.len() < 2 {
        return Ok(None);
    }
    let Some(address) = common_address(candidates) else {
        return Ok(None);
    };
    let Some((_, field)) = perfile::register_and_field(name) else {
        return Ok(None);
    };
    if field.is_some() {
        let first = &candidates[0].field_definition;
        if first.is_none() || candidates.iter().any(|c| &c.field_definition != first) {
            return Ok(None);
        }
    }
    let path = candidates[0].path.clone();
    let note = format!(
        "defined {} times in the PER file, all at {address}; read as '{path}'",
        candidates.len()
    );
    match per_address(probe, &path) {
        Ok(mut register) => {
            register.note = Some(note);
            Ok(Some(register))
        }
        Err(error) if error.lost => Err(error),
        Err(error) if error.message.contains("PAR_256") => {
            Ok(from_per_file(name, candidates, path, Some(note)))
        }
        Err(_) => Ok(None),
    }
}

/// Resolve a register name with `PER.ADDRESS()`.
///
/// - "No default peripheral file": `PER.ReProgram` once, then retry.
/// - PAR_256 (PER.ADDRESS() fails on rgroup entries, PER.VALUE() works): the
///   address comes from the PER file scan.
/// - Ambiguous, but every definition is the same register: read the first.
///
/// Other failures are explained: an ambiguous name lists the full paths from
/// the PER file, and unresolvable entries suggest reading by address.
pub fn resolve_register(
    probe: &mut dyn Probe,
    per: &mut PerSnapshot,
    name: &str,
) -> DResult<PerRegister> {
    let mut message = match try_paths(probe, name)? {
        Ok(register) => return Ok(register),
        Err(message) => message,
    };
    if per.reprogram_after(probe, &message)? {
        per.ensure(probe)?;
        message = match try_paths(probe, name)? {
            Ok(register) => return Ok(register),
            Err(message) => message,
        };
    }
    let candidates = match per.file() {
        Some(file) => perfile::scan(file, name),
        None => Vec::new(),
    };
    if message.contains("PAR_256") {
        let path = per_candidates(name).remove(0);
        if let Some(register) = from_per_file(name, &candidates, path, None) {
            return Ok(register);
        }
    }
    if message.to_ascii_lowercase().contains("ambig") {
        if let Some(register) = resolve_duplicate(probe, name, &candidates)? {
            return Ok(register);
        }
    }
    Err(DebugError::new(lookup_error(name, &message, &candidates)))
}

/// Candidates listed in a lookup error.
const LISTED_CANDIDATES: usize = 8;

/// The message for a failed lookup of `name`, from TRACE32's `message` and
/// what the PER file scan found.
pub fn lookup_error(name: &str, message: &str, candidates: &[perfile::Candidate]) -> String {
    let trace32 = if message.is_empty() {
        String::new()
    } else {
        format!(" (TRACE32: {message})")
    };
    let lower = message.to_ascii_lowercase();
    // One address for all candidates: suggest reading it directly.
    let address = match candidates.first().and_then(|c| c.address.clone()) {
        Some(first)
            if candidates
                .iter()
                .all(|c| c.address.as_ref() == Some(&first)) =>
        {
            Some(first)
        }
        _ => None,
    };
    let by_address = match &address {
        Some(address) => format!("reg {address}"),
        None => "reg <class>:<address>".to_string(),
    };
    let mut text = if lower.contains("ambig") {
        format!(
            "{name}: the name occurs more than once in the PER file{trace32}. Give the full \
             path, with the elements separated by dots and quoted when they contain spaces, \
             e.g. reg '\"<tree>\".\"<subtree>\".{name}', or read it by address: {by_address}"
        )
    } else if message.contains("PAR_256") {
        format!(
            "{name}: TRACE32's PER functions could not resolve this entry (internal error \
             PAR_256; seen for read-only rgroup definitions); read it by address: {by_address}"
        )
    } else {
        format!("{name}: not found in the PER file{trace32}")
    };
    if !candidates.is_empty() && !lower.contains("no default peripheral file") {
        text.push_str("\n  defined in the PER file as:");
        for candidate in candidates.iter().take(LISTED_CANDIDATES) {
            text.push_str(&format!("\n    reg '{}'", candidate.path));
            if let Some(address) = &candidate.address {
                text.push_str(&format!("  ({address})"));
            }
            if candidate.read_only {
                text.push_str("  rgroup");
            }
        }
        if candidates.len() > LISTED_CANDIDATES {
            text.push_str(&format!(
                "\n    ... {} more",
                candidates.len() - LISTED_CANDIDATES
            ));
        }
    }
    text
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

/// The path without its last element (`.HSCTLR.C` → `.HSCTLR`).
fn parent_path(path: &str) -> Option<&str> {
    let mut quoted = false;
    let mut last = None;
    for (index, c) in path.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '.' if !quoted && index > 0 => last = Some(index),
            _ => {}
        }
    }
    last.map(|index| &path[..index])
}

/// The result of the address self-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressCheck {
    /// A memory-mapped address: no conversion, nothing to check.
    NotApplicable,
    /// `Data.Long(<address>)` read the register's value.
    Confirmed,
    /// The value is 0 or all ones, which a wrong address may read too.
    Unconfirmed,
    /// `Data.Long(<address>)` read something else, or failed (bus error).
    Failed,
}

impl AddressCheck {
    pub fn as_json(self) -> serde_json::Value {
        match self {
            AddressCheck::Confirmed => true.into(),
            AddressCheck::Failed => false.into(),
            AddressCheck::Unconfirmed | AddressCheck::NotApplicable => serde_json::Value::Null,
        }
    }

    pub fn name(self) -> Option<&'static str> {
        match self {
            AddressCheck::NotApplicable => None,
            AddressCheck::Confirmed => Some("confirmed"),
            AddressCheck::Unconfirmed => Some("unconfirmed"),
            AddressCheck::Failed => Some("failed"),
        }
    }
}

/// Does the printed address of a coprocessor register read the same
/// register? `Data.Long(<address>)` must equal the register's value (the
/// containing register's when `register` is a field). A value of 0 or all
/// ones cannot confirm an address: wrong addresses often read the same.
pub fn verify_address(
    probe: &mut dyn Probe,
    register: &PerRegister,
    value: u64,
) -> DResult<AddressCheck> {
    const MASK: u64 = 0xFFFF_FFFF;
    if !register.address.is_coprocessor() {
        return Ok(AddressCheck::NotApplicable);
    }
    fn per_value(probe: &mut dyn Probe, path: &str) -> DResult<Option<u64>> {
        optional_u64(probe, &format!("PER.VALUE({})", practice_string(path)))
    }
    let mut whole = value;
    if let Some(path) = &register.register_path {
        match per_value(probe, path)? {
            Some(value) => whole = value,
            None => return Ok(AddressCheck::Unconfirmed),
        }
    } else if let Some(parent) = parent_path(&register.path) {
        match per_address(probe, parent) {
            Ok(parent) if parent.offset == register.offset => match per_value(probe, &parent.path)?
            {
                Some(value) => whole = value,
                None => return Ok(AddressCheck::Unconfirmed),
            },
            Err(error) if error.lost => return Err(error),
            // A tree or a different register: `register` is the register.
            _ => {}
        }
    }
    let read = optional_u64(probe, &format!("Data.Long({})", register.address))?;
    Ok(match read {
        Some(read) if read & MASK == whole & MASK => {
            if matches!(whole & MASK, 0 | MASK) {
                AddressCheck::Unconfirmed
            } else {
                AddressCheck::Confirmed
            }
        }
        _ => AddressCheck::Failed,
    })
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
        /// Functions and commands that fail with this TRACE32 message.
        pub errors: HashMap<String, String>,
        /// PER.ReProgram succeeds but loads nothing.
        pub reprogram_fails: bool,
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
            if let Some(message) = self.errors.get(expression) {
                return Err(t32rcl::Error::Trace32(t32rcl::Trace32Error {
                    code: Some(t32rcl::T32_ERR_FN1),
                    operation: t32rcl::Operation::Api,
                    message: message.clone(),
                }));
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
            if let Some(message) = self.errors.get(command) {
                return Err(t32rcl::Error::Trace32(t32rcl::Trace32Error {
                    code: Some(t32rcl::T32_ERR_FN1),
                    operation: t32rcl::Operation::Command(command.into()),
                    message: message.clone(),
                }));
            }
            // Like TRACE32: loading the default PER file makes the PER
            // functions work.
            if command == "PER.ReProgram" && !self.reprogram_fails {
                self.errors
                    .retain(|_, message| !is_missing_per_file(message));
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
    fn coprocessor_addresses_are_converted_to_the_command_line_form() {
        // PER.ADDRESS() of c15:0x400C: its text cannot be pasted back and its
        // offset is the command-line address times 4.
        let mut probe = FakeProbe::with(&[
            ("PER.ADDRESS(\".VBAR\")", Value::Text("C15:0x400C0".into())),
            (
                "ADDRESS.OFFSET(PER.ADDRESS(\".VBAR\"))",
                Value::Int(0x400C * 4),
            ),
            ("PER.ADDRESS(\".DSCR\")", Value::Text("C14:0x2200".into())),
            ("ADDRESS.OFFSET(PER.ADDRESS(\".DSCR\"))", Value::Int(0x880)),
            (
                "PER.ADDRESS(\".CTRL\")",
                Value::Text("AD:0x40020000".into()),
            ),
            (
                "ADDRESS.OFFSET(PER.ADDRESS(\".CTRL\"))",
                Value::Int(0x4002_0000),
            ),
        ]);
        let mut per = PerSnapshot::default();
        let register = resolve_register(&mut probe, &mut per, "VBAR").unwrap();
        assert_eq!(register.path, ".VBAR");
        assert_eq!(register.address.to_string(), "C15:0x400C");
        assert_eq!(register.raw, "C15:0x400C0");
        assert_eq!(register.source, AddressSource::PerAddress);
        // C14 follows the same rule (confirmed on hardware).
        let register = resolve_register(&mut probe, &mut per, "DSCR").unwrap();
        assert_eq!(register.address.to_string(), "C14:0x0220");
        let register = resolve_register(&mut probe, &mut per, "CTRL").unwrap();
        assert_eq!(register.address.to_string(), "AD:0x40020000");

        assert_eq!(
            command_line_address("C15", 0x000E * 4).unwrap().to_string(),
            "C15:0x000E"
        );
        assert_eq!(
            command_line_address("C14", 0x1C0).unwrap().to_string(),
            "C14:0x0070"
        );
        assert_eq!(command_line_address("C15", 0x3), None);
    }

    const PER_FILE: &str = "\
tree \"Core (X)\"
tree \"System Control\"
  group.long c15:0x1001++0x00
    line.long 0x00 \"SCTLR,Control\"
      bitfld.long 0x00 2. \"C,Cache enable\" \"Off,On\"
      bitfld.long 0x00 4. \"E,Endianness\" \"LE,BE\"
  group.long c15:0x2001++0x00
    line.long 0x00 \"ACTLR,Auxiliary\"
tree.end
tree \"Hyp\"
  group.long c15:0x1001++0x00
    line.long 0x00 \"SCTLR,Control\"
      bitfld.long 0x00 2. \"C,Cache enable\" \"Off,On\"
      bitfld.long 0x00 5. \"E,Endianness\" \"LE,BE\"
  group.long c15:0x3001++0x00
    line.long 0x00 \"ACTLR,Auxiliary\"
  rgroup.long c15:0x000E++0x00
    line.long 0x00 \"CNTFRQ,Frequency\"
      hexmask.long 0x00 0.--31. 1. \"FREQ,Frequency\"
tree.end
tree.end
";

    /// A PER snapshot whose PER file is `PER_FILE` on disk.
    fn scanned_per_file(dir: &Path) -> PerSnapshot {
        std::fs::write(dir.join("perx.per"), PER_FILE).unwrap();
        let mut per = PerSnapshot::new(Some(dir.to_path_buf()));
        let mut probe = FakeProbe::with(&[
            ("PER.FILENAME()", Value::Text("perx.per".into())),
            ("SYStem.Mode()", Value::Int(11)),
            ("STATE.RUN()", Value::Bool(false)),
        ]);
        per.ensure(&mut probe).unwrap();
        assert_eq!(per.file(), Some(dir.join("perx.per").as_path()));
        per
    }

    fn ambiguous(probe: &mut FakeProbe, name: &str) {
        probe.errors.insert(
            format!("PER.ADDRESS({})", practice_string(&format!(".{name}"))),
            format!("Ambiguous keyword '{name}'"),
        );
    }

    #[test]
    fn not_found_is_reported_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let mut per = scanned_per_file(dir.path());
        let mut probe = FakeProbe::default();
        let error = resolve_register(&mut probe, &mut per, "NOPE").unwrap_err();
        assert!(
            error.message.starts_with("NOPE: not found in the PER file"),
            "{error}"
        );
        assert!(!error.message.contains("defined in the PER file"));
        assert!(!error.lost);
    }

    #[test]
    fn real_ambiguity_lists_full_paths_and_addresses() {
        let dir = tempfile::tempdir().unwrap();
        let mut per = scanned_per_file(dir.path());
        let mut probe = FakeProbe::default();
        ambiguous(&mut probe, "ACTLR");
        let error = resolve_register(&mut probe, &mut per, "ACTLR").unwrap_err();
        assert_eq!(
            error.message,
            "ACTLR: the name occurs more than once in the PER file (TRACE32: Ambiguous \
             keyword 'ACTLR'). Give the full path, with the elements separated by dots and \
             quoted when they contain spaces, e.g. reg '\"<tree>\".\"<subtree>\".ACTLR', or \
             read it by address: reg <class>:<address>\n  \
             defined in the PER file as:\n    \
             reg '\"Core (X)\".\"System Control\".ACTLR'  (C15:0x2001)\n    \
             reg '\"Core (X)\".Hyp.ACTLR'  (C15:0x3001)"
        );
        // Without the PER file on disk, the advice stays generic.
        let error = resolve_register(&mut probe, &mut PerSnapshot::default(), "ACTLR").unwrap_err();
        assert!(
            error
                .message
                .ends_with("or read it by address: reg <class>:<address>"),
            "{error}"
        );
    }

    #[test]
    fn duplicate_definitions_of_one_register_are_read_through_the_first_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut per = scanned_per_file(dir.path());
        let mut probe = FakeProbe::default();
        let first = "\"Core (X)\".\"System Control\".SCTLR";
        for (path, offset) in [
            (first.to_string(), 0x1001 * 4),
            (format!("{first}.C"), 0x1001 * 4),
        ] {
            let function = format!("PER.ADDRESS({})", practice_string(&path));
            probe.set(&function, Value::Text("C15:0x10010".into()));
            probe.set(&format!("ADDRESS.OFFSET({function})"), Value::Int(offset));
        }
        ambiguous(&mut probe, "SCTLR");
        let register = resolve_register(&mut probe, &mut per, "SCTLR").unwrap();
        assert_eq!(register.path, first);
        assert_eq!(register.address.to_string(), "C15:0x1001");
        assert_eq!(
            register.note.as_deref(),
            Some(
                "defined 2 times in the PER file, all at C15:0x1001; read as \
                 '\"Core (X)\".\"System Control\".SCTLR'"
            )
        );
        // A field with the same definition everywhere resolves too...
        ambiguous(&mut probe, "SCTLR.C");
        let register = resolve_register(&mut probe, &mut per, "SCTLR.C").unwrap();
        assert_eq!(register.path, format!("{first}.C"));
        // ...a field defined differently does not.
        ambiguous(&mut probe, "SCTLR.E");
        let error = resolve_register(&mut probe, &mut per, "SCTLR.E").unwrap_err();
        assert!(error.message.contains("occurs more than once"), "{error}");
    }

    #[test]
    fn rgroup_entries_take_the_address_from_the_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut per = scanned_per_file(dir.path());
        let mut probe = FakeProbe::default();
        for path in [".CNTFRQ", ".CNTFRQ.FREQ"] {
            probe.errors.insert(
                format!("PER.ADDRESS({})", practice_string(path)),
                "internal error : PAR_256".into(),
            );
        }
        let register = resolve_register(&mut probe, &mut per, "CNTFRQ").unwrap();
        assert_eq!(register.path, ".CNTFRQ");
        assert_eq!(register.address.to_string(), "C15:0x000E");
        assert_eq!(register.source, AddressSource::PerFile);
        assert_eq!(register.register_path, None);
        let register = resolve_register(&mut probe, &mut per, "CNTFRQ.FREQ").unwrap();
        assert_eq!(register.register_path.as_deref(), Some(".CNTFRQ"));

        // Without an address from the PER file, the error explains PAR_256.
        let error =
            resolve_register(&mut probe, &mut PerSnapshot::default(), "CNTFRQ").unwrap_err();
        assert!(
            error.message.starts_with(
                "CNTFRQ: TRACE32's PER functions could not resolve this entry (internal error \
                 PAR_256; seen for read-only rgroup definitions); read it by address"
            ),
            "{error}"
        );
        assert!(!error.message.contains("not found"));
    }

    const MISSING: &str = "No default peripheral file (PER.ReProgram) found.";

    #[test]
    fn per_reprogram_is_triggered_by_the_error_not_the_file_name() {
        // PER.FILENAME() names the CPU's PER file although none is loaded.
        let mut probe = FakeProbe::with(&[
            ("PER.FILENAME()", Value::Text("perx.per".into())),
            ("SYStem.Mode()", Value::Int(11)),
            ("STATE.RUN()", Value::Bool(false)),
        ]);
        probe
            .errors
            .insert("PER.Set.CONDitions".into(), MISSING.into());
        let mut per = PerSnapshot::default();
        per.ensure(&mut probe).unwrap();
        assert_eq!(
            probe.commands(),
            ["PER.Set.CONDitions", "PER.ReProgram", "PER.Set.CONDitions"]
        );
        // Later snapshots do not load it again.
        per.invalidate();
        per.ensure(&mut probe).unwrap();
        assert_eq!(probe.commands().len(), 4);
        assert_eq!(probe.commands()[3], "PER.Set.CONDitions");
    }

    #[test]
    fn a_failing_lookup_loads_the_per_file_and_retries() {
        let mut probe = FakeProbe::with(&[
            ("SYStem.Mode()", Value::Int(11)),
            ("STATE.RUN()", Value::Bool(false)),
            ("PER.ADDRESS(\".HSR\")", Value::Text("C15:0x40250".into())),
            (
                "ADDRESS.OFFSET(PER.ADDRESS(\".HSR\"))",
                Value::Int(0x4025 * 4),
            ),
        ]);
        probe
            .errors
            .insert("PER.ADDRESS(\".HSR\")".into(), MISSING.into());
        let register = resolve_register(&mut probe, &mut PerSnapshot::default(), "HSR").unwrap();
        assert_eq!(register.address.to_string(), "C15:0x4025");
        assert_eq!(probe.commands(), ["PER.ReProgram", "PER.Set.CONDitions"]);
    }

    #[test]
    fn per_reprogram_runs_at_most_once_per_connection() {
        let mut probe = FakeProbe::with(&[
            ("SYStem.Mode()", Value::Int(11)),
            ("STATE.RUN()", Value::Bool(false)),
        ]);
        probe.reprogram_fails = true;
        probe
            .errors
            .insert("PER.Set.CONDitions".into(), MISSING.into());
        probe
            .errors
            .insert("PER.ADDRESS(\".HSR\")".into(), MISSING.into());
        let mut per = PerSnapshot::default();
        per.ensure(&mut probe).unwrap();
        assert!(resolve_register(&mut probe, &mut per, "HSR").is_err());
        let reprograms = |probe: &FakeProbe| {
            probe
                .commands()
                .iter()
                .filter(|c| **c == "PER.ReProgram")
                .count()
        };
        assert_eq!(reprograms(&probe), 1);
        // A new connection may have a new PowerView.
        per.reconnected();
        per.ensure(&mut probe).unwrap();
        assert_eq!(reprograms(&probe), 2);
    }

    #[test]
    fn symbolize_uses_name_and_begin_within_the_symbol() {
        let mut probe = FakeProbe::with(&[
            (
                "sYmbol.NAME(P:0x1010)",
                Value::Text("\\\\app\\Global\\main".into()),
            ),
            (
                "ADDRESS.OFFSET(sYmbol.BEGIN(\\\\app\\Global\\main))",
                Value::Int(0x1000),
            ),
            (
                "ADDRESS.OFFSET(sYmbol.END(\\\\app\\Global\\main))",
                Value::Int(0x10FF),
            ),
        ]);
        let symbol = symbolize(&mut probe, 0x1010).unwrap().unwrap();
        assert_eq!(symbol.describe(), "main+0x10");
        assert_eq!(symbolize(&mut probe, 0x2000).unwrap(), None);
    }

    #[test]
    fn far_away_nearest_symbol_is_no_symbol() {
        // sYmbol.NAME() names the last symbol below a boot ROM address.
        let mut probe = FakeProbe::with(&[
            (
                "sYmbol.NAME(P:0x29FB81A1)",
                Value::Text("\\\\app\\Global\\__record_start".into()),
            ),
            (
                "ADDRESS.OFFSET(sYmbol.BEGIN(\\\\app\\Global\\__record_start))",
                Value::Int(0x29F8_7E00),
            ),
            (
                "ADDRESS.OFFSET(sYmbol.END(\\\\app\\Global\\__record_start))",
                Value::Int(0x29F8_7E3F),
            ),
            (
                "sYmbol.NAME(P:0x2010)",
                Value::Text("\\\\app\\Global\\vectors".into()),
            ),
            (
                "ADDRESS.OFFSET(sYmbol.BEGIN(\\\\app\\Global\\vectors))",
                Value::Int(0x2000),
            ),
            (
                "sYmbol.NAME(P:0x3000)",
                Value::Text("\\\\app\\Global\\label".into()),
            ),
            (
                "ADDRESS.OFFSET(sYmbol.BEGIN(\\\\app\\Global\\label))",
                Value::Int(0x2000),
            ),
        ]);
        assert_eq!(symbolize(&mut probe, 0x29FB_81A1).unwrap(), None);
        // No size: a small distance is still the symbol, a large one is not.
        assert_eq!(
            symbolize(&mut probe, 0x2010).unwrap().unwrap().describe(),
            "vectors+0x10"
        );
        assert_eq!(symbolize(&mut probe, 0x3000).unwrap(), None);
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
