//! `verify`: compare the ELF's loadable content with target memory.
//!
//! Only the program headers matter: every `PT_LOAD` segment with file
//! content is compared at its load address (LMA, `p_paddr`), not at its run
//! address (VMA, `p_vaddr`). A segment that is copied to RAM at startup
//! (`.data`) lives in NVM at its LMA; comparing at the VMA would compare
//! against live RAM that the program has changed.

use t32rcl::Address;

use super::probe::{DResult, DebugError, Probe};

const PT_LOAD: u32 = 1;

/// One loadable segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Load address (`p_paddr`).
    pub paddr: u64,
    /// Run address (`p_vaddr`), for display only.
    pub vaddr: u64,
    /// The `p_filesz` bytes from the file.
    pub data: Vec<u8>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    little: bool,
}

impl Reader<'_> {
    fn get(&self, offset: usize, size: usize) -> Result<&[u8], String> {
        offset
            .checked_add(size)
            .and_then(|end| self.bytes.get(offset..end))
            .ok_or_else(|| "truncated ELF file".to_string())
    }

    fn u16(&self, offset: usize) -> Result<u64, String> {
        let b: [u8; 2] = self.get(offset, 2)?.try_into().unwrap();
        Ok(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        } as u64)
    }

    fn u32(&self, offset: usize) -> Result<u64, String> {
        let b: [u8; 4] = self.get(offset, 4)?.try_into().unwrap();
        Ok(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        } as u64)
    }

    fn u64(&self, offset: usize) -> Result<u64, String> {
        let b: [u8; 8] = self.get(offset, 8)?.try_into().unwrap();
        Ok(if self.little {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        })
    }
}

fn index(value: u64) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| "ELF offset out of range".to_string())
}

/// The `PT_LOAD` segments with `p_filesz > 0` (NOLOAD and `.bss`-only
/// segments have nothing to compare).
pub fn load_segments(bytes: &[u8]) -> Result<Vec<Segment>, String> {
    if bytes.len() < 16 || &bytes[..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    let wide = match bytes[4] {
        1 => false,
        2 => true,
        other => return Err(format!("unknown ELF class {other}")),
    };
    let little = match bytes[5] {
        1 => true,
        2 => false,
        other => return Err(format!("unknown ELF data encoding {other}")),
    };
    let r = Reader { bytes, little };
    let (phoff, phentsize, phnum) = if wide {
        (r.u64(0x20)?, r.u16(0x36)?, r.u16(0x38)?)
    } else {
        (r.u32(0x1C)?, r.u16(0x2A)?, r.u16(0x2C)?)
    };
    let mut segments = Vec::new();
    for number in 0..phnum {
        let base = index(phoff + number * phentsize)?;
        let (p_type, offset, vaddr, paddr, filesz) = if wide {
            (
                r.u32(base)?,
                r.u64(base + 8)?,
                r.u64(base + 16)?,
                r.u64(base + 24)?,
                r.u64(base + 32)?,
            )
        } else {
            (
                r.u32(base)?,
                r.u32(base + 4)?,
                r.u32(base + 8)?,
                r.u32(base + 12)?,
                r.u32(base + 16)?,
            )
        };
        if p_type as u32 != PT_LOAD || filesz == 0 {
            continue;
        }
        let data = r.get(index(offset)?, index(filesz)?)?.to_vec();
        segments.push(Segment { paddr, vaddr, data });
    }
    Ok(segments)
}

/// The comparison of one segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentResult {
    pub paddr: u64,
    pub vaddr: u64,
    pub size: usize,
    pub differing: usize,
    pub first_difference: Option<u64>,
}

/// Read every segment at `AD:<p_paddr>` and compare it byte by byte.
pub fn compare(probe: &mut dyn Probe, segments: &[Segment]) -> DResult<Vec<SegmentResult>> {
    let mut results = Vec::new();
    for segment in segments {
        let address = Address::new(Some("AD"), segment.paddr);
        let memory = probe
            .read_memory(&address, segment.data.len())
            .map_err(|error| {
                DebugError::from(error).context(format!(
                    "cannot read AD:0x{:X} ({} bytes)",
                    segment.paddr,
                    segment.data.len()
                ))
            })?;
        let mut differing = 0;
        let mut first_difference = None;
        for (offset, (expected, actual)) in segment.data.iter().zip(&memory).enumerate() {
            if expected != actual {
                differing += 1;
                first_difference.get_or_insert(segment.paddr + offset as u64);
            }
        }
        results.push(SegmentResult {
            paddr: segment.paddr,
            vaddr: segment.vaddr,
            size: segment.data.len(),
            differing,
            first_difference,
        });
    }
    Ok(results)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::debug::probe::fake::FakeProbe;

    /// A little-endian ELF32 with the given (paddr, vaddr, filesz, memsz, data)
    /// program headers; the data follows the headers.
    pub fn elf32(segments: &[(u32, u32, &str, u32)], extra_type: Option<u32>) -> Vec<u8> {
        let count = segments.len() + usize::from(extra_type.is_some());
        let phoff = 52u32;
        let mut data_offset = phoff + 32 * count as u32;
        let mut header = vec![0u8; 52];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4] = 1; // ELFCLASS32
        header[5] = 1; // ELFDATA2LSB
        header[6] = 1;
        header[0x1C..0x20].copy_from_slice(&phoff.to_le_bytes());
        header[0x2A..0x2C].copy_from_slice(&32u16.to_le_bytes());
        header[0x2C..0x2E].copy_from_slice(&(count as u16).to_le_bytes());
        let mut table = Vec::new();
        let mut payload = Vec::new();
        for (paddr, vaddr, data, memsz) in segments {
            for word in [
                PT_LOAD,
                data_offset,
                *vaddr,
                *paddr,
                data.len() as u32,
                *memsz,
                5,
                4,
            ] {
                table.extend_from_slice(&word.to_le_bytes());
            }
            payload.extend_from_slice(data.as_bytes());
            data_offset += data.len() as u32;
        }
        if let Some(kind) = extra_type {
            for word in [kind, 0, 0, 0, 16, 16, 4, 4] {
                table.extend_from_slice(&word.to_le_bytes());
            }
        }
        [header, table, payload].concat()
    }

    #[test]
    fn extracts_pt_load_at_the_physical_address() {
        let elf = elf32(
            &[
                (0x0800_0000, 0x0800_0000, "CODE", 4),
                (0x0800_0400, 0x2000_0000, "DATA", 4),
                // .bss: nothing in the file.
                (0x2000_0004, 0x2000_0004, "", 0x100),
            ],
            Some(4), // PT_NOTE
        );
        let segments = load_segments(&elf).unwrap();
        assert_eq!(
            segments,
            [
                Segment {
                    paddr: 0x0800_0000,
                    vaddr: 0x0800_0000,
                    data: b"CODE".to_vec()
                },
                Segment {
                    paddr: 0x0800_0400,
                    vaddr: 0x2000_0000,
                    data: b"DATA".to_vec()
                },
            ]
        );
    }

    #[test]
    fn rejects_non_elf_and_truncated_files() {
        assert_eq!(load_segments(b"hello").unwrap_err(), "not an ELF file");
        let mut elf = elf32(&[(0, 0, "CODE", 4)], None);
        elf.truncate(60);
        assert_eq!(load_segments(&elf).unwrap_err(), "truncated ELF file");
    }

    #[test]
    fn segment_with_different_vma_is_compared_at_the_lma() {
        let elf = elf32(
            &[(0x100, 0x100, "CODE", 4), (0x488, 0x2000_0000, "DATA", 4)],
            None,
        );
        let segments = load_segments(&elf).unwrap();
        let mut probe = FakeProbe::default();
        probe.write(0x100, b"CODE");
        probe.write(0x488, b"DATA");
        // Live RAM at the VMA has been changed by the program.
        probe.write(0x2000_0000, b"XXXX");
        let results = compare(&mut probe, &segments).unwrap();
        assert!(results.iter().all(|r| r.differing == 0), "{results:?}");
        assert_eq!(probe.log, ["read AD:0x100 4", "read AD:0x488 4"]);
    }

    #[test]
    fn differences_are_counted() {
        let segments = vec![Segment {
            paddr: 0x200,
            vaddr: 0x200,
            data: b"ABCDEFGH".to_vec(),
        }];
        let mut probe = FakeProbe::default();
        probe.write(0x200, b"ABxDEyGz");
        let results = compare(&mut probe, &segments).unwrap();
        assert_eq!(results[0].differing, 3);
        assert_eq!(results[0].first_difference, Some(0x202));
    }
}
