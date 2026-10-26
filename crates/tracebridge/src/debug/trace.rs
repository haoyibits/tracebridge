//! The newest records of the trace that is already set up in PowerView, read
//! through `Trace.EXPORT.Ascii` (for `debug trace` and `debug fault --trace`).
//!
//! tracebridge sets up no trace: the trace sink, the ETM and their board
//! settings belong in the project's own script (`debug do`). This only exports
//! what `Trace.METHOD` has recorded, whatever sink that is.
//!
//! The export lists one line per trace record that holds program flow,
//! with these columns, separated by white space:
//!
//! ```text
//! -00000000002995     HT:280004E0        33.    679681. *l\rustvisor_el2_exception+0x44
//! record              address        CLOCKS.Back CLOCKS.Zero  sYmbol (cut to its column)
//! ```
//!
//! The address is hexadecimal without `0x`, after its access class. A record
//! without a predecessor has no `CLOCKS.Back`, so a line with one number before
//! the symbol has `CLOCKS.Zero` only. TRACE32 cuts long symbols at the front
//! (`*` marks the cut), so the symbols shown are looked up again through
//! [`symbolize`]. Lines that do not start with a record number (none with these
//! items, but the default items add disassembly lines) are skipped.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::probe::{DResult, DebugError, Probe, SymbolRef, eval_u64, fail, symbolize};
use crate::style::Style;

/// Where the export is written, inside the run directory; it is kept so that
/// the raw listing can be looked at.
pub const EXPORT_FILE: &str = "trace.ad";

/// The export items. `List.NoDummy` leaves out records without program flow.
const EXPORT_ITEMS: &str = "Address CLOCKS.Back CLOCKS.Zero sYmbol List.NoDummy /ShowRecord";

/// One exported trace record.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// Record number; the newest record is -1.
    pub record: i64,
    /// Access class as TRACE32 shows it, e.g. `HT`.
    pub access: String,
    pub address: u64,
    /// Cycles since the previous record (`CLOCKS.Back`).
    pub clocks_back: Option<u64>,
    /// Cycles since TRACE32's zero point (`CLOCKS.Zero`).
    pub clocks_zero: Option<u64>,
    pub symbol: Option<SymbolRef>,
}

/// The newest records and where their export is.
#[derive(Debug, Clone, PartialEq)]
pub struct Dump {
    /// Records in the trace buffer.
    pub available: u64,
    pub records: Vec<Record>,
    pub file: PathBuf,
}

/// `123.` as TRACE32 writes a decimal number.
fn decimal(token: &str) -> Option<u64> {
    let digits = token.strip_suffix('.')?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// One line of the export, `None` for lines that are not a record.
pub fn parse_line(line: &str) -> Option<Record> {
    let mut tokens = line.split_whitespace();
    let record = tokens.next()?;
    let digits = record.strip_prefix('-').unwrap_or(record);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let record: i64 = record.parse().ok()?;
    let (access, address) = tokens.next()?.split_once(':')?;
    if access.is_empty() || !access.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let address = u64::from_str_radix(address, 16).ok()?;
    let numbers: Vec<u64> = tokens.map_while(decimal).take(2).collect();
    let (clocks_back, clocks_zero) = match numbers[..] {
        [back, zero] => (Some(back), Some(zero)),
        [zero] => (None, Some(zero)),
        _ => (None, None),
    };
    Some(Record {
        record,
        access: access.to_string(),
        address,
        clocks_back,
        clocks_zero,
        symbol: None,
    })
}

/// Export the newest `last` records of a halted core's trace and look up
/// their symbols. `run_dir` must exist.
pub fn read(probe: &mut dyn Probe, run_dir: &Path, last: u32) -> DResult<Dump> {
    if last == 0 {
        fail!("the number of records must be at least 1");
    }
    let available = eval_u64(probe, "Trace.RECORDS()").map_err(|error| {
        if error.lost {
            error
        } else {
            error.context("cannot read the trace (Trace.RECORDS()); is a trace method set up?")
        }
    })?;
    if available == 0 {
        fail!(
            "the trace is empty: set it up (Trace.METHOD, the trace sink, ETM.ON) and run \
             the core, e.g. with 'tracebridge debug do <script>'"
        );
    }
    let count = available.min(u64::from(last));
    let file = run_dir.join(EXPORT_FILE);
    let path = file.display().to_string();
    if path.contains('"') {
        fail!("the run directory must not contain a double quote: {path}");
    }
    // A failed export must not leave an older listing to be read.
    match std::fs::remove_file(&file) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => fail!("cannot remove {path}: {error}"),
    }
    let command = format!("Trace.EXPORT.Ascii \"{path}\" (-{count}.)--(-1.) {EXPORT_ITEMS}");
    probe
        .cmd(&command)
        .map_err(|error| DebugError::from(error).context(command.clone()))?;
    let text = std::fs::read(&file).map_err(|error| {
        DebugError::new(format!("cannot read {path} after the export: {error}"))
    })?;
    let mut records: Vec<Record> = String::from_utf8_lossy(&text)
        .lines()
        .filter_map(parse_line)
        .collect();
    let mut symbols: HashMap<u64, Option<SymbolRef>> = HashMap::new();
    for record in &mut records {
        record.symbol = match symbols.entry(record.address) {
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(entry) => entry.insert(symbolize(probe, record.address)?).clone(),
        };
    }
    Ok(Dump {
        available,
        records,
        file,
    })
}

impl Record {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "record": self.record,
            "access": self.access,
            "address": self.address,
            "hex": format!("0x{:08X}", self.address),
            "clocks_back": self.clocks_back,
            "clocks_zero": self.clocks_zero,
            "symbol": self.symbol.as_ref().map(|symbol| json!({
                "path": symbol.path,
                "name": symbol.short_name(),
                "offset": symbol.offset,
                "text": symbol.describe(),
            })),
        })
    }
}

impl Dump {
    /// One line per record: number, address, cycles since the previous
    /// record, symbol.
    pub fn lines(&self, style: Style) -> Vec<String> {
        let width = self
            .records
            .iter()
            .map(|r| r.record.to_string().len())
            .max()
            .unwrap_or(0);
        self.records
            .iter()
            .map(|r| {
                let back = match r.clocks_back {
                    Some(back) => format!("{back:>6}"),
                    None => format!("{:>6}", "-"),
                };
                let symbol = match &r.symbol {
                    Some(symbol) => style.symbol(symbol.describe()),
                    None => style.dim("(no symbol)"),
                };
                format!(
                    "{}  {}  {}  {symbol}",
                    style.dim(format!("{:>width$}", r.record)),
                    style.address(format!("{}:{:08X}", r.access, r.address)),
                    back
                )
            })
            .collect()
    }

    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "available": self.available,
            "file": self.file.display().to_string(),
            "records": self.records.iter().map(Record::to_json).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_columns_of_a_real_export() {
        // Lines from an export on a Cortex-R52 (ETM through the ETR).
        let first = parse_line(
            "-00000000002981     HT:280004F0               677951. *l\\rustvisor_el2_exception+0x4C",
        )
        .unwrap();
        assert_eq!(
            first,
            Record {
                record: -2981,
                access: "HT".into(),
                address: 0x2800_04F0,
                clocks_back: None,
                clocks_zero: Some(677_951),
                symbol: None,
            }
        );
        let next = parse_line(
            "-00000000002995     HT:280004E0        33.    679681. *l\\rustvisor_el2_exception+0x44",
        )
        .unwrap();
        assert_eq!(next.clocks_back, Some(33));
        assert_eq!(next.clocks_zero, Some(679_681));
        // An address of decimal digits only is still hexadecimal.
        let guest = parse_line(
            "-00000000002927     ST:28010570         0.    677951. \\\\guest\\Global\\guest_main+0x47C",
        )
        .unwrap();
        assert_eq!(guest.access, "ST");
        assert_eq!(guest.address, 0x2801_0570);
        assert_eq!(guest.clocks_back, Some(0));
    }

    #[test]
    fn skips_lines_that_are_not_records() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line(" ldr     r0,[r13]"), None);
        assert_eq!(parse_line("2: b.w     0x280100C0"), None);
        assert_eq!(parse_line("-000001   *     HT:280004D8 ptrace"), None);
        // No timing items: the address alone.
        let bare = parse_line("-1 P:1000").unwrap();
        assert_eq!((bare.clocks_back, bare.clocks_zero), (None, None));
    }
}
