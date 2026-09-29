//! A text scan of the PER file. It supplies paths, addresses and BITFLD
//! choice texts where TRACE32's PER functions do not: the full paths of an
//! ambiguous or partial name, the address of `rgroup` entries (PER.ADDRESS()
//! fails on them), and the text of a BITFLD value (PER.VALUE.STRING() fails
//! with "Must be a BITFLD"). Values always come from TRACE32 (PER.VALUE(),
//! Data.Long()).
//!
//! Only `tree`, `tree.open`, `tree.close` and `tree.end` build the path,
//! `base` with a literal address and `group`-style lines give the address.
//! `sif`/`if` are not interpreted, so a candidate may belong to a branch that
//! is inactive for this core.

use std::path::{Path, PathBuf};

use super::probe::TargetAddress;

/// Where a register name occurs in the PER file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The full path in PER.Set.ByName syntax, quoted where needed.
    pub path: String,
    /// The address, when the group and base are literal addresses.
    pub address: Option<TargetAddress>,
    /// Defined in an `rgroup` (read-only group).
    pub read_only: bool,
    /// For `REG.FIELD`: the field's definition line (whitespace collapsed),
    /// to tell whether duplicate definitions describe the same field.
    pub field_definition: Option<String>,
    /// For a BITFLD: its choice texts, value 0 first.
    pub choices: Option<Vec<String>>,
    /// The tree names above the register, outermost first.
    pub trees: Vec<String>,
}

/// Quote a path element when it is not a plain name.
fn element(name: &str) -> String {
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        name.to_string()
    } else {
        format!("\"{name}\"")
    }
}

/// The first `"..."` of a line.
fn quoted(text: &str) -> Option<&str> {
    quoted_all(text).into_iter().next()
}

/// Every `"..."` of a line.
fn quoted_all(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else { break };
        found.push(&after[..end]);
        rest = &after[end + 1..];
    }
    found
}

/// The choices of a `bitfld` line: its second quoted string, split at the
/// file's ENUMDELIMITER.
fn bitfld_choices(line: &str, delimiter: &str) -> Option<Vec<String>> {
    let kind = line.split(['.', ' ', '\t']).next()?.to_ascii_lowercase();
    if !kind.ends_with("bitfld") {
        return None;
    }
    let choices = *quoted_all(line).get(1)?;
    Some(
        choices
            .split(delimiter)
            .map(|choice| choice.trim().to_string())
            .collect(),
    )
}

/// The text for `value` from a BITFLD's choices. `?...` marks the rest as
/// reserved, `?` a single reserved value.
pub fn choice_for(choices: &[String], value: u64) -> Option<String> {
    let index = usize::try_from(value).ok()?;
    for (position, choice) in choices.iter().enumerate() {
        if choice.starts_with("?...") {
            return None;
        }
        if position == index {
            return (!choice.is_empty() && choice != "?").then(|| choice.clone());
        }
    }
    None
}

/// `c15:0x4001` or `ad:0x70F40000`, with the class in upper case.
fn literal_address(token: &str) -> Option<TargetAddress> {
    let address = TargetAddress::parse(token)?;
    (!address.class.is_empty()).then(|| TargetAddress {
        class: address.class.to_ascii_uppercase(),
        value: address.value,
    })
}

fn number(token: &str) -> Option<u64> {
    let token = token.trim();
    match token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
    {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => token.strip_suffix('.').unwrap_or(token).parse().ok(),
    }
}

/// `group.long c15:0x4001++0x00`, `rgroup.long 0x0++0xF`, ...
fn group_start(line: &str) -> Option<(&str, bool)> {
    let (keyword, rest) = line.split_once(char::is_whitespace)?;
    let kind = keyword.split('.').next()?;
    if !kind.ends_with("group") || !kind.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    let token = rest.split_whitespace().next()?;
    let start = token.split("++").next()?.split("--").next()?;
    Some((start, kind == "rgroup"))
}

struct Tree {
    name: String,
    base: Option<TargetAddress>,
}

/// Every definition of register `register` (the text before the comma of a
/// `line.*` label). `field` is appended to the paths.
pub fn find(text: &str, register: &str, field: Option<&str>) -> Vec<Candidate> {
    let mut trees: Vec<Tree> = Vec::new();
    let mut top_base: Option<TargetAddress> = None;
    let mut group: Option<(Option<TargetAddress>, bool)> = None;
    let mut found: Vec<Candidate> = Vec::new();
    // The last candidate waits for the definition of `field`.
    let mut awaiting_field = false;
    let mut delimiter = ",".to_string();
    for raw in text.lines() {
        let line = raw.trim();
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("enumdelimiter") {
            if let Some(value) = quoted(line).filter(|value| !value.is_empty()) {
                delimiter = value.to_string();
            }
            continue;
        }
        if awaiting_field {
            let kind = lower.split(['.', ' ', '\t']).next().unwrap_or("");
            if kind.ends_with("fld") || kind == "hexmask" {
                let label = quoted(line).and_then(|label| label.split(',').next());
                if label.map(str::trim) == field {
                    if let Some(candidate) = found.last_mut() {
                        candidate.field_definition =
                            Some(line.split_whitespace().collect::<Vec<_>>().join(" "));
                        candidate.choices = bitfld_choices(line, &delimiter);
                    }
                    awaiting_field = false;
                }
                continue;
            }
            if lower.starts_with("line.")
                || lower.starts_with("tree")
                || group_start(line).is_some()
            {
                awaiting_field = false;
            }
        }
        if lower.starts_with("tree.end") {
            trees.pop();
            group = None;
            continue;
        }
        if lower.starts_with("tree") && !lower.starts_with("tree.end") {
            let keyword = lower.split_whitespace().next().unwrap_or("");
            if matches!(keyword, "tree" | "tree.open" | "tree.close") {
                if let Some(name) = quoted(line) {
                    let base = trees.last().map_or(top_base.clone(), |t| t.base.clone());
                    trees.push(Tree {
                        name: name.to_string(),
                        base,
                    });
                    group = None;
                }
                continue;
            }
        }
        if let Some(rest) = lower.strip_prefix("base ") {
            let base = literal_address(rest.trim());
            match trees.last_mut() {
                Some(tree) => tree.base = base,
                None => top_base = base,
            }
            continue;
        }
        if let Some((start, read_only)) = group_start(line) {
            let address = literal_address(start).or_else(|| {
                let offset = number(start)?;
                let base = trees.last().map_or(top_base.clone(), |t| t.base.clone())?;
                Some(base.offset(offset))
            });
            group = Some((address, read_only));
            continue;
        }
        if lower.starts_with("line.") {
            let Some(label) = quoted(line) else { continue };
            let name = label.split(',').next().unwrap_or("").trim();
            if name != register {
                continue;
            }
            let offset = line.split_whitespace().nth(1).and_then(number);
            let (group_address, read_only) = group.clone().unwrap_or((None, false));
            let address = match (group_address, offset) {
                (Some(address), Some(offset)) => Some(address.offset(offset)),
                _ => None,
            };
            let mut path: Vec<String> = trees.iter().map(|t| element(&t.name)).collect();
            path.push(element(register));
            if let Some(field) = field {
                path.push(element(field));
            }
            found.push(Candidate {
                path: path.join("."),
                address,
                read_only,
                field_definition: None,
                choices: None,
                trees: trees.iter().map(|t| t.name.clone()).collect(),
            });
            awaiting_field = field.is_some();
        }
    }
    // Definitions repeated in the branches of a condition count once.
    let mut unique: Vec<Candidate> = Vec::new();
    for candidate in found {
        if !unique.contains(&candidate) {
            unique.push(candidate);
        }
    }
    unique
}

/// Split a user name into (register, field) when it is `NAME` or
/// `NAME.FIELD` (optionally with a leading dot); full paths are not scanned.
pub fn register_and_field(name: &str) -> Option<(&str, Option<&str>)> {
    let name = name.trim().strip_prefix('.').unwrap_or(name.trim());
    if name.contains('"') {
        return None;
    }
    let mut parts = name.split('.');
    let register = parts.next().filter(|r| !r.is_empty())?;
    let field = parts.next();
    if parts.next().is_some() || field.is_some_and(str::is_empty) {
        return None;
    }
    Some((register, field))
}

/// The PER file PowerView reports (`PER.FILENAME()`), which may be a bare
/// name in the TRACE32 system directory.
pub fn locate(filename: &str, system_dir: Option<&Path>) -> Option<PathBuf> {
    let path = Path::new(filename.trim());
    if filename.trim().is_empty() {
        return None;
    }
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    let joined = system_dir?.join(path);
    joined.is_file().then_some(joined)
}

fn read(file: &Path) -> Option<String> {
    std::fs::read(file)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Scan a PER file; unreadable files give no candidates.
pub fn scan(file: &Path, name: &str) -> Vec<Candidate> {
    let Some((register, field)) = register_and_field(name) else {
        return Vec::new();
    };
    match read(file) {
        Some(text) => find(&text, register, field),
        None => Vec::new(),
    }
}

/// A PER path as its elements, quotes removed, and whether it starts with a
/// dot (a search of the whole file).
pub fn split_path(path: &str) -> (bool, Vec<String>) {
    let path = path.trim();
    let (searched, path) = match path.strip_prefix('.') {
        Some(rest) => (true, rest),
        None => (false, path),
    };
    let mut elements = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in path.chars() {
        match c {
            '"' => quoted = !quoted,
            '.' if !quoted => elements.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    elements.push(current);
    (searched, elements)
}

/// `needle` occurs in `haystack` in order (not necessarily adjacent).
fn is_subsequence(needle: &[String], haystack: &[String]) -> bool {
    let mut rest = haystack.iter();
    needle
        .iter()
        .all(|wanted| rest.any(|element| element == wanted))
}

/// The definitions that a path of the form `.TREE.REG` or `.TREE.REG.FIELD`
/// was probably meant for. TRACE32 does not accept such paths: a leading dot
/// is followed by a register name or `REG.FIELD` only.
pub fn find_partial(text: &str, name: &str) -> Vec<Candidate> {
    let (_, elements) = split_path(name);
    let count = elements.len();
    let mut found = Vec::new();
    // `.TREE....REG` and `.TREE....REG.FIELD`.
    for field_last in [false, true] {
        let register_index = if field_last {
            count.checked_sub(2)
        } else {
            count.checked_sub(1)
        };
        let Some(register_index) = register_index.filter(|&index| index >= 1) else {
            continue;
        };
        let field = field_last.then(|| elements[count - 1].as_str());
        let trees = &elements[..register_index];
        for candidate in find(text, &elements[register_index], field) {
            let has_field = field.is_none() || candidate.field_definition.is_some();
            if has_field && is_subsequence(trees, &candidate.trees) && !found.contains(&candidate) {
                found.push(candidate);
            }
        }
    }
    found
}

/// `find_partial` on a PER file.
pub fn scan_partial(file: &Path, name: &str) -> Vec<Candidate> {
    match read(file) {
        Some(text) => find_partial(&text, name),
        None => Vec::new(),
    }
}

/// The BITFLD choice text of `value` for the field at `path` (`.REG.FIELD`
/// or a full path). All matching definitions must agree on the choices.
pub fn field_choice(text: &str, path: &str, value: u64) -> Option<String> {
    let (searched, elements) = split_path(path);
    let count = elements.len();
    if count < 2 {
        return None;
    }
    let (register, field) = (&elements[count - 2], &elements[count - 1]);
    let candidates: Vec<Candidate> = find(text, register, Some(field))
        .into_iter()
        .filter(|candidate| {
            if searched && count == 2 {
                return true;
            }
            candidate.trees.len() == count - 2 && candidate.trees[..] == elements[..count - 2]
        })
        .collect();
    let choices = candidates.first()?.choices.clone()?;
    if candidates
        .iter()
        .any(|c| c.choices.as_ref() != Some(&choices))
    {
        return None;
    }
    choice_for(&choices, value)
}

/// `field_choice` on a PER file.
pub fn scan_field_choice(file: &Path, path: &str, value: u64) -> Option<String> {
    field_choice(&read(file)?, path, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PER: &str = r#"
; made-up peripheral file
base ad:0x0
sif (CORENAME()=="COREX")
tree "Core Registers (Core X)"
tree "System Control"
    group.long c15:0x1001++0x00
        line.long 0x00 "SCTLR,System Control Register"
            bitfld.long 0x00 2. "    C   ,Cache enable" "Off,On"
tree.end
tree.open "Hyp Registers"
    rgroup.long c15:0x000E++0x00
        line.long 0x00 "CNTFRQ,Counter Frequency"
    group.long c15:0x1001++0x00
        line.long 0x00 "SCTLR,System Control Register"
    if (((per.l(c15:0x2025))&0xFC000000)==0x0)
    group.long c15:0x2025++0x00
        line.long 0x00 "SYN,Syndrome"
    else
    group.long c15:0x2025++0x00
        line.long 0x00 "SYN,Syndrome"
    endif
tree.end
tree.end
endif
tree "TMR (Timer Unit)"
    base ad:0x40010000
    tree "TMR_0"
        base ad:0x40020000
        group.long 0x0++0xF
            line.long 0x0 "CR,Control"
            line.long 0x4 "SR,Status"
    tree.end
    tree "TMR_1"
        group.long 0x10++0xF
            line.long 0x0 "CR,Control"
    tree.end
tree.end
tree "GIC"
    base COMP.BASE("GICD",-1.)
    group.long 0x0++0x3
        line.long 0x0 "CR,Distributor control"
tree.end
ENUMDELIMITER ";"
tree "Clock"
    group.long 0x100++0x3
        line.long 0x0 "CKSEL,Clock select"
            bitfld.long 0x0 0.--2. "  SRC  ,Source" "Off;PLL, fast;?;XTAL;?..."
            hexmask.long.word 0x0 16.--31. 1. "DIV,Divider"
tree.end
"#;

    fn address(text: &str) -> Option<TargetAddress> {
        TargetAddress::parse(text)
    }

    #[test]
    fn lists_every_tree_path_of_an_ambiguous_name() {
        let found = find(PER, "SCTLR", None);
        let paths: Vec<&str> = found.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "\"Core Registers (Core X)\".\"System Control\".SCTLR",
                "\"Core Registers (Core X)\".\"Hyp Registers\".SCTLR",
            ]
        );
        assert!(found.iter().all(|c| c.address == address("C15:0x1001")));
        let found = find(PER, "SCTLR", Some("C"));
        assert!(found[0].path.ends_with(".SCTLR.C"));
    }

    #[test]
    fn duplicates_in_condition_branches_are_listed_once() {
        assert_eq!(find(PER, "SYN", None).len(), 1);
    }

    #[test]
    fn read_only_groups_and_bases() {
        let found = find(PER, "CNTFRQ", None);
        assert_eq!(found.len(), 1);
        assert!(found[0].read_only);
        assert_eq!(found[0].address, address("C15:0xE"));

        let found = find(PER, "CR", None);
        let described: Vec<(String, Option<String>)> = found
            .iter()
            .map(|c| (c.path.clone(), c.address.as_ref().map(ToString::to_string)))
            .collect();
        assert_eq!(
            described,
            [
                (
                    "\"TMR (Timer Unit)\".TMR_0.CR".to_string(),
                    Some("AD:0x40020000".to_string())
                ),
                (
                    "\"TMR (Timer Unit)\".TMR_1.CR".to_string(),
                    Some("AD:0x40010010".to_string())
                ),
                // An expression base gives no address.
                ("GIC.CR".to_string(), None),
            ]
        );
        assert_eq!(find(PER, "SR", None)[0].address, address("AD:0x40020004"));
    }

    #[test]
    fn names_that_can_be_scanned() {
        assert_eq!(register_and_field("HSCTLR"), Some(("HSCTLR", None)));
        assert_eq!(register_and_field(".HSCTLR.C"), Some(("HSCTLR", Some("C"))));
        assert_eq!(register_and_field("A.B.C"), None);
        assert_eq!(register_and_field("\"A B\".C"), None);
    }

    #[test]
    fn bitfld_choices_follow_the_enum_delimiter() {
        let found = find(PER, "CKSEL", Some("SRC"));
        assert_eq!(
            found[0].choices,
            Some(
                ["Off", "PLL, fast", "?", "XTAL", "?..."]
                    .map(String::from)
                    .to_vec()
            )
        );
        assert_eq!(find(PER, "CKSEL", Some("DIV"))[0].choices, None);
        let found = find(PER, "SCTLR", Some("C"));
        assert_eq!(
            found[0].choices,
            Some(vec!["Off".to_string(), "On".to_string()])
        );
    }

    #[test]
    fn choice_texts_by_value() {
        assert_eq!(field_choice(PER, ".CKSEL.SRC", 0).as_deref(), Some("Off"));
        assert_eq!(
            field_choice(PER, ".CKSEL.SRC", 1).as_deref(),
            Some("PLL, fast")
        );
        // "?" is one reserved value, "?..." all the rest.
        assert_eq!(field_choice(PER, ".CKSEL.SRC", 2), None);
        assert_eq!(field_choice(PER, ".CKSEL.SRC", 3).as_deref(), Some("XTAL"));
        assert_eq!(field_choice(PER, ".CKSEL.SRC", 4), None);
        assert_eq!(field_choice(PER, ".CKSEL.SRC", 9), None);
        // A full path picks its own definition...
        assert_eq!(
            field_choice(
                PER,
                "\"Core Registers (Core X)\".\"System Control\".SCTLR.C",
                1
            )
            .as_deref(),
            Some("On")
        );
        // ...while a search needs every definition to agree (here the Hyp
        // copy of SCTLR has no fields).
        assert_eq!(field_choice(PER, ".SCTLR.C", 1), None);
        assert_eq!(field_choice(PER, ".CKSEL", 1), None);
    }

    #[test]
    fn partial_paths_find_what_they_probably_meant() {
        let paths = |name: &str| -> Vec<String> {
            find_partial(PER, name)
                .into_iter()
                .map(|c| c.path)
                .collect()
        };
        assert_eq!(paths(".TMR_0.CR"), ["\"TMR (Timer Unit)\".TMR_0.CR"]);
        assert_eq!(
            find_partial(PER, ".TMR_0.CR")[0].address,
            address("AD:0x40020000")
        );
        // Trees may be skipped.
        assert_eq!(
            paths(".\"TMR (Timer Unit)\".CR"),
            [
                "\"TMR (Timer Unit)\".TMR_0.CR",
                "\"TMR (Timer Unit)\".TMR_1.CR"
            ]
        );
        assert_eq!(paths(".Clock.CKSEL.SRC"), ["Clock.CKSEL.SRC"]);
        // No such field, no such tree, or nothing partial about it.
        assert!(paths(".TMR_0.CR.NOPE").is_empty());
        assert!(paths(".NOPE.CR").is_empty());
        assert!(paths(".CR").is_empty());
    }

    #[test]
    fn paths_split_into_elements() {
        assert_eq!(
            split_path(".\"A (B.C)\".D.E"),
            (true, vec!["A (B.C)".to_string(), "D".into(), "E".into()])
        );
        assert_eq!(
            split_path("A.B"),
            (false, vec!["A".to_string(), "B".into()])
        );
    }

    #[test]
    fn locates_bare_names_in_the_system_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("perx.per"), PER).unwrap();
        assert_eq!(
            locate("perx.per", Some(dir.path())),
            Some(dir.path().join("perx.per"))
        );
        assert_eq!(locate("", Some(dir.path())), None);
        assert_eq!(locate("missing.per", Some(dir.path())), None);
        assert_eq!(scan(&dir.path().join("perx.per"), "CNTFRQ").len(), 1);
    }
}
