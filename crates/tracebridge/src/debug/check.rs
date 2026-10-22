//! `check <file>`: a data-driven acceptance check. The check file lives in
//! the project; tracebridge only executes it and knows no board facts.
//!
//! ```toml
//! description = "optional"
//!
//! [[check]]
//! name = "HSCTLR"
//! read = { reg = "HSCTLR" }        # or addr / core / expr
//! expect = { eq = 0x30C5083A }     # eq/ne (+ mask), nonzero, range, in_symbol, one_of
//! variants = ["default"]           # optional
//!
//! [[check]]
//! name = "core clock"
//! read = { clock = "CLUSTER0_CORE0" }  # a clock of `debug clock`'s tree
//! expect = { hz = 400_000_000, source = "SYS_CLK_CGM0" }  # any of state, hz, source, warnings
//! ```

use std::path::Path;

use toml::{Table, Value};

use super::clock;
use super::probe::{
    self, DResult, DebugError, PerRegister, PerSnapshot, Probe, TargetAddress, hex32,
};
use crate::pycompat::parse_int_auto;
use crate::style::Style;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckFile {
    pub description: Option<String>,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub read: ReadSpec,
    pub expect: Expect,
    pub variants: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadSpec {
    /// `PER.VALUE(".<name>")` or a full PER path.
    Reg(String),
    /// `Data.Long(<address>)`.
    Addr(TargetAddress),
    /// `Register(<name>)`.
    Core(String),
    /// Any PRACTICE expression.
    Expr(String),
    /// A clock of the clock tree description (`debug clock`).
    Clock(String),
}

/// A number, or a symbol address plus an offset (`"sym:<name>[+off]"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand {
    Int(u64),
    Symbol { name: String, offset: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    Eq {
        value: Operand,
        mask: Option<u64>,
    },
    Ne {
        value: Operand,
        mask: Option<u64>,
    },
    NonZero,
    Range(u64, u64),
    InSymbol(String),
    OneOf(Vec<Operand>),
    /// The expectations of a `read.clock` check.
    Clock(ClockExpect),
}

/// A frequency in Hz. Never NaN (checked when parsed), so `Eq` holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hz(pub f64);

impl Eq for Hz {}

/// What a `read.clock` check expects; every part is optional, at least one
/// is given. The same judgement as `debug clock`'s JSON gives: `hz` within
/// half a hertz (and so "on"), `source` the clock it runs from, `warnings`
/// the exact set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClockExpect {
    /// `state = "on"` (true) or `"off"` (false).
    pub on: Option<bool>,
    pub hz: Option<Hz>,
    pub source: Option<String>,
    pub warnings: Option<Vec<String>>,
}

impl ClockExpect {
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        match (self.hz, self.on) {
            (Some(Hz(hz)), _) => parts.push(clock::format_frequency(hz)),
            (None, Some(true)) => parts.push("on".to_string()),
            (None, Some(false)) => parts.push("off".to_string()),
            (None, None) => {}
        }
        if let Some(source) = &self.source {
            parts.push(format!("from {source}"));
        }
        match &self.warnings {
            Some(warnings) if warnings.is_empty() => parts.push("no warnings".to_string()),
            Some(warnings) => parts.push(format!("warnings [{}]", warnings.join(", "))),
            None => {}
        }
        parts.join(", ")
    }

    /// What does not match `clock`; empty when it holds.
    fn mismatches(&self, clock: &clock::Evaluated) -> Vec<String> {
        let (state, hz) = clock_state(clock);
        let mut found = Vec::new();
        let want_on = if self.hz.is_some() {
            Some(true)
        } else {
            self.on
        };
        if let Some(on) = want_on {
            let want = if on { "on" } else { "off" };
            if state != want {
                found.push(format!("state {state}, expected {want}"));
            }
        }
        if let Some(Hz(expected)) = self.hz {
            if hz.is_none_or(|hz| (hz - expected).abs() > 0.5) {
                found.push(format!("expected {}", clock::format_frequency(expected)));
            }
        }
        if let Some(source) = &self.source {
            if clock.parent.as_deref() != Some(source.as_str()) {
                found.push(format!("expected from {source}"));
            }
        }
        if let Some(warnings) = &self.warnings {
            let mut expected = warnings.clone();
            let mut actual = clock.warnings.clone();
            expected.sort();
            actual.sort();
            if expected != actual {
                found.push(format!("expected warnings [{}]", warnings.join(", ")));
            }
        }
        found
    }
}

/// The state word and frequency of an evaluated clock.
fn clock_state(clock: &clock::Evaluated) -> (&'static str, Option<f64>) {
    match &clock.state {
        clock::State::Hz(hz) => ("on", Some(*hz)),
        clock::State::Off(_) => ("off", None),
        clock::State::Unknown(_) => ("unknown", None),
        clock::State::Error(_) => ("error", None),
    }
}

/// An evaluated clock as text: `on 400 MHz from PLL1_D0_PHI_HP ! not locked`.
fn describe_clock(clock: &clock::Evaluated) -> String {
    let (state, hz) = clock_state(clock);
    let mut parts = vec![state.to_string()];
    if let Some(hz) = hz {
        parts.push(clock::format_frequency(hz));
    }
    if let Some(parent) = &clock.parent {
        parts.push(format!("from {parent}"));
    }
    if !clock.warnings.is_empty() {
        parts.push(format!("! {}", clock.warnings.join(", ")));
    }
    match &clock.state {
        clock::State::Off(reason) | clock::State::Unknown(reason) => {
            parts.push(format!("({reason})"))
        }
        _ => {}
    }
    parts.join(" ")
}

impl Check {
    /// Checks without `variants` run for every variant; the others only when
    /// `--variant` names one of them.
    pub fn selected(&self, variant: Option<&str>) -> bool {
        match (&self.variants, variant) {
            (None, _) => true,
            (Some(variants), Some(variant)) => variants.iter().any(|v| v == variant),
            (Some(_), None) => false,
        }
    }
}

// ---------------------------------------------------------------- parsing

pub fn load(path: &Path) -> DResult<CheckFile> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| DebugError::new(format!("cannot read {}: {error}", path.display())))?;
    parse(&text).map_err(|error| error.context(path.display()))
}

pub fn parse(text: &str) -> DResult<CheckFile> {
    let document: Table = text
        .parse()
        .map_err(|error: toml::de::Error| DebugError::new(error.message().trim_end()))?;
    for key in document.keys() {
        if key != "description" && key != "check" {
            return Err(DebugError::new(format!(
                "unknown key '{key}' (expected description and [[check]])"
            )));
        }
    }
    let description = match document.get("description") {
        None => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return Err(DebugError::new("description must be a string")),
    };
    let entries = match document.get("check") {
        None => return Err(DebugError::new("no [[check]] entries")),
        Some(Value::Array(entries)) => entries,
        Some(_) => {
            return Err(DebugError::new(
                "check must be an array of tables ([[check]])",
            ));
        }
    };
    let mut checks = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let Value::Table(table) = entry else {
            return Err(DebugError::new(format!(
                "check #{} must be a table",
                index + 1
            )));
        };
        let label = match table.get("name") {
            Some(Value::String(name)) => format!("check #{} ({name})", index + 1),
            _ => format!("check #{}", index + 1),
        };
        checks.push(parse_check(table).map_err(|error| error.context(&label))?);
    }
    Ok(CheckFile {
        description,
        checks,
    })
}

fn parse_check(table: &Table) -> DResult<Check> {
    for key in table.keys() {
        if !matches!(key.as_str(), "name" | "read" | "expect" | "variants") {
            return Err(DebugError::new(format!("unknown key '{key}'")));
        }
    }
    let name = match table.get("name") {
        Some(Value::String(name)) if !name.is_empty() => name.clone(),
        Some(_) => return Err(DebugError::new("name must be a non-empty string")),
        None => return Err(DebugError::new("name is missing")),
    };
    let read = match table.get("read") {
        Some(Value::Table(read)) => parse_read(read)?,
        Some(_) => return Err(DebugError::new("read must be a table")),
        None => return Err(DebugError::new("read is missing")),
    };
    let expect = match table.get("expect") {
        Some(Value::Table(expect)) if matches!(read, ReadSpec::Clock(_)) => {
            Expect::Clock(parse_clock_expect(expect)?)
        }
        Some(Value::Table(expect)) => parse_expect(expect)?,
        Some(_) => return Err(DebugError::new("expect must be a table")),
        None => return Err(DebugError::new("expect is missing")),
    };
    let variants = match table.get("variants") {
        None => None,
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .map(|item| match item {
                    Value::String(name) => Ok(name.clone()),
                    _ => Err(DebugError::new("variants must be an array of strings")),
                })
                .collect::<DResult<Vec<_>>>()?,
        ),
        Some(_) => return Err(DebugError::new("variants must be an array of strings")),
    };
    Ok(Check {
        name,
        read,
        expect,
        variants,
    })
}

fn parse_read(table: &Table) -> DResult<ReadSpec> {
    let exactly_one =
        || DebugError::new("read must have exactly one of reg, addr, core, expr, clock");
    if table.len() != 1 {
        return Err(exactly_one());
    }
    let (key, value) = table.iter().next().unwrap();
    let Value::String(text) = value else {
        return Err(DebugError::new(format!("read.{key} must be a string")));
    };
    if text.trim().is_empty() {
        return Err(DebugError::new(format!("read.{key} is empty")));
    }
    Ok(match key.as_str() {
        "reg" => ReadSpec::Reg(text.clone()),
        "addr" => ReadSpec::Addr(TargetAddress::parse(text).ok_or_else(|| {
            DebugError::new(format!(
                "read.addr {text:?} must be <access class>:<address>, e.g. AD:0x20000000"
            ))
        })?),
        "core" => ReadSpec::Core(text.clone()),
        "expr" => ReadSpec::Expr(text.clone()),
        "clock" => ReadSpec::Clock(text.trim().to_string()),
        _ => return Err(exactly_one()),
    })
}

fn parse_operand(value: &Value, key: &str) -> DResult<Operand> {
    match value {
        Value::Integer(number) if *number >= 0 => Ok(Operand::Int(*number as u64)),
        Value::Integer(_) => Err(DebugError::new(format!("{key} must not be negative"))),
        Value::String(text) => {
            if let Some(symbol) = text.strip_prefix("sym:") {
                return parse_symbol(symbol)
                    .ok_or_else(|| DebugError::new(format!("{key}: invalid symbol {text:?}")));
            }
            parse_int_auto(text)
                .and_then(|number| u64::try_from(number).ok())
                .map(Operand::Int)
                .ok_or_else(|| {
                    DebugError::new(format!(
                        "{key}: {text:?} is neither an integer nor \"sym:<name>[+offset]\""
                    ))
                })
        }
        _ => Err(DebugError::new(format!(
            "{key} must be an integer or \"sym:<name>[+offset]\""
        ))),
    }
}

/// `<name>`, `<name>+<offset>` or `<name>-<offset>`.
fn parse_symbol(text: &str) -> Option<Operand> {
    let text = text.trim();
    let split = text.rfind(['+', '-']).filter(|&index| index > 0);
    let (name, offset) = match split {
        Some(index) => {
            let magnitude = parse_int_auto(&text[index + 1..])?;
            let magnitude = i64::try_from(magnitude).ok()?;
            let offset = if text.as_bytes()[index] == b'-' {
                -magnitude
            } else {
                magnitude
            };
            (text[..index].trim(), offset)
        }
        None => (text, 0),
    };
    if name.is_empty() {
        return None;
    }
    Some(Operand::Symbol {
        name: name.to_string(),
        offset,
    })
}

fn parse_mask(table: &Table) -> DResult<Option<u64>> {
    match table.get("mask") {
        None => Ok(None),
        Some(value) => match parse_operand(value, "expect.mask")? {
            Operand::Int(mask) => Ok(Some(mask)),
            Operand::Symbol { .. } => Err(DebugError::new("expect.mask must be an integer")),
        },
    }
}

const CLOCK_FORMS: &str = "state, hz, source, warnings";

fn parse_clock_expect(table: &Table) -> DResult<ClockExpect> {
    let mut expect = ClockExpect::default();
    if table.is_empty() {
        return Err(DebugError::new(format!(
            "expect must have at least one of {CLOCK_FORMS} for read.clock"
        )));
    }
    for (key, value) in table {
        match (key.as_str(), value) {
            ("state", Value::String(state)) if state == "on" || state == "off" => {
                expect.on = Some(state == "on");
            }
            ("state", _) => return Err(DebugError::new("expect.state must be \"on\" or \"off\"")),
            ("hz", Value::Integer(hz)) if *hz >= 0 => expect.hz = Some(Hz(*hz as f64)),
            ("hz", Value::Float(hz)) if hz.is_finite() && *hz >= 0.0 => expect.hz = Some(Hz(*hz)),
            ("hz", _) => return Err(DebugError::new("expect.hz must be a frequency in Hz")),
            ("source", Value::String(source)) if !source.trim().is_empty() => {
                expect.source = Some(source.trim().to_string());
            }
            ("source", _) => return Err(DebugError::new("expect.source must be a clock name")),
            ("warnings", Value::Array(items)) => {
                expect.warnings = Some(
                    items
                        .iter()
                        .map(|item| match item {
                            Value::String(text) => Ok(text.clone()),
                            _ => Err(DebugError::new(
                                "expect.warnings must be an array of strings",
                            )),
                        })
                        .collect::<DResult<Vec<_>>>()?,
                );
            }
            ("warnings", _) => {
                return Err(DebugError::new(
                    "expect.warnings must be an array of strings",
                ));
            }
            (other, _) => {
                return Err(DebugError::new(format!(
                    "unknown expectation '{other}' for read.clock (expected {CLOCK_FORMS})"
                )));
            }
        }
    }
    if expect.hz.is_some() && expect.on == Some(false) {
        return Err(DebugError::new("expect.hz with state \"off\""));
    }
    Ok(expect)
}

fn parse_expect(table: &Table) -> DResult<Expect> {
    const FORMS: &str = "eq, ne, nonzero, range, in_symbol, one_of";
    if let Some(key) = table
        .keys()
        .find(|key| matches!(key.as_str(), "state" | "hz" | "source" | "warnings"))
    {
        return Err(DebugError::new(format!(
            "expect.{key} only works with read.clock"
        )));
    }
    let forms: Vec<&String> = table.keys().filter(|key| key.as_str() != "mask").collect();
    if forms.len() != 1 {
        return Err(DebugError::new(format!(
            "expect must have exactly one of {FORMS} (plus mask with eq/ne)"
        )));
    }
    let key = forms[0].as_str();
    let value = &table[key];
    if table.contains_key("mask") && !matches!(key, "eq" | "ne") {
        return Err(DebugError::new("expect.mask only works with eq and ne"));
    }
    Ok(match key {
        "eq" => Expect::Eq {
            value: parse_operand(value, "expect.eq")?,
            mask: parse_mask(table)?,
        },
        "ne" => Expect::Ne {
            value: parse_operand(value, "expect.ne")?,
            mask: parse_mask(table)?,
        },
        "nonzero" => match value {
            Value::Boolean(true) => Expect::NonZero,
            _ => return Err(DebugError::new("expect.nonzero must be true")),
        },
        "range" => {
            let bounds = match value {
                Value::Array(items) if items.len() == 2 => items
                    .iter()
                    .map(|item| match parse_operand(item, "expect.range")? {
                        Operand::Int(number) => Ok(number),
                        Operand::Symbol { .. } => {
                            Err(DebugError::new("expect.range bounds must be integers"))
                        }
                    })
                    .collect::<DResult<Vec<u64>>>()?,
                _ => return Err(DebugError::new("expect.range must be [low, high]")),
            };
            if bounds[0] > bounds[1] {
                return Err(DebugError::new("expect.range: low is greater than high"));
            }
            Expect::Range(bounds[0], bounds[1])
        }
        "in_symbol" => match value {
            Value::String(name) if !name.trim().is_empty() => Expect::InSymbol(name.clone()),
            _ => return Err(DebugError::new("expect.in_symbol must be a symbol name")),
        },
        "one_of" => match value {
            Value::Array(items) if !items.is_empty() => Expect::OneOf(
                items
                    .iter()
                    .map(|item| parse_operand(item, "expect.one_of"))
                    .collect::<DResult<Vec<_>>>()?,
            ),
            _ => return Err(DebugError::new("expect.one_of must be a non-empty array")),
        },
        other => {
            return Err(DebugError::new(format!(
                "unknown expectation '{other}' (expected one of {FORMS})"
            )));
        }
    })
}

// -------------------------------------------------------------- execution

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub variant: Option<String>,
    pub halt: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    Error,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Pass => "ok",
            Status::Fail => "FAIL",
            Status::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub name: String,
    pub status: Status,
    /// What was (or would be) read, e.g. `reg .HSCTLR (C15:0x4001)`.
    pub read: String,
    pub value: Option<u64>,
    /// The value as text, for reads that are not a number (`read.clock`).
    pub text: Option<String>,
    /// The expectation with symbols resolved.
    pub expect: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub description: Option<String>,
    pub variant: Option<String>,
    pub dry_run: bool,
    /// `--halt` stopped the core.
    pub halted: bool,
    pub skipped: usize,
    pub results: Vec<CheckResult>,
}

impl Report {
    fn count(&self, status: Status) -> usize {
        self.results.iter().filter(|r| r.status == status).count()
    }

    pub fn passed(&self) -> bool {
        self.results.iter().all(|r| r.status == Status::Pass)
    }

    /// 0 when every selected check passed, 3 otherwise.
    pub fn exit_code(&self) -> i32 {
        if self.passed() { 0 } else { super::EXIT_FAILED }
    }

    pub fn summary(&self) -> String {
        let mut text = if self.dry_run {
            format!(
                "dry run: {} resolved, {} errors",
                self.count(Status::Pass),
                self.count(Status::Error)
            )
        } else {
            format!(
                "{} passed, {} failed, {} errors",
                self.count(Status::Pass),
                self.count(Status::Fail),
                self.count(Status::Error)
            )
        };
        if self.skipped > 0 {
            let variant = match &self.variant {
                Some(variant) => format!("not in variant {variant:?}"),
                None => "variant-specific; pass --variant".to_string(),
            };
            text.push_str(&format!(", {} skipped ({variant})", self.skipped));
        }
        text
    }

    pub fn human(&self, style: Style) -> String {
        let mut lines = Vec::new();
        if let Some(description) = &self.description {
            lines.push(style.value(description));
        }
        if self.halted {
            lines.push(
                style.warn("core halted (Break) for CP15/core register reads; it stays halted"),
            );
        }
        let width = self.results.iter().map(|r| r.name.len()).max().unwrap_or(0);
        for result in &self.results {
            let status = format!("{:<5}", result.status.label());
            let status = match result.status {
                Status::Pass => style.good(status),
                Status::Fail | Status::Error => style.bad(status),
            };
            let value = match (&result.text, result.value) {
                (Some(text), _) => style.value(format!("{text:<10}")),
                (None, Some(value)) => style.value(format!("{:<10}", hex32(value))),
                (None, None) if self.dry_run => format!("{:<10}", result.read),
                (None, None) => style.dim(format!("{:<10}", "-")),
            };
            let mut line = format!(
                "  {status} {}  {value}  {}",
                style.label(format!("{:<width$}", result.name)),
                result.expect
            );
            if let Some(error) = &result.error {
                line.push_str(&format!("  ({error})"));
            }
            lines.push(line.trim_end().to_string());
        }
        lines.push(if self.passed() {
            style.good(self.summary())
        } else {
            style.bad(self.summary())
        });
        lines.join("\n")
    }

    pub fn to_json(&self) -> serde_json::Value {
        let results: Vec<_> = self
            .results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "status": r.status.label().to_ascii_lowercase(),
                    "read": r.read,
                    "value": r.value,
                    "hex": if r.text.is_some() { None } else { r.value.map(hex32) },
                    "text": r.text,
                    "expect": r.expect,
                    "error": r.error,
                })
            })
            .collect();
        serde_json::json!({
            "description": self.description,
            "variant": self.variant,
            "dry_run": self.dry_run,
            "halted": self.halted,
            "passed": self.passed(),
            "summary": {
                "passed": self.count(Status::Pass),
                "failed": self.count(Status::Fail),
                "errors": self.count(Status::Error),
                "skipped": self.skipped,
            },
            "results": results,
        })
    }
}

/// An expectation with its symbols resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Resolved {
    Eq(u64, Option<u64>),
    Ne(u64, Option<u64>),
    NonZero,
    Range(u64, u64),
    OneOf(Vec<u64>),
    /// Judged against the evaluated clock tree, not a number.
    Clock(ClockExpect),
}

impl Resolved {
    fn holds(&self, value: u64) -> bool {
        match self {
            Resolved::Eq(expected, mask) => value & mask.unwrap_or(u64::MAX) == *expected,
            Resolved::Ne(expected, mask) => value & mask.unwrap_or(u64::MAX) != *expected,
            Resolved::NonZero => value != 0,
            Resolved::Range(low, high) => (*low..=*high).contains(&value),
            Resolved::OneOf(values) => values.contains(&value),
            Resolved::Clock(_) => false,
        }
    }
}

fn describe_operand(operand: &Operand, value: u64) -> String {
    match operand {
        Operand::Int(_) => hex32(value),
        Operand::Symbol { name, offset: 0 } => format!("{name} ({})", hex32(value)),
        Operand::Symbol { name, offset } if *offset > 0 => {
            format!("{name}+0x{offset:X} ({})", hex32(value))
        }
        Operand::Symbol { name, offset } => {
            format!("{name}-0x{:X} ({})", offset.unsigned_abs(), hex32(value))
        }
    }
}

fn resolve_operand(probe: &mut dyn Probe, operand: &Operand) -> DResult<u64> {
    match operand {
        Operand::Int(value) => Ok(*value),
        Operand::Symbol { name, offset } => {
            let base = probe::symbol_address(probe, name)?.value;
            Ok(base.wrapping_add_signed(*offset))
        }
    }
}

fn resolve_expect(probe: &mut dyn Probe, expect: &Expect) -> DResult<(Resolved, String)> {
    let masked = |mask: &Option<u64>| match mask {
        Some(mask) => format!("& {} ", hex32(*mask)),
        None => String::new(),
    };
    Ok(match expect {
        Expect::Eq { value, mask } => {
            let resolved = resolve_operand(probe, value)?;
            (
                Resolved::Eq(resolved, *mask),
                format!("{}== {}", masked(mask), describe_operand(value, resolved)),
            )
        }
        Expect::Ne { value, mask } => {
            let resolved = resolve_operand(probe, value)?;
            (
                Resolved::Ne(resolved, *mask),
                format!("{}!= {}", masked(mask), describe_operand(value, resolved)),
            )
        }
        Expect::NonZero => (Resolved::NonZero, "!= 0".into()),
        Expect::Range(low, high) => (
            Resolved::Range(*low, *high),
            format!("in [{}, {}]", hex32(*low), hex32(*high)),
        ),
        Expect::InSymbol(name) => {
            let (begin, end) = probe::symbol_range(probe, name)?;
            (
                Resolved::Range(begin.value, end),
                format!("in {name} [{}..{}]", hex32(begin.value), hex32(end)),
            )
        }
        Expect::OneOf(operands) => {
            let mut values = Vec::new();
            let mut texts = Vec::new();
            for operand in operands {
                let value = resolve_operand(probe, operand)?;
                values.push(value);
                texts.push(describe_operand(operand, value));
            }
            (
                Resolved::OneOf(values),
                format!("one of [{}]", texts.join(", ")),
            )
        }
        Expect::Clock(expect) => (Resolved::Clock(expect.clone()), expect.describe()),
    })
}

/// A check after the resolve phase.
struct Prepared<'a> {
    check: &'a Check,
    register: Option<PerRegister>,
    read: String,
    expect: Option<(Resolved, String)>,
    error: Option<DebugError>,
}

impl Prepared<'_> {
    /// CP15/CP14 and core registers can only be read from a halted core.
    fn needs_halt(&self) -> bool {
        match &self.check.read {
            ReadSpec::Reg(_) => self
                .register
                .as_ref()
                .is_some_and(|r| r.address.is_coprocessor()),
            ReadSpec::Addr(address) => address.is_coprocessor(),
            ReadSpec::Core(_) => true,
            ReadSpec::Expr(_) | ReadSpec::Clock(_) => false,
        }
    }
}

/// Keep a lost connection fatal; everything else fails only this check.
fn per_check<T>(result: DResult<T>) -> DResult<Result<T, DebugError>> {
    match result {
        Err(error) if error.lost => Err(error),
        other => Ok(other),
    }
}

/// The clock tree for `read.clock` checks: its description and the board's
/// frequencies (`[clock]` in trace32.toml).
pub struct Clocks<'a> {
    pub tree: &'a clock::Tree,
    pub inputs: &'a [(String, f64)],
}

impl CheckFile {
    /// Whether a check selected for `variant` reads a clock.
    pub fn reads_clocks(&self, variant: Option<&str>) -> bool {
        self.checks
            .iter()
            .any(|check| check.selected(variant) && matches!(check.read, ReadSpec::Clock(_)))
    }
}

pub fn run(
    probe: &mut dyn Probe,
    per: &mut PerSnapshot,
    file: &CheckFile,
    options: &Options,
    clocks: Option<&Clocks>,
) -> DResult<Report> {
    let selected: Vec<&Check> = file
        .checks
        .iter()
        .filter(|check| check.selected(options.variant.as_deref()))
        .collect();
    let skipped = file.checks.len() - selected.len();

    if selected
        .iter()
        .any(|check| matches!(check.read, ReadSpec::Reg(_)))
    {
        per.ensure(probe)?;
    }

    // Resolve register names and symbols; nothing is read from the target.
    let mut prepared = Vec::new();
    for check in selected {
        let mut item = Prepared {
            check,
            register: None,
            read: String::new(),
            expect: None,
            error: None,
        };
        match &check.read {
            ReadSpec::Reg(name) => match per_check(probe::resolve_register(probe, per, name))? {
                Ok(register) => {
                    item.read = format!("reg {} ({})", register.path, register.address);
                    item.register = Some(register);
                }
                Err(error) => {
                    item.read = format!("reg {name}");
                    item.error = Some(error);
                }
            },
            ReadSpec::Addr(address) => item.read = format!("addr {address}"),
            ReadSpec::Core(name) => item.read = format!("core {name}"),
            ReadSpec::Expr(expression) => item.read = format!("expr {expression}"),
            ReadSpec::Clock(name) => {
                item.read = format!("clock {name}");
                match clocks {
                    None => {
                        item.error =
                            Some(DebugError::new("read.clock needs a clock tree description"));
                    }
                    Some(clocks) if !clocks.tree.clocks.iter().any(|c| c.name == *name) => {
                        item.error = Some(DebugError::new(format!(
                            "clock {name} is not in the clock tree description"
                        )));
                    }
                    Some(_) => {}
                }
            }
        }
        match per_check(resolve_expect(probe, &check.expect))? {
            Ok(expect) => item.expect = Some(expect),
            Err(error) => {
                item.error.get_or_insert(error);
            }
        }
        prepared.push(item);
    }

    let mut halted = false;
    if !options.dry_run {
        let halting: Vec<&str> = prepared
            .iter()
            .filter(|item| item.needs_halt())
            .map(|item| item.check.name.as_str())
            .collect();
        if !halting.is_empty() && probe::core_running(probe)? {
            if !options.halt {
                return Err(DebugError::new(format!(
                    "the core is running, and {} check(s) read CP15 or core registers, which \
                     needs a halted core ({}); stop it with 'tracebridge debug break' or pass --halt",
                    halting.len(),
                    halting.join(", ")
                )));
            }
            probe.cmd("Break")?;
            halted = true;
            per.invalidate();
            if prepared.iter().any(|item| item.register.is_some()) {
                per.ensure(probe)?;
            }
        }
    }

    // The clock tree is evaluated once, from memory-mapped registers, after
    // the core is stopped (when --halt stops it).
    let wants_clocks = prepared
        .iter()
        .any(|item| item.error.is_none() && matches!(item.check.read, ReadSpec::Clock(_)));
    let clock_report = match clocks {
        Some(clocks) if wants_clocks && !options.dry_run => Some(per_check(clock::evaluate(
            probe,
            clocks.tree,
            clocks.inputs,
        ))?),
        _ => None,
    };

    let mut results = Vec::new();
    for item in prepared {
        let expect_text = item
            .expect
            .as_ref()
            .map(|(_, text)| text.clone())
            .unwrap_or_else(|| describe_expect(&item.check.expect));
        let mut result = CheckResult {
            name: item.check.name.clone(),
            status: Status::Error,
            read: item.read.clone(),
            value: None,
            text: None,
            expect: expect_text,
            error: None,
        };
        if let Some(error) = &item.error {
            result.error = Some(error.message.clone());
            results.push(result);
            continue;
        }
        if options.dry_run {
            result.status = Status::Pass;
            results.push(result);
            continue;
        }
        if let (ReadSpec::Clock(name), Some(Expect::Clock(expect))) =
            (&item.check.read, Some(&item.check.expect))
        {
            judge_clock(&mut result, name, expect, clock_report.as_ref());
            results.push(result);
            continue;
        }
        let value = per_check(read_value(probe, &item))?;
        match value {
            Ok(value) => {
                result.value = Some(value);
                let (resolved, _) = item.expect.as_ref().expect("resolved without error");
                result.status = if resolved.holds(value) {
                    Status::Pass
                } else {
                    Status::Fail
                };
            }
            Err(error) => result.error = Some(error.message),
        }
        results.push(result);
    }

    Ok(Report {
        description: file.description.clone(),
        variant: options.variant.clone(),
        dry_run: options.dry_run,
        halted,
        skipped,
        results,
    })
}

/// Fill in the result of a `read.clock` check from the evaluated tree.
fn judge_clock(
    result: &mut CheckResult,
    name: &str,
    expect: &ClockExpect,
    report: Option<&Result<clock::Report, DebugError>>,
) {
    let clock = match report {
        Some(Ok(report)) => report.clocks.iter().find(|clock| clock.name == name),
        Some(Err(error)) => {
            result.error = Some(format!(
                "the clock tree could not be read: {}",
                error.message
            ));
            return;
        }
        None => None,
    };
    let Some(clock) = clock else {
        result.error = Some(format!("clock {name} was not evaluated"));
        return;
    };
    result.text = Some(describe_clock(clock));
    if let clock::State::Error(reason) = &clock.state {
        result.error = Some(reason.clone());
        return;
    }
    if let clock::State::Hz(hz) = clock.state {
        result.value = Some(hz.round() as u64);
    }
    result.status = if expect.mismatches(clock).is_empty() {
        Status::Pass
    } else {
        Status::Fail
    };
}

fn read_value(probe: &mut dyn Probe, item: &Prepared) -> DResult<u64> {
    match &item.check.read {
        ReadSpec::Reg(_) => probe::read_register(probe, item.register.as_ref().unwrap()),
        ReadSpec::Addr(address) => probe::read_long(probe, address),
        ReadSpec::Core(name) => probe::eval_u64(probe, &format!("Register({name})")),
        ReadSpec::Expr(expression) => probe::eval_u64(probe, expression),
        ReadSpec::Clock(_) => unreachable!("clock checks are judged by judge_clock"),
    }
}

/// The expectation as written, for checks whose symbols did not resolve.
fn describe_expect(expect: &Expect) -> String {
    let operand = |operand: &Operand| match operand {
        Operand::Int(value) => hex32(*value),
        Operand::Symbol { name, offset: 0 } => format!("sym:{name}"),
        Operand::Symbol { name, offset } => format!("sym:{name}{offset:+}"),
    };
    match expect {
        Expect::Eq { value, .. } => format!("== {}", operand(value)),
        Expect::Ne { value, .. } => format!("!= {}", operand(value)),
        Expect::NonZero => "!= 0".into(),
        Expect::Range(low, high) => format!("in [{}, {}]", hex32(*low), hex32(*high)),
        Expect::InSymbol(name) => format!("in {name}"),
        Expect::OneOf(values) => format!(
            "one of [{}]",
            values.iter().map(operand).collect::<Vec<_>>().join(", ")
        ),
        Expect::Clock(expect) => expect.describe(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::probe::fake::FakeProbe;
    use t32rcl::Value;

    const EXAMPLE: &str = include_str!("../../../../docs/check-example.toml");

    fn per_ready(probe: &mut FakeProbe, running: bool) {
        probe.set("SYStem.Mode()", Value::Int(11));
        probe.set("STATE.RUN()", Value::Bool(running));
        probe.set("PER.FILENAME()", Value::Text("perx.per".into()));
    }

    #[test]
    fn parses_every_read_and_expect_form() {
        let file = parse(
            r#"
            description = "demo"
            [[check]]
            name = "a"
            read = { reg = "CTRL" }
            expect = { eq = 0x1234, mask = 0xFF }
            [[check]]
            name = "b"
            read = { addr = "AD:0x100" }
            expect = { ne = 0 }
            [[check]]
            name = "c"
            read = { core = "PC" }
            expect = { in_symbol = "main" }
            [[check]]
            name = "d"
            read = { expr = "Data.Long(AD:0x0)" }
            expect = { nonzero = true }
            [[check]]
            name = "e"
            read = { reg = "BLOCK.CR" }
            expect = { range = [1, 0x10] }
            [[check]]
            name = "f"
            read = { core = "R0" }
            expect = { one_of = [0, "sym:vectors", "sym:vectors+0x10", "0xFFFFFFFF"] }
            [[check]]
            name = "g"
            read = { core = "LR" }
            expect = { eq = "sym:handler-4" }
            variants = ["x", "y"]
            "#,
        )
        .unwrap();
        assert_eq!(file.description.as_deref(), Some("demo"));
        let expects: Vec<&Expect> = file.checks.iter().map(|c| &c.expect).collect();
        assert_eq!(
            expects,
            [
                &Expect::Eq {
                    value: Operand::Int(0x1234),
                    mask: Some(0xFF)
                },
                &Expect::Ne {
                    value: Operand::Int(0),
                    mask: None
                },
                &Expect::InSymbol("main".into()),
                &Expect::NonZero,
                &Expect::Range(1, 0x10),
                &Expect::OneOf(vec![
                    Operand::Int(0),
                    Operand::Symbol {
                        name: "vectors".into(),
                        offset: 0
                    },
                    Operand::Symbol {
                        name: "vectors".into(),
                        offset: 0x10
                    },
                    Operand::Int(0xFFFF_FFFF),
                ]),
                &Expect::Eq {
                    value: Operand::Symbol {
                        name: "handler".into(),
                        offset: -4
                    },
                    mask: None
                },
            ]
        );
        assert_eq!(file.checks[0].read, ReadSpec::Reg("CTRL".into()));
        assert_eq!(
            file.checks[1].read,
            ReadSpec::Addr(TargetAddress::parse("AD:0x100").unwrap())
        );
        assert_eq!(file.checks[2].read, ReadSpec::Core("PC".into()));
        assert_eq!(
            file.checks[3].read,
            ReadSpec::Expr("Data.Long(AD:0x0)".into())
        );
        assert_eq!(
            file.checks[6].variants,
            Some(vec!["x".to_string(), "y".to_string()])
        );
    }

    #[test]
    fn parse_errors_name_the_check() {
        let error = |text: &str| parse(text).unwrap_err().message;
        assert_eq!(
            error(
                "[[check]]\nname = \"a\"\nread = { reg = \"X\", core = \"PC\" }\nexpect = { nonzero = true }"
            ),
            "check #1 (a): read must have exactly one of reg, addr, core, expr, clock"
        );
        assert_eq!(
            error(
                "[[check]]\nname = \"a\"\nread = { addr = \"0x100\" }\nexpect = { nonzero = true }"
            ),
            "check #1 (a): read.addr \"0x100\" must be <access class>:<address>, e.g. AD:0x20000000"
        );
        assert_eq!(
            error("[[check]]\nname = \"a\"\nread = { core = \"PC\" }\nexpect = { eq = 1, ne = 2 }"),
            "check #1 (a): expect must have exactly one of eq, ne, nonzero, range, in_symbol, one_of (plus mask with eq/ne)"
        );
        assert_eq!(
            error("[[check]]\nname = \"a\"\nread = { core = \"PC\" }\nexpect = { eq = \"sym:\" }"),
            "check #1 (a): expect.eq: invalid symbol \"sym:\""
        );
        assert_eq!(
            error("[[check]]\nname = \"a\"\nread = { core = \"PC\" }\nexpect = { range = [2, 1] }"),
            "check #1 (a): expect.range: low is greater than high"
        );
        assert_eq!(
            error(
                "[[check]]\nname = \"a\"\nread = { core = \"PC\" }\nexpect = { nonzero = true }\ntypo = 1"
            ),
            "check #1 (a): unknown key 'typo'"
        );
        assert_eq!(error("description = \"x\""), "no [[check]] entries");
    }

    #[test]
    fn example_file_parses() {
        let file = parse(EXAMPLE).unwrap();
        assert!(file.checks.len() >= 5);
    }

    fn file(text: &str) -> CheckFile {
        parse(text).unwrap()
    }

    #[test]
    fn variant_selection() {
        let file = file(
            r#"
            [[check]]
            name = "all"
            read = { expr = "1" }
            expect = { eq = 1 }
            [[check]]
            name = "only-a"
            read = { expr = "1" }
            expect = { eq = 1 }
            variants = ["a"]
            [[check]]
            name = "a-or-b"
            read = { expr = "1" }
            expect = { eq = 1 }
            variants = ["a", "b"]
            "#,
        );
        let names = |variant: Option<&str>| -> Vec<&str> {
            file.checks
                .iter()
                .filter(|c| c.selected(variant))
                .map(|c| c.name.as_str())
                .collect()
        };
        assert_eq!(names(None), ["all"]);
        assert_eq!(names(Some("a")), ["all", "only-a", "a-or-b"]);
        assert_eq!(names(Some("b")), ["all", "a-or-b"]);
        assert_eq!(names(Some("c")), ["all"]);

        let mut probe = FakeProbe::with(&[("1", Value::Int(1))]);
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file,
            &Options {
                variant: Some("b".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(report.results.len(), 2);
        assert_eq!(report.skipped, 1);
        assert_eq!(
            report.summary(),
            "2 passed, 0 failed, 0 errors, 1 skipped (not in variant \"b\")"
        );
    }

    const MIXED: &str = r#"
        [[check]]
        name = "ctrl"
        read = { reg = "CTRL" }
        expect = { eq = 0x30, mask = 0xF0 }
        [[check]]
        name = "word"
        read = { addr = "AD:0x100" }
        expect = { one_of = [1, "sym:table+4"] }
        [[check]]
        name = "pc"
        read = { core = "PC" }
        expect = { in_symbol = "main" }
    "#;

    fn mixed_probe(running: bool) -> FakeProbe {
        let mut probe = FakeProbe::with(&[
            ("PER.ADDRESS(\".CTRL\")", Value::Text("C15:0x10".into())),
            ("ADDRESS.OFFSET(PER.ADDRESS(\".CTRL\"))", Value::Int(4)),
            ("PER.VALUE(\".CTRL\")", Value::Int(0x1234)),
            ("Data.Long(AD:0x100)", Value::Int(0x2004)),
            ("sYmbol.BEGIN(table)", Value::Text("D:0x2000".into())),
            ("sYmbol.BEGIN(main)", Value::Text("P:0x1000".into())),
            ("sYmbol.END(main)", Value::Text("P:0x10FF".into())),
            ("Register(PC)", Value::Int(0x1010)),
        ]);
        per_ready(&mut probe, running);
        probe
    }

    #[test]
    fn passing_checks_exit_zero() {
        let mut probe = mixed_probe(false);
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(MIXED),
            &Options::default(),
            None,
        )
        .unwrap();
        assert!(report.passed(), "{report:#?}");
        assert_eq!(report.exit_code(), 0);
        assert_eq!(
            report.results[1].expect,
            "one of [0x00000001, table+0x4 (0x00002004)]"
        );
        assert_eq!(report.results[2].expect, "in main [0x00001000..0x000010FF]");
        assert_eq!(probe.commands(), ["PER.Set.CONDitions"]);
    }

    #[test]
    fn a_failing_check_exits_non_zero() {
        let mut probe = mixed_probe(false);
        probe.set("Register(PC)", Value::Int(0x2000));
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(MIXED),
            &Options::default(),
            None,
        )
        .unwrap();
        assert_eq!(report.results[2].status, Status::Fail);
        assert_eq!(report.exit_code(), 3);
        let text = report.human(Style::PLAIN);
        assert!(text.contains("FAIL  pc"), "{text}");
        let coloured = report.human(Style::COLOR);
        assert!(coloured.contains("\x1b[1;31mFAIL \x1b[0m"), "{coloured}");
        assert!(coloured.contains("\x1b[32mok   \x1b[0m"), "{coloured}");
        assert!(
            coloured.ends_with("\x1b[1;31m2 passed, 1 failed, 0 errors\x1b[0m"),
            "{coloured}"
        );
        assert_eq!(crate::style::strip(&coloured), text);
    }

    #[test]
    fn unresolved_symbol_is_an_error_not_a_crash() {
        let mut probe = mixed_probe(false);
        probe.values.remove("sYmbol.BEGIN(main)");
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(MIXED),
            &Options::default(),
            None,
        )
        .unwrap();
        assert_eq!(report.results[2].status, Status::Error);
        assert!(
            report.results[2]
                .error
                .as_deref()
                .unwrap()
                .starts_with("symbol main not found")
        );
        assert!(!probe.log.contains(&"fnc Register(PC)".to_string()));
        assert_eq!(report.exit_code(), 3);
    }

    #[test]
    fn cp15_read_while_running_needs_halt() {
        let mut probe = mixed_probe(true);
        let error = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(MIXED),
            &Options::default(),
            None,
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "the core is running, and 2 check(s) read CP15 or core registers, which needs a \
             halted core (ctrl, pc); stop it with 'tracebridge debug break' or pass --halt"
        );
        assert!(!probe.commands().contains(&"Break"));
        assert!(!probe.log.iter().any(|l| l.starts_with("fnc PER.VALUE")));
    }

    #[test]
    fn halt_option_breaks_first() {
        let mut probe = mixed_probe(true);
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(MIXED),
            &Options {
                halt: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(report.halted);
        assert!(report.human(Style::PLAIN).contains("core halted (Break)"));
        let commands = probe.commands();
        assert_eq!(commands[0], "PER.Set.CONDitions");
        assert_eq!(commands[1], "Break");
        let brk = probe.log.iter().position(|l| l == "cmd Break").unwrap();
        let first_read = probe
            .log
            .iter()
            .position(|l| l == "fnc PER.VALUE(\".CTRL\")")
            .unwrap();
        assert!(brk < first_read);
    }

    #[test]
    fn plain_memory_checks_do_not_need_a_halted_core() {
        let mut probe = mixed_probe(true);
        let file =
            file("[[check]]\nname = \"w\"\nread = { addr = \"AD:0x100\" }\nexpect = { ne = 0 }");
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file,
            &Options::default(),
            None,
        )
        .unwrap();
        assert!(report.passed());
        assert!(probe.commands().is_empty());
    }

    #[test]
    fn dry_run_resolves_without_reading() {
        let mut probe = mixed_probe(true);
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(MIXED),
            &Options {
                dry_run: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(report.passed());
        assert_eq!(report.results[0].read, "reg .CTRL (C15:0x0001)");
        assert!(report.results.iter().all(|r| r.value.is_none()));
        let reads: Vec<&String> = probe
            .log
            .iter()
            .filter(|l| {
                l.starts_with("fnc PER.VALUE")
                    || l.starts_with("fnc Data.Long")
                    || l.starts_with("fnc Register")
                    || l.starts_with("read ")
            })
            .collect();
        assert!(reads.is_empty(), "{reads:?}");
        assert!(
            report
                .summary()
                .starts_with("dry run: 3 resolved, 0 errors")
        );
    }

    /// OSC (board input) and IRC (16 MHz) feed PLL through SEL[0]; PLL runs
    /// at 4 x its source, CORE from PLL with a warning while LOCK[0] is 0.
    const CLOCK_TREE: &str = r#"
        chips = ["DEMO"]
        [reg]
        SEL = "AD:0x100"
        LOCK = "AD:0x104"
        [[clock]]
        name = "OSC"
        [[clock]]
        name = "IRC"
        hz = "16MHz"
        [[clock]]
        name = "PLL"
        select = "SEL[0]"
        sources = { "0" = "IRC", "1" = "OSC" }
        mul = "4"
        [[clock]]
        name = "CORE"
        from = "PLL"
        warn = [{ when = "LOCK[0] == 0", text = "not locked" }]
        [[clock]]
        name = "OFF"
        from = "IRC"
        enable = "0"
    "#;

    const CLOCK_CHECKS: &str = r#"
        [[check]]
        name = "pll"
        read = { clock = "PLL" }
        expect = { hz = 40_000_000, source = "OSC" }
        [[check]]
        name = "core"
        read = { clock = "CORE" }
        expect = { state = "on", warnings = [] }
        [[check]]
        name = "off"
        read = { clock = "OFF" }
        expect = { state = "off" }
        [[check]]
        name = "pll from irc"
        read = { clock = "PLL" }
        expect = { source = "IRC" }
        [[check]]
        name = "ctrl"
        read = { addr = "AD:0x100" }
        expect = { eq = 1 }
    "#;

    #[test]
    fn clock_checks_parse_and_reject_mixed_forms() {
        let parsed = file(CLOCK_CHECKS);
        assert_eq!(parsed.checks[0].read, ReadSpec::Clock("PLL".into()));
        assert_eq!(
            parsed.checks[0].expect,
            Expect::Clock(ClockExpect {
                hz: Some(Hz(40e6)),
                source: Some("OSC".into()),
                ..Default::default()
            })
        );
        assert!(parsed.reads_clocks(None));
        let error = |text: &str| parse(text).unwrap_err().message;
        let check = |read: &str, expect: &str| {
            format!("[[check]]\nname = \"x\"\nread = {read}\nexpect = {expect}\n")
        };
        assert!(
            error(&check(r#"{ clock = "PLL" }"#, "{ eq = 1 }")).ends_with(
                "unknown expectation 'eq' for read.clock (expected state, hz, source, warnings)"
            )
        );
        assert!(
            error(&check(r#"{ addr = "AD:0x0" }"#, "{ hz = 1 }"))
                .ends_with("expect.hz only works with read.clock")
        );
        assert!(error(&check(r#"{ clock = "PLL" }"#, "{}")).ends_with(
            "expect must have at least one of state, hz, source, warnings for read.clock"
        ));
        assert!(
            error(&check(
                r#"{ clock = "PLL" }"#,
                r#"{ state = "off", hz = 1 }"#
            ))
            .ends_with("expect.hz with state \"off\"")
        );
        assert!(
            error(&check(r#"{ clock = "PLL" }"#, r#"{ state = "maybe" }"#))
                .ends_with("expect.state must be \"on\" or \"off\"")
        );
    }

    #[test]
    fn clock_checks_judge_the_evaluated_tree() {
        let tree = clock::parse(CLOCK_TREE).unwrap();
        let inputs = vec![("OSC".to_string(), 10e6)];
        let clocks = Clocks {
            tree: &tree,
            inputs: &inputs,
        };
        let mut probe = FakeProbe::with(&[
            ("Data.Long(AD:0x100)", Value::Int(1)),
            ("Data.Long(AD:0x104)", Value::Int(0)),
            ("STATE.RUN()", Value::Bool(true)),
        ]);
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &file(CLOCK_CHECKS),
            &Options::default(),
            Some(&clocks),
        )
        .unwrap();
        let status: Vec<(&str, Status)> = report
            .results
            .iter()
            .map(|r| (r.name.as_str(), r.status))
            .collect();
        assert_eq!(
            status,
            [
                ("pll", Status::Pass),
                ("core", Status::Fail),
                ("off", Status::Pass),
                ("pll from irc", Status::Fail),
                ("ctrl", Status::Pass),
            ]
        );
        assert_eq!(report.results[0].value, Some(40_000_000));
        assert_eq!(
            report.results[0].text.as_deref(),
            Some("on 40 MHz from OSC")
        );
        assert_eq!(
            report.results[1].text.as_deref(),
            Some("on 40 MHz from PLL ! not locked")
        );
        assert_eq!(report.results[0].expect, "40 MHz, from OSC");
        assert_eq!(report.results[1].expect, "on, no warnings");
        let json = report.to_json();
        assert_eq!(json["results"][0]["hex"], serde_json::Value::Null);
        assert_eq!(json["results"][0]["text"], "on 40 MHz from OSC");
        assert_eq!(json["results"][4]["hex"], "0x00000001");
        // The tree is read once for all clock checks: one read per register.
        let reads = probe
            .log
            .iter()
            .filter(|line| line.as_str() == "fnc Data.Long(AD:0x100)")
            .count();
        assert_eq!(reads, 2, "{:?}", probe.log); // tree + the addr check
        let text = report.human(Style::PLAIN);
        assert!(
            text.contains("FAIL  core          on 40 MHz from PLL ! not locked  on, no warnings"),
            "{text}"
        );

        let unknown = file(
            "[[check]]\nname = \"x\"\nread = { clock = \"NOPE\" }\nexpect = { state = \"on\" }\n",
        );
        let report = run(
            &mut probe,
            &mut PerSnapshot::default(),
            &unknown,
            &Options::default(),
            Some(&clocks),
        )
        .unwrap();
        assert_eq!(
            report.results[0].error.as_deref(),
            Some("clock NOPE is not in the clock tree description")
        );
    }
}
