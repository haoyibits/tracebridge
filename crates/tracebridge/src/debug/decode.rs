//! Pure decoders for `status` and `fault`: the debugger mode, the AArch32
//! CPSR/SPSR, the Hyp vector table slots and the HSR (Arm DDI 0568A.c,
//! section E2.1 "HSR"; the same layout as Armv7-A/R HSR).

/// `SYStem.Mode()` codes (General Function Reference, SYStem.Mode()).
pub fn system_mode_name(code: u64) -> Option<&'static str> {
    Some(match code {
        0 => "down",
        1 => "standby",
        2 => "nodebug",
        4 => "prepare",
        11 => "up",
        12 => "up (standby)",
        13 => "prepare (standby)",
        _ => return None,
    })
}

/// The mode for display: the name, or the raw code when it is unknown.
pub fn system_mode_label(code: u64) -> String {
    system_mode_name(code)
        .map(str::to_string)
        .unwrap_or_else(|| format!("mode {code}"))
}

/// A decoded AArch32 CPSR or SPSR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Psr {
    pub raw: u32,
    /// M[4:0].
    pub mode: u32,
    pub mode_name: Option<&'static str>,
    /// T, bit 5.
    pub thumb: bool,
    /// A, I and F (bits 8, 7, 6): set means masked.
    pub a: bool,
    pub i: bool,
    pub f: bool,
    /// N, Z, C, V (bits 31..28).
    pub nzcv: u32,
}

pub fn decode_psr(raw: u32) -> Psr {
    let mode = raw & 0x1F;
    Psr {
        raw,
        mode,
        mode_name: match mode {
            0x10 => Some("usr"),
            0x11 => Some("fiq"),
            0x12 => Some("irq"),
            0x13 => Some("svc"),
            0x16 => Some("mon"),
            0x17 => Some("abt"),
            0x1A => Some("hyp"),
            0x1B => Some("und"),
            0x1F => Some("sys"),
            _ => None,
        },
        thumb: raw & (1 << 5) != 0,
        a: raw & (1 << 8) != 0,
        i: raw & (1 << 7) != 0,
        f: raw & (1 << 6) != 0,
        nzcv: raw >> 28,
    }
}

impl Psr {
    /// `hyp, T=1 (Thumb), masked: A I F, flags: Z C`.
    pub fn describe(&self) -> String {
        let mode = match self.mode_name {
            Some(name) => name.to_string(),
            None => format!("mode 0x{:02X} (not an AArch32 mode)", self.mode),
        };
        let state = if self.thumb {
            "T=1 (Thumb)"
        } else {
            "T=0 (Arm)"
        };
        let masked: Vec<&str> = [("A", self.a), ("I", self.i), ("F", self.f)]
            .into_iter()
            .filter_map(|(name, set)| set.then_some(name))
            .collect();
        let masked = if masked.is_empty() {
            "none".to_string()
        } else {
            masked.join(" ")
        };
        let flags: Vec<&str> = [("N", 8), ("Z", 4), ("C", 2), ("V", 1)]
            .into_iter()
            .filter_map(|(name, bit)| (self.nzcv & bit != 0).then_some(name))
            .collect();
        let flags = if flags.is_empty() {
            "none".to_string()
        } else {
            flags.join(" ")
        };
        format!("{mode}, {state}, masked: {masked}, flags: {flags}")
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "raw": self.raw,
            "hex": format!("0x{:08X}", self.raw),
            "mode": self.mode,
            "mode_name": self.mode_name,
            "thumb": self.thumb,
            "a_masked": self.a,
            "i_masked": self.i,
            "f_masked": self.f,
            "nzcv": self.nzcv,
        })
    }
}

/// Hyp vector table entries, as offsets from HVBAR.
pub const HYP_VECTORS: [(u64, &str); 8] = [
    (0x00, "reset"),
    (0x04, "undefined instruction"),
    (0x08, "HVC/SVC"),
    (0x0C, "prefetch abort"),
    (0x10, "data abort"),
    (0x14, "hyp trap"),
    (0x18, "IRQ"),
    (0x1C, "FIQ"),
];

/// The slot name when `pc` is exactly one of the entries of a table at `base`.
pub fn vector_slot(base: u64, pc: u64) -> Option<(u64, &'static str)> {
    let offset = pc.checked_sub(base)?;
    HYP_VECTORS
        .iter()
        .find(|(entry, _)| *entry == offset)
        .map(|&(entry, name)| (entry, name))
}

/// What the IL bit means for this syndrome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstructionLength {
    Bits16,
    Bits32,
    /// IL is RES1 and says nothing about the instruction; the reason is given.
    NotValid(&'static str),
}

impl InstructionLength {
    pub fn describe(&self) -> String {
        match self {
            InstructionLength::Bits16 => "16-bit instruction".into(),
            InstructionLength::Bits32 => "32-bit instruction".into(),
            InstructionLength::NotValid(reason) => format!("IL not valid (RES1): {reason}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataAbort {
    pub isv: bool,
    pub wnr: bool,
    pub dfsc: u32,
    pub dfsc_name: &'static str,
    pub ea: bool,
    pub cm: bool,
    pub s1ptw: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefetchAbort {
    pub ifsc: u32,
    pub ifsc_name: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Syndrome {
    DataAbort(DataAbort),
    PrefetchAbort(PrefetchAbort),
    Other,
}

/// A decoded HSR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hsr {
    pub raw: u32,
    pub ec: u32,
    pub ec_name: &'static str,
    pub il_bit: bool,
    pub il: InstructionLength,
    pub iss: u32,
    pub syndrome: Syndrome,
}

/// Which fault address register belongs to the exception class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAddress {
    Hdfar,
    Hifar,
    None,
}

fn ec_name(ec: u32) -> &'static str {
    match ec {
        0x00 => "unknown reason",
        0x01 => "trapped WFI/WFE",
        0x03 => "trapped MCR/MRC cp15",
        0x04 => "trapped MCRR/MRRC cp15",
        0x05 => "trapped MCR/MRC cp14",
        0x06 => "trapped LDC/STC",
        0x07 => "HCPTR-trapped FP/SIMD",
        0x08 => "trapped VMRS",
        0x0C => "trapped MRRC cp14",
        0x0E => "illegal exception return",
        0x11 => "SVC routed to EL2",
        0x12 => "HVC",
        0x20 => "prefetch abort from a lower EL",
        0x21 => "prefetch abort, same EL",
        0x22 => "PC alignment fault",
        0x24 => "data abort from a lower EL",
        0x25 => "data abort, same EL",
        _ => "reserved",
    }
}

fn dfsc_name(dfsc: u32) -> &'static str {
    match dfsc {
        0b000100 => "translation fault (address matches no MPU region)",
        0b001100 => "permission fault",
        0b010000 => "synchronous external abort",
        0b011000 => "synchronous parity/ECC error",
        0b010001 => "SError",
        0b011001 => "SError, parity/ECC",
        0b100001 => "alignment fault",
        0b100010 => "debug exception",
        0b110100 => "IMPLEMENTATION DEFINED: cache lockdown",
        0b110101 => "IMPLEMENTATION DEFINED: unsupported exclusive",
        _ => "reserved",
    }
}

fn ifsc_name(ifsc: u32) -> &'static str {
    match ifsc {
        0b000100 => "translation fault",
        0b001100 => "permission fault",
        0b010000 => "synchronous external abort",
        0b011000 => "synchronous parity/ECC error",
        0b100010 => "debug exception",
        _ => "reserved",
    }
}

pub fn decode_hsr(raw: u32) -> Hsr {
    let ec = raw >> 26;
    let il_bit = raw & (1 << 25) != 0;
    let iss = raw & 0x01FF_FFFF;
    let syndrome = match ec {
        0x24 | 0x25 => {
            let dfsc = iss & 0x3F;
            Syndrome::DataAbort(DataAbort {
                isv: iss & (1 << 24) != 0,
                wnr: iss & (1 << 6) != 0,
                dfsc,
                dfsc_name: dfsc_name(dfsc),
                ea: iss & (1 << 9) != 0,
                cm: iss & (1 << 8) != 0,
                s1ptw: iss & (1 << 7) != 0,
            })
        }
        0x20 | 0x21 => {
            let ifsc = iss & 0x3F;
            Syndrome::PrefetchAbort(PrefetchAbort {
                ifsc,
                ifsc_name: ifsc_name(ifsc),
            })
        }
        _ => Syndrome::Other,
    };
    // IL is RES1, not an instruction length, for these syndromes.
    let not_valid = match (&syndrome, ec) {
        (Syndrome::PrefetchAbort(_), _) => Some("prefetch abort"),
        (Syndrome::DataAbort(abort), _) if !abort.isv => Some("data abort with ISV=0"),
        (_, 0x00) => Some("EC 0x00"),
        (_, 0x0E) => Some("illegal exception return"),
        _ => None,
    };
    let il = match not_valid {
        Some(reason) => InstructionLength::NotValid(reason),
        None if il_bit => InstructionLength::Bits32,
        None => InstructionLength::Bits16,
    };
    Hsr {
        raw,
        ec,
        ec_name: ec_name(ec),
        il_bit,
        il,
        iss,
        syndrome,
    }
}

impl Hsr {
    pub fn fault_address(&self) -> FaultAddress {
        match self.syndrome {
            Syndrome::DataAbort(_) => FaultAddress::Hdfar,
            Syndrome::PrefetchAbort(_) => FaultAddress::Hifar,
            Syndrome::Other => FaultAddress::None,
        }
    }

    /// Indented lines for the human report.
    pub fn describe(&self) -> Vec<String> {
        let mut lines = vec![
            format!("EC    0x{:02X}  {}", self.ec, self.ec_name),
            format!("IL    {}  {}", u8::from(self.il_bit), self.il.describe()),
            format!("ISS   0x{:07X}", self.iss),
        ];
        match &self.syndrome {
            Syndrome::DataAbort(abort) => {
                lines.push(format!(
                    "ISV   {}{}",
                    u8::from(abort.isv),
                    if abort.isv {
                        ""
                    } else {
                        "  (no instruction syndrome: SAS/SSE/SRT not valid)"
                    }
                ));
                lines.push(format!(
                    "WnR   {}  ({})",
                    u8::from(abort.wnr),
                    if abort.wnr { "write" } else { "read" }
                ));
                lines.push(format!("DFSC  0b{:06b}  {}", abort.dfsc, abort.dfsc_name));
                lines.push(format!(
                    "EA {}  CM {}  S1PTW {}",
                    u8::from(abort.ea),
                    u8::from(abort.cm),
                    u8::from(abort.s1ptw)
                ));
            }
            Syndrome::PrefetchAbort(abort) => {
                lines.push(format!("IFSC  0b{:06b}  {}", abort.ifsc, abort.ifsc_name));
            }
            Syndrome::Other => {}
        }
        lines
    }

    pub fn to_json(&self) -> serde_json::Value {
        let il = match &self.il {
            InstructionLength::Bits16 => serde_json::json!({"valid": true, "bits": 16}),
            InstructionLength::Bits32 => serde_json::json!({"valid": true, "bits": 32}),
            InstructionLength::NotValid(reason) => {
                serde_json::json!({"valid": false, "reason": reason})
            }
        };
        let syndrome = match &self.syndrome {
            Syndrome::DataAbort(abort) => serde_json::json!({
                "kind": "data_abort",
                "isv": abort.isv,
                "wnr": abort.wnr,
                "dfsc": abort.dfsc,
                "dfsc_name": abort.dfsc_name,
                "ea": abort.ea,
                "cm": abort.cm,
                "s1ptw": abort.s1ptw,
            }),
            Syndrome::PrefetchAbort(abort) => serde_json::json!({
                "kind": "prefetch_abort",
                "ifsc": abort.ifsc,
                "ifsc_name": abort.ifsc_name,
            }),
            Syndrome::Other => serde_json::Value::Null,
        };
        serde_json::json!({
            "raw": self.raw,
            "hex": format!("0x{:08X}", self.raw),
            "ec": self.ec,
            "ec_name": self.ec_name,
            "il_bit": self.il_bit,
            "il": il,
            "iss": self.iss,
            "syndrome": syndrome,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_abort_same_el_permission_fault_without_isv() {
        let hsr = decode_hsr(0x9600_004C);
        assert_eq!(hsr.ec, 0x25);
        assert_eq!(hsr.ec_name, "data abort, same EL");
        assert!(hsr.il_bit);
        assert_eq!(hsr.il, InstructionLength::NotValid("data abort with ISV=0"));
        assert!(hsr.il.describe().starts_with("IL not valid (RES1)"));
        let Syndrome::DataAbort(abort) = &hsr.syndrome else {
            panic!("{hsr:?}")
        };
        assert!(!abort.isv);
        assert!(abort.wnr);
        assert_eq!(abort.dfsc, 0b001100);
        assert_eq!(abort.dfsc_name, "permission fault");
        assert_eq!(hsr.fault_address(), FaultAddress::Hdfar);
    }

    #[test]
    fn data_abort_translation_fault() {
        let hsr = decode_hsr(0x9600_0044);
        assert_eq!(hsr.ec, 0x25);
        let Syndrome::DataAbort(abort) = &hsr.syndrome else {
            panic!("{hsr:?}")
        };
        assert_eq!(
            abort.dfsc_name,
            "translation fault (address matches no MPU region)"
        );
        assert!(matches!(hsr.il, InstructionLength::NotValid(_)));
    }

    #[test]
    fn data_abort_with_isv_has_a_valid_il() {
        // EC 0x24, IL=0, ISV=1, WnR=0, DFSC alignment.
        let hsr = decode_hsr((0x24 << 26) | (1 << 24) | 0b100001);
        assert_eq!(hsr.il, InstructionLength::Bits16);
        assert_eq!(hsr.ec_name, "data abort from a lower EL");
        let hsr = decode_hsr((0x24 << 26) | (1 << 25) | (1 << 24) | (1 << 9) | 0b010000);
        assert_eq!(hsr.il, InstructionLength::Bits32);
        let Syndrome::DataAbort(abort) = &hsr.syndrome else {
            panic!()
        };
        assert!(abort.ea && !abort.cm && !abort.s1ptw && !abort.wnr);
    }

    #[test]
    fn prefetch_abort_il_is_not_valid() {
        let hsr = decode_hsr(0x8600_000C);
        assert_eq!(hsr.ec, 0x21);
        assert_eq!(hsr.ec_name, "prefetch abort, same EL");
        assert_eq!(hsr.il, InstructionLength::NotValid("prefetch abort"));
        assert_eq!(
            hsr.syndrome,
            Syndrome::PrefetchAbort(PrefetchAbort {
                ifsc: 0b001100,
                ifsc_name: "permission fault"
            })
        );
        assert_eq!(hsr.fault_address(), FaultAddress::Hifar);
    }

    #[test]
    fn unknown_reason_il_is_not_valid() {
        let hsr = decode_hsr(0x0200_0000);
        assert_eq!(hsr.ec, 0);
        assert_eq!(hsr.ec_name, "unknown reason");
        assert_eq!(hsr.il, InstructionLength::NotValid("EC 0x00"));
        assert_eq!(hsr.fault_address(), FaultAddress::None);
        let hsr = decode_hsr((0x0E << 26) | (1 << 25));
        assert_eq!(
            hsr.il,
            InstructionLength::NotValid("illegal exception return")
        );
    }

    #[test]
    fn other_classes_keep_il() {
        let hsr = decode_hsr((0x12 << 26) | (1 << 25) | 0x42);
        assert_eq!(hsr.ec_name, "HVC");
        assert_eq!(hsr.il, InstructionLength::Bits32);
        assert_eq!(decode_hsr(0x3F << 26).ec_name, "reserved");
    }

    #[test]
    fn cpsr_hyp_thumb_all_masked() {
        let psr = decode_psr(0x6000_01FA);
        assert_eq!(psr.mode, 0x1A);
        assert_eq!(psr.mode_name, Some("hyp"));
        assert!(psr.thumb && psr.a && psr.i && psr.f);
        assert_eq!(psr.nzcv, 0b0110);
        assert_eq!(
            psr.describe(),
            "hyp, T=1 (Thumb), masked: A I F, flags: Z C"
        );
    }

    #[test]
    fn cpsr_same_mode_bits_without_flags() {
        let psr = decode_psr(0x0000_01FA);
        assert_eq!(psr.mode_name, Some("hyp"));
        assert!(psr.thumb && psr.a && psr.i && psr.f);
        assert_eq!(psr.nzcv, 0);
        assert_eq!(
            psr.describe(),
            "hyp, T=1 (Thumb), masked: A I F, flags: none"
        );
        let psr = decode_psr(0x0000_0013);
        assert_eq!(psr.describe(), "svc, T=0 (Arm), masked: none, flags: none");
        assert!(
            decode_psr(0x0000_0005)
                .describe()
                .contains("not an AArch32 mode")
        );
    }

    #[test]
    fn vector_slots() {
        assert_eq!(
            vector_slot(0x0800_0020, 0x0800_0030),
            Some((0x10, "data abort"))
        );
        assert_eq!(vector_slot(0x0800_0020, 0x0800_0020), Some((0, "reset")));
        assert_eq!(vector_slot(0x0800_0020, 0x0800_0022), None);
        assert_eq!(vector_slot(0x0800_0020, 0x0800_0040), None);
        assert_eq!(vector_slot(0x0800_0020, 0x0800_0000), None);
    }

    #[test]
    fn system_modes() {
        assert_eq!(system_mode_label(0), "down");
        assert_eq!(system_mode_label(11), "up");
        assert_eq!(system_mode_label(12), "up (standby)");
        assert_eq!(system_mode_label(7), "mode 7");
    }
}
