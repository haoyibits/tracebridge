//! The `debug` commands. Read-only commands (R) change nothing on the target
//! or in the debugger; the others (S) are marked where they are defined.

use std::path::{Path, PathBuf};

use serde_json::json;
use t32rcl::Value;

use super::check;
use super::decode::{self, FaultAddress};
use super::elf;
use super::probe::{
    self, AddressCheck, AddressSource, DResult, DebugError, DebuggerState, PerSnapshot, Probe,
    SymbolRef, TargetAddress, eval_u64, fail, format_value, hex32, read_long, running_error,
    symbolize, value_as_u64,
};
use super::{EXIT_FAILED, Outcome};
use crate::config::Config;

/// PowerView build that introduced PER.Watch, PER.AddWatch and PER.ClearWatch.
pub const PER_WATCH_BUILD: u64 = 176763;
/// Most words `mem` reads in one command.
const MEM_MAX_WORDS: u32 = 4096;

/// What a command runs against.
pub struct Context<'a> {
    pub probe: &'a mut dyn Probe,
    pub per: &'a mut PerSnapshot,
    pub config: &'a Config,
    /// Relative file arguments are resolved against this directory.
    pub cwd: &'a Path,
}

/// A function result, or `None` when TRACE32 cannot evaluate it; a lost
/// connection is still an error.
fn optional(result: t32rcl::Result<Value>) -> DResult<Option<Value>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            let error = DebugError::from(error);
            if error.lost { Err(error) } else { Ok(None) }
        }
    }
}

fn symbol_json(symbol: &Option<SymbolRef>) -> serde_json::Value {
    match symbol {
        Some(symbol) => json!({
            "path": symbol.path,
            "name": symbol.short_name(),
            "offset": symbol.offset,
            "text": symbol.describe(),
        }),
        None => serde_json::Value::Null,
    }
}

fn with_symbol(value: u64, symbol: &Option<SymbolRef>) -> String {
    match symbol {
        Some(symbol) => format!("{}  {}", hex32(value), symbol.describe()),
        None => format!("{}  (no symbol)", hex32(value)),
    }
}

// ------------------------------------------------------------- R: status

/// R: debugger mode, run state, power, CPU; PC and CPSR when halted.
pub fn status(ctx: &mut Context) -> DResult<Outcome> {
    let probe = &mut *ctx.probe;
    let mode = eval_u64(probe, "SYStem.Mode()")?;
    let running = match optional(probe.fnc("STATE.RUN()"))? {
        Some(Value::Bool(value)) => Some(value),
        _ => None,
    };
    let power = match optional(probe.fnc("STATE.POWER()"))? {
        Some(Value::Bool(value)) => Some(value),
        _ => None,
    };
    let cpu = match optional(probe.fnc("SYStem.CPU()"))? {
        Some(Value::Text(text)) => Some(text),
        _ => None,
    };
    let state = DebuggerState { mode, running };

    let mut lines = vec![format!(
        "mode   {} ({mode})",
        decode::system_mode_label(mode)
    )];
    let run_text = match (state.is_up(), running) {
        (true, Some(true)) => "running",
        (true, Some(false)) => "halted",
        _ => "-",
    };
    lines.push(format!("state  {run_text}"));
    lines.push(format!(
        "power  {}",
        match power {
            Some(true) => "on",
            Some(false) => "off",
            None => "-",
        }
    ));
    lines.push(format!("cpu    {}", cpu.as_deref().unwrap_or("-")));

    let mut pc_json = serde_json::Value::Null;
    let mut cpsr_json = serde_json::Value::Null;
    if state.is_up() && running == Some(false) {
        if let Some(pc) = optional(probe.fnc("Register(PP)"))?
            .as_ref()
            .and_then(value_as_u64)
        {
            let symbol = symbolize(probe, pc)?;
            lines.push(format!("pc     {}", with_symbol(pc, &symbol)));
            pc_json = json!({"value": pc, "hex": hex32(pc), "symbol": symbol_json(&symbol)});
        }
        if let Some(cpsr) = optional(probe.fnc("Register(CPSR)"))?
            .as_ref()
            .and_then(value_as_u64)
        {
            let psr = decode::decode_psr(cpsr as u32);
            lines.push(format!("cpsr   {}  {}", hex32(cpsr), psr.describe()));
            cpsr_json = psr.to_json();
        }
    }
    Ok(Outcome::ok(
        lines.join("\n"),
        json!({
            "mode": mode,
            "mode_name": decode::system_mode_name(mode),
            "up": state.is_up(),
            "running": running,
            "power": power,
            "cpu": cpu,
            "pc": pc_json,
            "cpsr": cpsr_json,
        }),
    ))
}

// ---------------------------------------------------------------- R: reg

struct RegEntry {
    name: String,
    path: Option<String>,
    address: Option<String>,
    /// The address column when it needs an explanation.
    shown_address: Option<String>,
    raw_address: Option<String>,
    /// Whether `Data.Long(address)` reads the same register.
    address_check: AddressCheck,
    note: Option<String>,
    value: Option<u64>,
    choice: Option<probe::Choice>,
    error: Option<String>,
}

/// R: registers by PER name (`HSR`, `HSCTLR.C`), full PER path, or raw
/// address (`C15:0x4025`, `AD:0x40000000`).
pub fn reg(ctx: &mut Context, names: &[String]) -> DResult<Outcome> {
    let probe = &mut *ctx.probe;
    let running = probe::core_running(probe)?;
    if names
        .iter()
        .any(|name| TargetAddress::parse(name).is_none())
    {
        ctx.per.ensure(probe)?;
    }
    let mut entries = Vec::new();
    for name in names {
        let mut entry = RegEntry {
            name: name.clone(),
            path: None,
            address: None,
            shown_address: None,
            raw_address: None,
            address_check: AddressCheck::NotApplicable,
            note: None,
            value: None,
            choice: None,
            error: None,
        };
        let result: DResult<()> = (|| {
            if let Some(address) = TargetAddress::parse(name) {
                entry.address = Some(address.to_string());
                if address.is_coprocessor() && running {
                    return Err(running_error(&format!(
                        "{address} (a coprocessor register)"
                    )));
                }
                entry.value = Some(read_long(probe, &address)?);
                return Ok(());
            }
            let register = probe::resolve_register(probe, ctx.per, name)?;
            entry.path = Some(register.path.clone());
            entry.address = Some(register.address.to_string());
            entry.note = register.note.clone();
            entry.raw_address = Some(register.raw.clone());
            if register.address.is_coprocessor() && running {
                return Err(running_error(&format!(
                    "{name} ({}, a coprocessor register)",
                    register.address
                )));
            }
            let value = probe::read_register(probe, &register)?;
            entry.value = Some(value);
            entry.choice = probe::read_choice(probe, ctx.per, &register, value)?;
            entry.address_check = probe::verify_address(probe, &register, value)?;
            match (entry.address_check, register.source) {
                (AddressCheck::Failed, AddressSource::PerAddress) => {
                    entry.shown_address = Some(format!(
                        "{} (PER.ADDRESS text; no command-line address reads this register)",
                        register.raw
                    ));
                }
                (AddressCheck::Failed, AddressSource::PerFile) => {
                    entry.shown_address = Some(format!(
                        "{} (from the PER file; Data.Long there reads another value)",
                        register.address
                    ));
                }
                (AddressCheck::Unconfirmed, _) => {
                    entry.shown_address = Some(format!(
                        "{} (unconfirmed: the value cannot tell)",
                        register.address
                    ));
                }
                _ => {}
            }
            Ok(())
        })();
        if let Err(mut error) = result {
            if error.lost {
                return Err(error);
            }
            if running && entry.value.is_none() && !error.message.contains("core is running") {
                error.message.push_str(" (the core is running)");
            }
            entry.error = Some(error.message);
        }
        entries.push(entry);
    }

    let width = entries.iter().map(|e| e.name.len()).max().unwrap_or(0);
    let lines: Vec<String> = entries
        .iter()
        .map(|entry| {
            let address = entry
                .shown_address
                .clone()
                .or_else(|| entry.address.clone())
                .unwrap_or_default();
            match (&entry.error, entry.value) {
                (Some(error), _) => format!("{:<width$}  error: {error}", entry.name),
                (None, Some(value)) => {
                    let mut line =
                        format!("{:<width$}  {address:<16}  {}", entry.name, hex32(value));
                    if let Some(choice) = &entry.choice {
                        line.push_str(&format!("  \"{}\"", choice.text));
                    }
                    if let Some(note) = &entry.note {
                        line.push_str(&format!("\n{:<width$}  note: {note}", ""));
                    }
                    line
                }
                (None, None) => format!("{:<width$}  {address}", entry.name),
            }
        })
        .collect();
    let failed = entries.iter().any(|e| e.error.is_some());
    let json_entries: Vec<_> = entries
        .iter()
        .map(|e| {
            json!({
                "name": e.name,
                "path": e.path,
                "address": e.address,
                "raw_address": e.raw_address,
                "address_checked": e.address_check.as_json(),
                "address_check": e.address_check.name(),
                "note": e.note,
                "value": e.value,
                "hex": e.value.map(hex32),
                "choice": e.choice.as_ref().map(|c| c.text.clone()),
                "choice_source": e.choice.as_ref().map(|c| {
                    if c.from_per_file { "per_file" } else { "per_value_string" }
                }),
                "error": e.error,
            })
        })
        .collect();
    Ok(Outcome {
        text: lines.join("\n"),
        json: json!({"running": running, "registers": json_entries}),
        code: i32::from(failed),
    })
}

// ---------------------------------------------------------------- R: mem

/// R: `count` 32-bit words at an address or a symbol, read with Data.Long().
pub fn mem(ctx: &mut Context, location: &str, count: u32) -> DResult<Outcome> {
    let probe = &mut *ctx.probe;
    if count == 0 || count > MEM_MAX_WORDS {
        fail!("count must be between 1 and {MEM_MAX_WORDS}");
    }
    let (base, symbol) = match TargetAddress::parse(location) {
        Some(address) => (address, None),
        None => (probe::symbol_address(probe, location)?, Some(location)),
    };
    if base.is_coprocessor() {
        if count > 1 {
            fail!("{base} is a coprocessor register; read it with 'reg {base}'");
        }
        if probe::core_running(probe)? {
            return Err(running_error(&format!("{base} (a coprocessor register)")));
        }
    }
    let mut words = Vec::new();
    for index in 0..u64::from(count) {
        let address = base.offset(index * 4);
        let word = read_long(probe, &address).map_err(|error| {
            if error.lost {
                error
            } else {
                error.context(format!("cannot read {address}"))
            }
        })?;
        words.push(word);
    }
    let mut lines = Vec::new();
    if let Some(symbol) = symbol {
        lines.push(format!("{symbol} = {base}"));
    }
    for (row, chunk) in words.chunks(4).enumerate() {
        let offset = row as u64 * 16;
        let values: Vec<String> = chunk.iter().map(|w| format!("{w:08X}")).collect();
        lines.push(format!(
            "{}  +0x{offset:03X}  {}",
            base.offset(offset),
            values.join(" ")
        ));
    }
    Ok(Outcome::ok(
        lines.join("\n"),
        json!({
            "symbol": symbol,
            "address": base.to_string(),
            "words": words,
            "hex": words.iter().map(|w| format!("0x{w:08X}")).collect::<Vec<_>>(),
        }),
    ))
}

// -------------------------------------------------------------- R: fault

/// Hyp fault registers. These CP15 encodings are architectural (Armv7-A/R
/// with virtualization, Armv8-R AArch32), not board facts.
const HSR: &str = "C15:0x4025";
const HDFAR: &str = "C15:0x4006";
const HIFAR: &str = "C15:0x4206";
const HVBAR: &str = "C15:0x400C";

/// The entry of a vector table other than the active one that holds PC.
struct OtherTable {
    base: u64,
    slot: &'static str,
    symbol: String,
}

/// R: Armv7/Armv8-R AArch32 Hyp fault report.
pub fn fault(ctx: &mut Context) -> DResult<Outcome> {
    let probe = &mut *ctx.probe;
    if let Some(Value::Bool(true)) = optional(probe.fnc("CPUIS64BIT()"))? {
        fail!("fault decodes the AArch32 Hyp mode registers, and this core is 64-bit");
    }
    if probe::core_running(probe)? {
        return Err(running_error(
            "The fault registers (CP15, ELR_hyp, SPSR_hyp)",
        ));
    }
    fn no_hyp(what: &'static str) -> impl Fn(DebugError) -> DebugError {
        move |error| {
            if error.lost {
                error
            } else {
                error.context(format!(
                    "cannot read {what}; fault needs an AArch32 core with Hyp mode"
                ))
            }
        }
    }
    let cp15 = |text: &str| TargetAddress::parse(text).expect("constant address");
    let hsr_raw = read_long(probe, &cp15(HSR)).map_err(no_hyp("HSR (C15:0x4025)"))? as u32;
    let elr = eval_u64(probe, "Register(ELR_HYP)").map_err(no_hyp("ELR_hyp"))?;
    let spsr = eval_u64(probe, "Register(SPSR_HYP)").map_err(no_hyp("SPSR_hyp"))?;
    let hdfar = read_long(probe, &cp15(HDFAR)).map_err(no_hyp("HDFAR (C15:0x4006)"))?;
    let hifar = read_long(probe, &cp15(HIFAR)).map_err(no_hyp("HIFAR (C15:0x4206)"))?;
    let hvbar = read_long(probe, &cp15(HVBAR)).map_err(no_hyp("HVBAR (C15:0x400C)"))?;
    let pc = eval_u64(probe, "Register(PP)")?;

    let hsr = decode::decode_hsr(hsr_raw);
    let spsr_decoded = decode::decode_psr(spsr as u32);
    let pc_symbol = symbolize(probe, pc)?;
    let hvbar_symbol = symbolize(probe, hvbar)?;
    let elr_symbol = symbolize(probe, elr)?;
    let slot = decode::vector_slot(hvbar, pc);
    // PC in another table: a fault taken before the final table is installed
    // stops in the first one. Vector tables are 32-byte aligned.
    let other_table = match (&slot, &pc_symbol) {
        (None, Some(symbol)) => symbol.offset.and_then(|offset| {
            let base = pc - offset;
            let (_, slot) = decode::vector_slot(base, pc)?;
            (base != hvbar && base % 32 == 0).then(|| OtherTable {
                base,
                slot,
                symbol: symbol.short_name().to_string(),
            })
        }),
        _ => None,
    };

    let mut lines = Vec::new();
    let table_name = hvbar_symbol
        .as_ref()
        .map(|s| format!(" ({})", s.describe()))
        .unwrap_or_default();
    lines.push(format!("PC        {}", with_symbol(pc, &pc_symbol)));
    match (&slot, &other_table) {
        (Some((offset, name)), _) => lines.push(format!(
            "          hyp vector \"{name}\": HVBAR {}{table_name} + 0x{offset:02X}",
            hex32(hvbar)
        )),
        (None, Some(other)) => lines.push(format!(
            "          \"{}\" entry of {} at {}, which is NOT the active table \
             (HVBAR {}{table_name})",
            other.slot,
            other.symbol,
            hex32(other.base),
            hex32(hvbar)
        )),
        (None, None) => lines.push(format!(
            "          not at a hyp vector (HVBAR {}{table_name})",
            hex32(hvbar)
        )),
    }
    // HSR = 0 would decode as "EC 0x00 unknown reason"; it means that no
    // exception has been taken to Hyp mode.
    let recorded = hsr_raw != 0;
    if recorded {
        lines.push(format!("HSR       {}", hex32(hsr_raw.into())));
        lines.extend(hsr.describe().into_iter().map(|line| format!("  {line}")));
    } else {
        lines.push("HSR       0x00000000  HSR = 0: no exception recorded".into());
    }
    let fault_address = if recorded {
        hsr.fault_address()
    } else {
        FaultAddress::None
    };
    match fault_address {
        FaultAddress::Hdfar => lines.push(format!(
            "HDFAR     {}  (faulting data address)",
            hex32(hdfar)
        )),
        FaultAddress::Hifar => lines.push(format!(
            "HIFAR     {}  (faulting instruction address)",
            hex32(hifar)
        )),
        FaultAddress::None => {}
    }
    lines.push(format!("ELR_hyp   {}", with_symbol(elr, &elr_symbol)));
    lines.push(format!(
        "SPSR_hyp  {}  {}",
        hex32(spsr),
        spsr_decoded.describe()
    ));

    Ok(Outcome::ok(
        lines.join("\n"),
        json!({
            "pc": {"value": pc, "hex": hex32(pc), "symbol": symbol_json(&pc_symbol)},
            "hvbar": {"value": hvbar, "hex": hex32(hvbar), "symbol": symbol_json(&hvbar_symbol)},
            "vector": slot.map(|(offset, name)| json!({"offset": offset, "name": name})),
            "other_table": other_table.as_ref().map(|other| json!({
                "base": other.base,
                "hex": hex32(other.base),
                "symbol": other.symbol,
                "slot": other.slot,
            })),
            "exception_recorded": recorded,
            "hsr": if recorded { hsr.to_json() } else { json!({"raw": 0, "hex": "0x00000000"}) },
            "fault_address_register": match fault_address {
                FaultAddress::Hdfar => Some("HDFAR"),
                FaultAddress::Hifar => Some("HIFAR"),
                FaultAddress::None => None,
            },
            "hdfar": {"value": hdfar, "hex": hex32(hdfar)},
            "hifar": {"value": hifar, "hex": hex32(hifar)},
            "elr_hyp": {"value": elr, "hex": hex32(elr), "symbol": symbol_json(&elr_symbol)},
            "spsr_hyp": spsr_decoded.to_json(),
        }),
    ))
}

// ---------------------------------------------------------------- R: eval

/// R: the value of any PRACTICE expression.
pub fn eval(ctx: &mut Context, expression: &str) -> DResult<Outcome> {
    let value = probe::eval(ctx.probe, expression)?;
    Ok(Outcome::ok(
        format_value(&value),
        json!({"expression": expression, "result": probe::value_to_json(&value)}),
    ))
}

// -------------------------------------------------------------- R: verify

fn resolve_file(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// R: compare the ELF's `PT_LOAD` content with target memory at the load
/// addresses; `t32` also asks TRACE32 (`Data.LOAD.Elf /DIFF`).
pub fn verify(ctx: &mut Context, elf_path: Option<&Path>, t32: bool) -> DResult<Outcome> {
    let path = match elf_path {
        Some(path) => resolve_file(ctx.cwd, path),
        None => ctx.config.elf.clone(),
    };
    let bytes = std::fs::read(&path)
        .map_err(|error| DebugError::new(format!("cannot read {}: {error}", path.display())))?;
    let segments = elf::load_segments(&bytes)
        .map_err(|error| DebugError::new(format!("{}: {error}", path.display())))?;
    if segments.is_empty() {
        fail!("{}: no loadable segments with file content", path.display());
    }
    let results = elf::compare(ctx.probe, &segments)?;

    let mut lines = vec![path.display().to_string()];
    for result in &results {
        let runs = if result.vaddr != result.paddr {
            format!("  (runs at 0x{:08X})", result.vaddr)
        } else {
            String::new()
        };
        let verdict = match result.first_difference {
            None => "match".to_string(),
            Some(first) => format!(
                "{} bytes differ, first at AD:0x{first:08X}",
                result.differing
            ),
        };
        lines.push(format!(
            "  AD:0x{:08X}  {:>8} bytes{runs}  {verdict}",
            result.paddr, result.size
        ));
    }
    let differing: usize = results.iter().map(|r| r.differing).sum();
    let first = results.iter().find_map(|r| r.first_difference);
    let matched = differing == 0;
    lines.push(match first {
        None => "match".to_string(),
        Some(first) => format!("MISMATCH: {differing} bytes differ, first at AD:0x{first:08X}"),
    });

    let mut t32_json = serde_json::Value::Null;
    let mut t32_matched = true;
    if t32 {
        let text = path.to_string_lossy();
        if text.contains(['"', '\n', '\r']) {
            fail!("ELF path is unsafe for a TRACE32 command: {text}");
        }
        // /DIFF compares without changing memory; /PHYSLOAD compares at
        // p_paddr; /NoRegister, /NosYmbol and /NoClear leave PC and the
        // loaded symbols alone.
        ctx.probe.cmd(&format!(
            "Data.LOAD.Elf \"{text}\" /DIFF /PHYSLOAD /NoRegister /NosYmbol /NoClear"
        ))?;
        let found = probe::eval_bool(ctx.probe, "FOUND()")?;
        t32_matched = !found;
        let address = if found {
            Some(probe::eval_text(ctx.probe, "TRACK.ADDRESS()")?)
        } else {
            None
        };
        lines.push(match &address {
            None => "TRACE32 /DIFF: match".to_string(),
            Some(address) => format!("TRACE32 /DIFF: difference at {address}"),
        });
        if t32_matched != matched {
            lines.push("note: TRACE32 /DIFF and the LMA comparison disagree".into());
        }
        t32_json = json!({"match": t32_matched, "first_difference": address});
    }
    let segments_json: Vec<_> = results
        .iter()
        .map(|r| {
            json!({
                "paddr": r.paddr,
                "vaddr": r.vaddr,
                "size": r.size,
                "differing": r.differing,
                "first_difference": r.first_difference,
            })
        })
        .collect();
    Ok(Outcome {
        text: lines.join("\n"),
        json: json!({
            "elf": path.display().to_string(),
            "match": matched,
            "differing_bytes": differing,
            "first_difference": first,
            "segments": segments_json,
            "trace32_diff": t32_json,
        }),
        code: if matched && t32_matched {
            0
        } else {
            EXIT_FAILED
        },
    })
}

// --------------------------------------------------------------- check

/// R without `halt`, S with it (it may stop the core).
pub fn check(ctx: &mut Context, file: &Path, options: &check::Options) -> DResult<Outcome> {
    let path = resolve_file(ctx.cwd, file);
    let parsed = check::load(&path)?;
    let report = check::run(ctx.probe, ctx.per, &parsed, options)?;
    Ok(Outcome {
        text: report.human(),
        json: report.to_json(),
        code: report.exit_code(),
    })
}

// --------------------------------------------------------------- UI: watch

/// Names from a watch file: one per line, `#` comments.
pub fn watch_names(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// UI: a PER.Watch window with exactly these registers.
pub fn watch(ctx: &mut Context, items: &[String]) -> DResult<Outcome> {
    let names = match items {
        [single] if resolve_file(ctx.cwd, Path::new(single)).is_file() => {
            let path = resolve_file(ctx.cwd, Path::new(single));
            let text = std::fs::read_to_string(&path).map_err(|error| {
                DebugError::new(format!("cannot read {}: {error}", path.display()))
            })?;
            watch_names(&text)
        }
        _ => items.to_vec(),
    };
    if names.is_empty() {
        fail!("no register names to watch");
    }
    let probe = &mut *ctx.probe;
    let build = eval_u64(probe, "VERSION.BUILD()")?;
    if build < PER_WATCH_BUILD {
        fail!(
            "watch needs PowerView build {PER_WATCH_BUILD} (09/2025) or newer for PER.Watch; \
             this is build {build}"
        );
    }
    ctx.per.ensure(probe)?;
    let mut paths = Vec::new();
    let mut errors = Vec::new();
    for name in &names {
        match probe::resolve_register(probe, ctx.per, name) {
            Ok(register) => paths.push(register.path),
            Err(error) if error.lost => return Err(error),
            Err(error) => errors.push(error.message),
        }
    }
    if !errors.is_empty() {
        fail!("{} (the watch window was not changed)", errors.join("; "));
    }
    probe.cmd("PER.ClearWatch")?;
    for path in &paths {
        probe.cmd(&format!("PER.AddWatch {path}"))?;
    }
    probe.cmd("PER.Watch")?;
    Ok(Outcome::ok(
        format!("PER.Watch window with {}", paths.join(" ")),
        json!({"paths": paths}),
    ))
}

// ------------------------------------------------------ S: state changes

fn after_change(ctx: &mut Context, done: &str, command: &str) -> DResult<Outcome> {
    ctx.per.invalidate();
    let state = DebuggerState::read(ctx.probe)?;
    Ok(Outcome::ok(
        format!("{done} (now: {})", state.label()),
        json!({"command": command, "state": state.label(), "mode": state.mode, "running": state.running}),
    ))
}

/// S: `SYStem.Mode Attach` without a reset; the core keeps its run state.
pub fn attach(ctx: &mut Context) -> DResult<Outcome> {
    if probe::eval_bool(ctx.probe, "SYStem.Up()")? {
        let state = DebuggerState::read(ctx.probe)?;
        return Ok(Outcome::ok(
            format!("already attached (now: {})", state.label()),
            json!({"command": null, "state": state.label(), "mode": state.mode, "running": state.running}),
        ));
    }
    let commands = crate::target::attach_commands(ctx.config);
    for command in &commands {
        ctx.probe.cmd(command)?;
    }
    after_change(
        ctx,
        "attached without reset; PowerView reports attach as mode up",
        &commands.join("; "),
    )
}

/// S: `SYStem.Down`, `Break`, `Go` or any command.
pub fn command(ctx: &mut Context, command: &str, done: &str) -> DResult<Outcome> {
    if command.trim().is_empty() {
        fail!("no command given");
    }
    ctx.probe.cmd(command)?;
    after_change(ctx, done, command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::probe::fake::FakeProbe;
    use crate::target::tests::make_config;

    fn run<T>(probe: &mut FakeProbe, f: impl FnOnce(&mut Context) -> T) -> T {
        let dir = tempfile::tempdir().unwrap();
        let config = make_config(dir.path());
        let mut per = PerSnapshot::default();
        let mut ctx = Context {
            probe,
            per: &mut per,
            config: &config,
            cwd: dir.path(),
        };
        f(&mut ctx)
    }

    fn halted_hyp_probe() -> FakeProbe {
        FakeProbe::with(&[
            ("PER.FILENAME()", Value::Text("perx.per".into())),
            ("SYStem.Mode()", Value::Int(11)),
            ("STATE.RUN()", Value::Bool(false)),
            ("STATE.POWER()", Value::Bool(true)),
            ("SYStem.CPU()", Value::Text("CORTEXR52".into())),
            ("CPUIS64BIT()", Value::Bool(false)),
            ("Register(PP)", Value::Int(0x1030)),
            ("Register(CPSR)", Value::Int(0x6000_01FA)),
            ("Register(ELR_HYP)", Value::Int(0x2006)),
            ("Register(SPSR_HYP)", Value::Int(0x0000_01FA)),
            ("Data.Long(C15:0x4025)", Value::Int(0x9600_004C)),
            ("Data.Long(C15:0x4006)", Value::Int(0x0800_0000)),
            ("Data.Long(C15:0x4206)", Value::Int(0x2006)),
            ("Data.Long(C15:0x400C)", Value::Int(0x1020)),
            (
                "sYmbol.NAME(P:0x1030)",
                Value::Text("\\\\app\\Global\\vectors_b".into()),
            ),
            (
                "ADDRESS.OFFSET(sYmbol.BEGIN(\\\\app\\Global\\vectors_b))",
                Value::Int(0x1020),
            ),
            (
                "sYmbol.NAME(P:0x1020)",
                Value::Text("\\\\app\\Global\\vectors_b".into()),
            ),
            (
                "sYmbol.NAME(P:0x2006)",
                Value::Text("\\\\app\\Global\\main".into()),
            ),
            (
                "ADDRESS.OFFSET(sYmbol.BEGIN(\\\\app\\Global\\main))",
                Value::Int(0x2000),
            ),
        ])
    }

    #[test]
    fn status_when_halted_decodes_pc_and_cpsr() {
        let mut probe = halted_hyp_probe();
        let outcome = run(&mut probe, status).unwrap();
        assert!(outcome.text.contains("mode   up (11)"), "{}", outcome.text);
        assert!(outcome.text.contains("state  halted"));
        assert!(outcome.text.contains("pc     0x00001030  vectors_b+0x10"));
        assert!(
            outcome
                .text
                .contains("cpsr   0x600001FA  hyp, T=1 (Thumb), masked: A I F, flags: Z C")
        );
        assert_eq!(outcome.json["cpsr"]["mode_name"], "hyp");
        assert!(probe.commands().is_empty());
    }

    #[test]
    fn status_when_down_reads_no_registers() {
        let mut probe = FakeProbe::with(&[("SYStem.Mode()", Value::Int(0))]);
        let outcome = run(&mut probe, status).unwrap();
        assert!(outcome.text.contains("mode   down (0)"));
        assert!(outcome.text.contains("state  -"));
        assert!(!probe.log.iter().any(|l| l.contains("Register(")));
    }

    #[test]
    fn fault_report_via_the_active_table() {
        let mut probe = halted_hyp_probe();
        let outcome = run(&mut probe, fault).unwrap();
        let text = &outcome.text;
        assert!(
            text.contains("hyp vector \"data abort\": HVBAR 0x00001020 (vectors_b) + 0x10"),
            "{text}"
        );
        assert!(text.contains("EC    0x25  data abort, same EL"));
        assert!(text.contains("IL not valid (RES1)"));
        assert!(text.contains("DFSC  0b001100  permission fault"));
        assert!(text.contains("HDFAR     0x08000000"));
        assert!(!text.contains("HIFAR"));
        assert!(text.contains("ELR_hyp   0x00002006  main+0x6"));
        assert_eq!(outcome.json["vector"]["name"], "data abort");
        assert!(probe.commands().is_empty());
    }

    #[test]
    fn fault_in_a_table_that_is_not_active() {
        let mut probe = halted_hyp_probe();
        // HVBAR still points at the first table; PC is in the second one.
        probe.set("Data.Long(C15:0x400C)", Value::Int(0x0F00));
        probe.set(
            "sYmbol.NAME(P:0xF00)",
            Value::Text("\\\\app\\Global\\vectors_a".into()),
        );
        probe.set(
            "ADDRESS.OFFSET(sYmbol.BEGIN(\\\\app\\Global\\vectors_a))",
            Value::Int(0xF00),
        );
        let outcome = run(&mut probe, fault).unwrap();
        assert!(
            outcome.text.contains(
                "\"data abort\" entry of vectors_b at 0x00001020, which is NOT the active table \
                 (HVBAR 0x00000F00 (vectors_a))"
            ),
            "{}",
            outcome.text
        );
    }

    #[test]
    fn fault_refuses_a_running_core() {
        let mut probe = halted_hyp_probe();
        probe.set("STATE.RUN()", Value::Bool(true));
        let error = run(&mut probe, fault).unwrap_err();
        assert!(
            error
                .message
                .contains("can only be read while the core is halted")
        );
        assert!(probe.commands().is_empty());
    }

    /// A C15 register at c15:0x1001 as PowerView reports it: PER.ADDRESS()
    /// text that cannot be pasted back, and an offset of 4 × the address.
    fn c15_register(probe: &mut FakeProbe) {
        for path in [".CTRL", ".CTRL.EN"] {
            probe.set(
                &format!("PER.ADDRESS(\"{path}\")"),
                Value::Text("C15:0x10010".into()),
            );
            probe.set(
                &format!("ADDRESS.OFFSET(PER.ADDRESS(\"{path}\"))"),
                Value::Int(0x1001 * 4),
            );
        }
        probe.set("PER.VALUE(\".CTRL\")", Value::Int(0x1234));
        probe.set("PER.VALUE(\".CTRL.EN\")", Value::Int(1));
        probe.set(
            "PER.VALUE.STRING(\".CTRL.EN\")",
            Value::Text("Enabled".into()),
        );
        probe.set("Data.Long(C15:0x1001)", Value::Int(0x1234));
    }

    #[test]
    fn reg_reads_names_fields_and_addresses() {
        let mut probe = halted_hyp_probe();
        c15_register(&mut probe);
        probe.set("Data.Long(C15:0x1F12)", Value::Int(0xABCD));
        let names = ["CTRL", "CTRL.EN", "C15:0x1F12", "NOPE"].map(String::from);
        let outcome = run(&mut probe, |ctx| reg(ctx, &names)).unwrap();
        let lines: Vec<&str> = outcome.text.lines().collect();
        assert_eq!(lines[0], "CTRL        C15:0x1001        0x00001234");
        // A field: the self-check compares with its register (.CTRL).
        assert_eq!(
            lines[1],
            "CTRL.EN     C15:0x1001        0x00000001  \"Enabled\""
        );
        assert_eq!(lines[2], "C15:0x1F12  C15:0x1F12        0x0000ABCD");
        assert!(lines[3].starts_with("NOPE        error: NOPE: not found in the PER file"));
        assert_eq!(outcome.code, 1);
        assert_eq!(outcome.json["registers"][0]["address_checked"], true);
        assert_eq!(outcome.json["registers"][1]["address_checked"], true);
        assert_eq!(probe.commands(), ["PER.Set.CONDitions"]);
        assert!(probe.log.contains(&"fnc Data.Long(C15:0x1001)".to_string()));
    }

    #[test]
    fn reg_prints_the_raw_address_when_the_self_check_fails() {
        let mut probe = halted_hyp_probe();
        c15_register(&mut probe);
        // The converted address reads a different register.
        probe.set("Data.Long(C15:0x1001)", Value::Int(0x131));
        let names = ["CTRL".to_string()];
        let outcome = run(&mut probe, |ctx| reg(ctx, &names)).unwrap();
        assert_eq!(
            outcome.text,
            "CTRL  C15:0x10010 (PER.ADDRESS text; no command-line address reads this \
             register)  0x00001234"
        );
        assert_eq!(outcome.json["registers"][0]["address_checked"], false);
        assert_eq!(outcome.code, 0);
    }

    #[test]
    fn memory_mapped_registers_need_no_self_check() {
        let mut probe = halted_hyp_probe();
        probe.set("PER.ADDRESS(\".CR\")", Value::Text("AD:0x40020000".into()));
        probe.set(
            "ADDRESS.OFFSET(PER.ADDRESS(\".CR\"))",
            Value::Int(0x4002_0000),
        );
        probe.set("PER.VALUE(\".CR\")", Value::Int(0x11A));
        let names = ["CR".to_string()];
        let outcome = run(&mut probe, |ctx| reg(ctx, &names)).unwrap();
        assert_eq!(outcome.text, "CR  AD:0x40020000     0x0000011A");
        assert!(!probe.log.iter().any(|l| l.contains("Data.Long")));
    }

    #[test]
    fn reg_loads_the_default_per_file_when_per_functions_say_so() {
        let mut probe = halted_hyp_probe();
        c15_register(&mut probe);
        // PER.FILENAME() names a file even though none is loaded.
        probe.errors.insert(
            "PER.Set.CONDitions".into(),
            "No default peripheral file (PER.ReProgram) found.".into(),
        );
        let names = ["CTRL".to_string()];
        let outcome = run(&mut probe, |ctx| reg(ctx, &names)).unwrap();
        assert_eq!(outcome.code, 0, "{}", outcome.text);
        assert_eq!(
            probe.commands(),
            ["PER.Set.CONDitions", "PER.ReProgram", "PER.Set.CONDitions"]
        );
        // Raw addresses need no PER file.
        let mut probe = halted_hyp_probe();
        probe.set("Data.Long(C15:0x1F12)", Value::Int(1));
        let names = ["C15:0x1F12".to_string()];
        run(&mut probe, |ctx| reg(ctx, &names)).unwrap();
        assert!(probe.commands().is_empty());
    }

    /// A C14 register at c14:0x0070 with the value `value`.
    fn c14_register(probe: &mut FakeProbe, value: i128) {
        probe.set("PER.ADDRESS(\".VCR\")", Value::Text("C14:0x700".into()));
        probe.set("ADDRESS.OFFSET(PER.ADDRESS(\".VCR\"))", Value::Int(0x1C0));
        probe.set("PER.VALUE(\".VCR\")", Value::Int(value));
        probe.set("Data.Long(C14:0x0070)", Value::Int(value));
    }

    #[test]
    fn a_zero_value_cannot_confirm_the_address() {
        let mut probe = halted_hyp_probe();
        c14_register(&mut probe, 0);
        let names = ["VCR".to_string()];
        let outcome = run(&mut probe, |ctx| reg(ctx, &names)).unwrap();
        assert_eq!(
            outcome.text,
            "VCR  C14:0x0070 (unconfirmed: the value cannot tell)  0x00000000"
        );
        let register = &outcome.json["registers"][0];
        assert_eq!(register["address"], "C14:0x0070");
        assert_eq!(register["address_checked"], serde_json::Value::Null);
        assert_eq!(register["address_check"], "unconfirmed");

        let mut probe = halted_hyp_probe();
        c14_register(&mut probe, 0x0004_4000);
        let outcome = run(&mut probe, |ctx| reg(ctx, &names)).unwrap();
        assert_eq!(outcome.text, "VCR  C14:0x0070        0x00044000");
        assert_eq!(outcome.json["registers"][0]["address_checked"], true);
    }

    #[test]
    fn a_bus_error_fails_the_check_but_not_the_command() {
        let mut probe = halted_hyp_probe();
        c14_register(&mut probe, 0x1234);
        probe.values.remove("Data.Long(C14:0x0070)");
        probe.errors.insert(
            "Data.Long(C14:0x0070)".into(),
            "bus error at address EC14:0x70".into(),
        );
        let names = ["VCR".to_string()];
        let outcome = run(&mut probe, |ctx| reg(ctx, &names)).unwrap();
        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.json["registers"][0]["address_check"], "failed");
        assert_eq!(outcome.json["registers"][0]["value"], 0x1234);
        assert!(
            outcome.text.contains("C14:0x700 (PER.ADDRESS text"),
            "{}",
            outcome.text
        );
    }

    #[test]
    fn rgroup_registers_are_read_with_per_value() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("perx.per"),
            "tree \"Timer\"\n  rgroup.long c15:0x000E++0x00\n    line.long 0x00 \"FRQ,Frequency\"\ntree.end\n",
        )
        .unwrap();
        let config = crate::target::tests::make_config(dir.path());
        let mut probe = halted_hyp_probe();
        probe.errors.insert(
            "PER.ADDRESS(\".FRQ\")".into(),
            "internal error : PAR_256".into(),
        );
        probe.set("PER.VALUE(\".FRQ\")", Value::Int(0x3B9A_CA00));
        probe.set("Data.Long(C15:0x000E)", Value::Int(0x3B9A_CA00));
        let mut per = PerSnapshot::new(Some(dir.path().to_path_buf()));
        let mut ctx = Context {
            probe: &mut probe,
            per: &mut per,
            config: &config,
            cwd: dir.path(),
        };
        let names = ["FRQ".to_string()];
        let outcome = reg(&mut ctx, &names).unwrap();
        assert_eq!(outcome.text, "FRQ  C15:0x000E        0x3B9ACA00");
        let register = &outcome.json["registers"][0];
        assert_eq!(register["address_check"], "confirmed");
        assert_eq!(register["raw_address"], "(address from the PER file)");
    }

    #[test]
    fn bitfld_text_comes_from_the_per_file_when_trace32_refuses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("perx.per"),
            "ENUMDELIMITER \",\"\ntree \"Watchdog\"\n  group.long ad:0x40000000++0x3\n    \
             line.long 0x0 \"CR,Control\"\n      bitfld.long 0x0 0. \"WEN,Enable\" \"Disabled,Enabled\"\n\
             tree.end\n",
        )
        .unwrap();
        let config = crate::target::tests::make_config(dir.path());
        let mut probe = halted_hyp_probe();
        probe.set(
            "PER.ADDRESS(\".CR.WEN\")",
            Value::Text("AD:0x40000000".into()),
        );
        probe.set(
            "ADDRESS.OFFSET(PER.ADDRESS(\".CR.WEN\"))",
            Value::Int(0x4000_0000),
        );
        probe.set("PER.VALUE(\".CR.WEN\")", Value::Int(1));
        // What TRACE32 answers for every BITFLD tried on hardware.
        probe.errors.insert(
            "PER.VALUE.STRING(\".CR.WEN\")".into(),
            "Must be a BITFLD".into(),
        );
        let mut per = PerSnapshot::new(Some(dir.path().to_path_buf()));
        let mut ctx = Context {
            probe: &mut probe,
            per: &mut per,
            config: &config,
            cwd: dir.path(),
        };
        let names = ["CR.WEN".to_string()];
        let outcome = reg(&mut ctx, &names).unwrap();
        assert_eq!(
            outcome.text,
            "CR.WEN  AD:0x40000000     0x00000001  \"Enabled\""
        );
        assert_eq!(outcome.json["registers"][0]["choice"], "Enabled");
        assert_eq!(outcome.json["registers"][0]["choice_source"], "per_file");
    }

    #[test]
    fn duplicate_definitions_are_read_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("perx.per"),
            "tree \"A\"\n  group.long c15:0x1001++0x00\n    line.long 0x00 \"CTRL,Control\"\ntree.end\n\
             tree \"B\"\n  group.long c15:0x1001++0x00\n    line.long 0x00 \"CTRL,Control\"\ntree.end\n",
        )
        .unwrap();
        let config = crate::target::tests::make_config(dir.path());
        let mut probe = halted_hyp_probe();
        probe.errors.insert(
            "PER.ADDRESS(\".CTRL\")".into(),
            "Ambiguous keyword 'CTRL'".into(),
        );
        probe.set("PER.ADDRESS(\"A.CTRL\")", Value::Text("C15:0x10010".into()));
        probe.set(
            "ADDRESS.OFFSET(PER.ADDRESS(\"A.CTRL\"))",
            Value::Int(0x1001 * 4),
        );
        probe.set("PER.VALUE(\"A.CTRL\")", Value::Int(0x1234));
        probe.set("Data.Long(C15:0x1001)", Value::Int(0x1234));
        let mut per = PerSnapshot::new(Some(dir.path().to_path_buf()));
        let mut ctx = Context {
            probe: &mut probe,
            per: &mut per,
            config: &config,
            cwd: dir.path(),
        };
        let names = ["CTRL".to_string()];
        let outcome = reg(&mut ctx, &names).unwrap();
        assert_eq!(
            outcome.text,
            "CTRL  C15:0x1001        0x00001234\n      \
             note: defined 2 times in the PER file, all at C15:0x1001; read as 'A.CTRL'"
        );
        assert_eq!(outcome.code, 0);
    }

    #[test]
    fn fault_without_an_exception() {
        let mut probe = halted_hyp_probe();
        probe.set("Data.Long(C15:0x4025)", Value::Int(0));
        // The boot ROM's ELR, far above the image's last symbol.
        probe.set("Register(ELR_HYP)", Value::Int(0x29FB_81A1));
        probe.set(
            "sYmbol.NAME(P:0x29FB81A1)",
            Value::Text("\\\\app\\Global\\__record_start".into()),
        );
        probe.set(
            "ADDRESS.OFFSET(sYmbol.BEGIN(\\\\app\\Global\\__record_start))",
            Value::Int(0x29F8_7E00),
        );
        let outcome = run(&mut probe, fault).unwrap();
        let text = &outcome.text;
        assert!(
            text.contains("HSR       0x00000000  HSR = 0: no exception recorded"),
            "{text}"
        );
        assert!(!text.contains("EC "), "{text}");
        assert!(!text.contains("HDFAR"), "{text}");
        assert!(text.contains("ELR_hyp   0x29FB81A1  (no symbol)"), "{text}");
        assert_eq!(outcome.json["exception_recorded"], false);
    }

    #[test]
    fn reg_says_clearly_that_cp15_needs_a_halted_core() {
        let mut probe = halted_hyp_probe();
        probe.set("STATE.RUN()", Value::Bool(true));
        probe.set("PER.ADDRESS(\".CTRL\")", Value::Text("C15:0x1001".into()));
        let names = ["CTRL".to_string()];
        let outcome = run(&mut probe, |ctx| reg(ctx, &names)).unwrap();
        assert!(
            outcome
                .text
                .contains("can only be read while the core is halted"),
            "{}",
            outcome.text
        );
        assert!(!probe.log.iter().any(|l| l.contains("PER.VALUE")));
        assert!(!probe.commands().contains(&"Break"));
    }

    #[test]
    fn mem_reads_words_at_a_symbol() {
        let mut probe = FakeProbe::with(&[
            ("sYmbol.BEGIN(record)", Value::Text("SD:0x100".into())),
            ("Data.Long(SD:0x100)", Value::Int(1)),
            ("Data.Long(SD:0x104)", Value::Int(2)),
            ("Data.Long(SD:0x108)", Value::Int(3)),
            ("Data.Long(SD:0x10C)", Value::Int(4)),
            ("Data.Long(SD:0x110)", Value::Int(5)),
        ]);
        let outcome = run(&mut probe, |ctx| mem(ctx, "record", 5)).unwrap();
        assert_eq!(
            outcome.text,
            "record = SD:0x100\n\
             SD:0x100  +0x000  00000001 00000002 00000003 00000004\n\
             SD:0x110  +0x010  00000005"
        );
    }

    #[test]
    fn watch_names_skip_comments() {
        assert_eq!(
            watch_names("# boot registers\nHSCTLR\n\n  HSR  # syndrome\n"),
            ["HSCTLR", "HSR"]
        );
    }

    #[test]
    fn watch_needs_a_recent_build_and_fills_the_window() {
        let mut probe = halted_hyp_probe();
        probe.set("VERSION.BUILD()", Value::Int(170000));
        let names = ["CTRL".to_string()];
        let error = run(&mut probe, |ctx| watch(ctx, &names)).unwrap_err();
        assert!(
            error
                .message
                .starts_with("watch needs PowerView build 176763")
        );

        probe.set("VERSION.BUILD()", Value::Int(190766));
        probe.set("PER.ADDRESS(\".CTRL\")", Value::Text("AD:0x1000".into()));
        run(&mut probe, |ctx| watch(ctx, &names)).unwrap();
        assert_eq!(
            probe.commands(),
            [
                "PER.Set.CONDitions",
                "PER.ClearWatch",
                "PER.AddWatch .CTRL",
                "PER.Watch"
            ]
        );
    }

    #[test]
    fn attach_when_down_uses_the_attach_sequence() {
        let mut probe = FakeProbe::with(&[
            ("SYStem.Up()", Value::Bool(false)),
            ("SYStem.Mode()", Value::Int(11)),
            ("STATE.RUN()", Value::Bool(true)),
        ]);
        let outcome = run(&mut probe, attach).unwrap();
        assert_eq!(
            probe.commands(),
            [
                "SYStem.Mode Down",
                "SYStem.CPU CPU",
                "CORE.ASSIGN 1.",
                "SYStem.Mode Attach"
            ]
        );
        assert!(outcome.text.ends_with("(now: up, running)"));
        let mut probe = FakeProbe::with(&[
            ("SYStem.Up()", Value::Bool(true)),
            ("SYStem.Mode()", Value::Int(11)),
            ("STATE.RUN()", Value::Bool(false)),
        ]);
        run(&mut probe, attach).unwrap();
        assert!(probe.commands().is_empty());
    }

    #[test]
    fn verify_compares_at_the_lma() {
        let mut probe = FakeProbe::default();
        probe.write(0x100, b"CODE");
        probe.write(0x488, b"DATA");
        let dir = tempfile::tempdir().unwrap();
        let elf = dir.path().join("app.elf");
        std::fs::write(
            &elf,
            elf::tests::elf32(
                &[(0x100, 0x100, "CODE", 4), (0x488, 0x2000_0000, "DATA", 4)],
                None,
            ),
        )
        .unwrap();
        let outcome = run(&mut probe, |ctx| verify(ctx, Some(&elf), false)).unwrap();
        assert_eq!(outcome.code, 0);
        assert!(outcome.text.ends_with("\nmatch"), "{}", outcome.text);
        assert!(outcome.text.contains("(runs at 0x20000000)"));

        probe.write(0x489, b"x");
        let outcome = run(&mut probe, |ctx| verify(ctx, Some(&elf), false)).unwrap();
        assert_eq!(outcome.code, EXIT_FAILED);
        assert!(
            outcome
                .text
                .ends_with("MISMATCH: 1 bytes differ, first at AD:0x00000489")
        );
    }
}
