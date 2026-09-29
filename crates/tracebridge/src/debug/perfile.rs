//! A text scan of the PER file, used **only** to make lookup errors
//! actionable: for an ambiguous name it lists the full paths that TRACE32
//! would accept, and for entries TRACE32's PER functions cannot resolve it
//! suggests the address. Lookups themselves always go through PER.ADDRESS()
//! and PER.VALUE().
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
    let start = text.find('"')? + 1;
    let end = text[start..].find('"')? + start;
    Some(&text[start..end])
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
    for raw in text.lines() {
        let line = raw.trim();
        let lower = line.to_ascii_lowercase();
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
            let candidate = Candidate {
                path: path.join("."),
                address,
                read_only,
            };
            if !found.contains(&candidate) {
                found.push(candidate);
            }
        }
    }
    found
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

/// Scan a PER file; unreadable files give no candidates.
pub fn scan(file: &Path, name: &str) -> Vec<Candidate> {
    let Some((register, field)) = register_and_field(name) else {
        return Vec::new();
    };
    match std::fs::read(file) {
        Ok(bytes) => find(&String::from_utf8_lossy(&bytes), register, field),
        Err(_) => Vec::new(),
    }
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
