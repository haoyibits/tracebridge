//! Interactive SEGGER RTT terminal through a running PowerView (rtt.py).
//!
//! TRACE32 owns the debug probe. This process connects to TRACE32's Remote API,
//! drains RTT up-channel 0 to stdout and forwards stdin to down-channel 0, so
//! it carries both printf output and an interactive console. All memory access
//! uses the `E:` (run-time) access class.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Parser;
use t32rcl::{Address, Debugger};

use crate::config::Config;
use crate::errors::Result;
use crate::{bail, bridge_error};

pub const ID_STRING: &[u8] = b"SEGGER RTT";

// 32-bit SEGGER RTT control-block offsets.
pub const UP_DESCRIPTOR: u64 = 0x18;
/// UP_DESCRIPTOR + one 24-byte up descriptor.
pub const DOWN_DESCRIPTOR: u64 = 0x30;
pub const DESC_BUFFER: u64 = 0x04;
#[allow(dead_code)] // part of the layout; read together with DESC_BUFFER
pub const DESC_SIZE: u64 = 0x08;
pub const DESC_WR_OFF: u64 = 0x0C;
pub const DESC_RD_OFF: u64 = 0x10;

/// Target memory as the RTT channel sees it.
pub trait Memory {
    fn read(&mut self, address: u64, length: usize) -> t32rcl::Result<Vec<u8>>;
    fn write(&mut self, address: u64, data: &[u8]) -> t32rcl::Result<()>;
    fn write_u32(&mut self, address: u64, value: u32) -> t32rcl::Result<()>;
}

/// `RttChannel.address`: run-time memory access.
fn run_time(address: u64) -> Address {
    Address::new(Some("E"), address)
}

impl Memory for Debugger {
    fn read(&mut self, address: u64, length: usize) -> t32rcl::Result<Vec<u8>> {
        self.memory_read(&run_time(address), length)
    }
    fn write(&mut self, address: u64, data: &[u8]) -> t32rcl::Result<()> {
        self.memory_write(&run_time(address), data)
    }
    fn write_u32(&mut self, address: u64, value: u32) -> t32rcl::Result<()> {
        self.memory_write_u32(&run_time(address), value)
    }
}

/// One ring buffer descriptor: `pBuffer`, `SizeOfBuffer`, `WrOff`, `RdOff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Descriptor {
    buffer: u32,
    size: u32,
    write_offset: u32,
    read_offset: u32,
}

impl Descriptor {
    fn read(memory: &mut impl Memory, address: u64) -> t32rcl::Result<Descriptor> {
        let raw = memory.read(address, 16)?;
        if raw.len() < 16 {
            return Err(t32rcl::Error::Protocol("short RTT descriptor read".into()));
        }
        let word = |i: usize| u32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap());
        Ok(Descriptor {
            buffer: word(0),
            size: word(1),
            write_offset: word(2),
            read_offset: word(3),
        })
    }

    fn is_valid(&self) -> bool {
        self.buffer != 0
            && self.size >= 2
            && self.write_offset < self.size
            && self.read_offset < self.size
    }
}

/// Bidirectional RTT channel 0 (`RttChannel`).
#[derive(Debug)]
pub struct RttChannel {
    pub control_block: u64,
    pub initialized: bool,
}

impl RttChannel {
    pub fn new(control_block: u64) -> Self {
        RttChannel {
            control_block,
            initialized: false,
        }
    }

    fn cb(&self, offset: u64) -> u64 {
        self.control_block + offset
    }

    /// Check the "SEGGER RTT" identifier.
    pub fn refresh_state(&mut self, memory: &mut impl Memory) -> t32rcl::Result<bool> {
        let identifier = memory.read(self.cb(0), 16)?;
        self.initialized = identifier.starts_with(ID_STRING);
        Ok(self.initialized)
    }

    /// Drain the target-to-host bytes that are currently available.
    pub fn read_up(&mut self, memory: &mut impl Memory) -> t32rcl::Result<Vec<u8>> {
        if !self.initialized && !self.refresh_state(memory)? {
            return Ok(Vec::new());
        }
        let descriptor = Descriptor::read(memory, self.cb(UP_DESCRIPTOR + DESC_BUFFER))?;
        if !descriptor.is_valid() {
            self.initialized = false;
            return Ok(Vec::new());
        }
        let Descriptor {
            buffer,
            size,
            write_offset,
            mut read_offset,
        } = descriptor;
        if write_offset == read_offset {
            return Ok(Vec::new());
        }
        let mut data = Vec::new();
        if write_offset < read_offset {
            data.extend(memory.read(
                u64::from(buffer) + u64::from(read_offset),
                (size - read_offset) as usize,
            )?);
            read_offset = 0;
        }
        if write_offset > read_offset {
            data.extend(memory.read(
                u64::from(buffer) + u64::from(read_offset),
                (write_offset - read_offset) as usize,
            )?);
        }
        // The host is the sole writer of the up-channel read offset.
        memory.write_u32(self.cb(UP_DESCRIPTOR + DESC_RD_OFF), write_offset)?;
        Ok(data)
    }

    /// Write as much of `data` as fits into host-to-target channel 0; returns
    /// the number of bytes written.
    pub fn write_down(&mut self, memory: &mut impl Memory, data: &[u8]) -> t32rcl::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        if !self.initialized && !self.refresh_state(memory)? {
            return Ok(0);
        }
        let descriptor = Descriptor::read(memory, self.cb(DOWN_DESCRIPTOR + DESC_BUFFER))?;
        if !descriptor.is_valid() {
            self.initialized = false;
            return Ok(0);
        }
        let Descriptor {
            buffer,
            size,
            write_offset,
            read_offset,
        } = descriptor;
        let (size, write_offset, read_offset) =
            (size as usize, write_offset as usize, read_offset as usize);
        let free = (read_offset + size - write_offset - 1) % size;
        let count = data.len().min(free);
        if count == 0 {
            return Ok(0);
        }
        let first = count.min(size - write_offset);
        memory.write(u64::from(buffer) + write_offset as u64, &data[..first])?;
        if count > first {
            memory.write(u64::from(buffer), &data[first..count])?;
        }
        // Publish WrOff last so the target never sees incomplete input.
        memory.write_u32(
            self.cb(DOWN_DESCRIPTOR + DESC_WR_OFF),
            ((write_offset + count) % size) as u32,
        )?;
        Ok(count)
    }
}

fn parse_address(value: &str) -> std::result::Result<u64, String> {
    crate::pycompat::parse_int_auto(value)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| format!("invalid address: {value}"))
}

/// Options of `tracebridge rtt` (`add_arguments`); defaults come from trace32.toml.
#[derive(Debug, Parser)]
#[command(
    name = "tracebridge rtt",
    about = "Interactive SEGGER RTT terminal through PowerView"
)]
pub struct RttArgs {
    /// TRACE32 symbol program name (default: project.program)
    #[arg(long)]
    pub program: Option<String>,
    /// RTT control-block symbol (default: rtt.symbol, _SEGGER_RTT)
    #[arg(long)]
    pub symbol: Option<String>,
    /// Control-block address; bypasses the symbol lookup, e.g. 0x20000000
    #[arg(long = "cb", value_parser = parse_address)]
    pub control_block: Option<u64>,
    /// Remote API host
    #[arg(long, default_value = "localhost")]
    pub node: String,
    /// Remote API port (default: trace32.rcl_port)
    #[arg(long)]
    pub port: Option<u16>,
    /// Poll period in seconds (default: rtt.poll_interval, 0.02)
    #[arg(long)]
    pub poll: Option<f64>,
    /// Set the up-channel RdOff to zero before polling
    #[arg(long)]
    pub replay: bool,
    /// Do not forward terminal input to the RTT down-channel
    #[arg(long)]
    pub output_only: bool,
}

/// Parse the arguments after `rtt`; usage errors exit like any clap parser.
pub fn parse_args(forwarded: &[String]) -> RttArgs {
    let argv = std::iter::once("tracebridge rtt".to_string()).chain(forwarded.iter().cloned());
    RttArgs::parse_from(argv)
}

/// Character-at-a-time input without echo; Ctrl-C still raises SIGINT.
/// The previous settings are restored on drop, including during a panic.
struct TerminalGuard {
    #[cfg(unix)]
    original: Option<nix::sys::termios::Termios>,
}

impl TerminalGuard {
    fn enable(enabled: bool) -> TerminalGuard {
        #[cfg(unix)]
        {
            use nix::sys::termios::{
                LocalFlags, SetArg, SpecialCharacterIndices, tcgetattr, tcsetattr,
            };
            let stdin = std::io::stdin();
            if !enabled || !nix::unistd::isatty(&stdin).unwrap_or(false) {
                return TerminalGuard { original: None };
            }
            let Ok(original) = tcgetattr(&stdin) else {
                return TerminalGuard { original: None };
            };
            let mut modified = original.clone();
            modified
                .local_flags
                .remove(LocalFlags::ICANON | LocalFlags::ECHO);
            modified.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
            modified.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
            if tcsetattr(&stdin, SetArg::TCSADRAIN, &modified).is_err() {
                return TerminalGuard { original: None };
            }
            TerminalGuard {
                original: Some(original),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = enabled;
            TerminalGuard {}
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(original) = &self.original {
            let _ = nix::sys::termios::tcsetattr(
                std::io::stdin(),
                nix::sys::termios::SetArg::TCSADRAIN,
                original,
            );
        }
    }
}

/// `read_terminal_input`: whatever is available right now, without blocking.
/// Embedded consoles usually treat BS as erase; most terminals send DEL.
fn read_terminal_input() -> Vec<u8> {
    #[cfg(unix)]
    {
        use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
        use std::os::fd::AsFd;
        let stdin = std::io::stdin();
        let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
        if !matches!(poll(&mut fds, PollTimeout::ZERO), Ok(count) if count > 0) {
            return Vec::new();
        }
        let mut buffer = [0u8; 256];
        match nix::unistd::read(stdin.as_fd(), &mut buffer) {
            Ok(count) => buffer[..count]
                .iter()
                .map(|&b| if b == 0x7f { 0x08 } else { b })
                .collect(),
            Err(_) => Vec::new(),
        }
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

/// `run`.
pub fn run(config: &Config, args: RttArgs) -> Result<()> {
    let port = args.port.unwrap_or(config.rcl_port);
    let program = args.program.unwrap_or_else(|| config.program.clone());
    let symbol = args.symbol.unwrap_or_else(|| config.rtt_symbol.clone());
    let control_block = args.control_block.or(config.rtt_control_block_address);
    let poll = args.poll.unwrap_or(config.rtt_poll_interval);
    if !(poll > 0.0 && poll.is_finite()) {
        bail!("--poll must be positive");
    }
    let poll = Duration::from_secs_f64(poll);

    let mut debugger = Debugger::connect(&args.node, port, Duration::from_secs(5))
        .and_then(|mut debugger| {
            debugger.print(&format!(
                "RTT terminal connected (pid {})",
                std::process::id()
            ))?;
            Ok(debugger)
        })
        .map_err(|error| {
            bridge_error!(
                "cannot connect to TRACE32 at {}:{port} ({error})\n\
                 Check the RCL=NETTCP / PORT= section of your config.t32.",
                args.node
            )
        })?;

    let control_block = match control_block {
        Some(address) => address,
        None if program.is_empty() => bail!("project.program is empty and no --cb was given"),
        None => debugger
            .symbol_address(&format!("\\\\{program}\\Global\\{symbol}"))
            .map_err(|error| {
                bridge_error!(
                    "cannot resolve {symbol} in '{program}' ({error})\n\
                     Run 'tracebridge load' first, or pass --cb 0x<address>."
                )
            })?,
    };

    let mut channel = RttChannel::new(control_block);
    eprintln!("TRACE32 RTT: {symbol} @ 0x{control_block:08X}; Ctrl-C to stop");
    if !channel.refresh_state(&mut debugger).unwrap_or(false) {
        eprintln!(
            "[rtt] waiting for the target to initialize SEGGER RTT; \
             if debugging is paused before RTT initialization, press Continue"
        );
    }
    if args.replay {
        debugger
            .write_u32(channel.cb(UP_DESCRIPTOR + DESC_RD_OFF), 0)
            .map_err(|error| bridge_error!("cannot reset the RTT read offset: {error}"))?;
    }

    let interrupted = std::sync::Arc::new(AtomicBool::new(false));
    crate::signals::flag_on_interrupt(&interrupted);
    let guard = TerminalGuard::enable(!args.output_only);
    let mut pending_input: Vec<u8> = Vec::new();
    let mut consecutive_errors = 0u64;
    let mut stdout = std::io::stdout();
    while !interrupted.load(Ordering::SeqCst) {
        let step = (|| -> t32rcl::Result<()> {
            let output = channel.read_up(&mut debugger)?;
            if !output.is_empty() {
                // Passed through byte for byte: escape sequences are the target's business.
                let _ = stdout.write_all(&output);
                let _ = stdout.flush();
            }
            if !args.output_only {
                pending_input.extend(read_terminal_input());
                if !pending_input.is_empty() {
                    let sent = channel.write_down(&mut debugger, &pending_input)?;
                    pending_input.drain(..sent);
                }
            }
            Ok(())
        })();
        match step {
            Ok(()) => {
                consecutive_errors = 0;
                std::thread::sleep(poll);
            }
            Err(error) => {
                consecutive_errors += 1;
                channel.initialized = false;
                if matches!(consecutive_errors, 5 | 50) || consecutive_errors % 300 == 0 {
                    eprintln!(
                        "\n[rtt] run-time memory access failed x{consecutive_errors}: {error}"
                    );
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    drop(guard);
    eprintln!("\nTRACE32 RTT terminal stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const CB: u64 = 0x2000_0000;
    const UP_BUFFER: u32 = 0x2000_1000;
    const DOWN_BUFFER: u32 = 0x2000_2000;

    /// Sparse little-endian memory that logs every access.
    #[derive(Default)]
    struct FakeMemory {
        bytes: HashMap<u64, u8>,
        log: Vec<String>,
        fail: bool,
    }

    impl FakeMemory {
        fn put(&mut self, address: u64, data: &[u8]) {
            for (i, byte) in data.iter().enumerate() {
                self.bytes.insert(address + i as u64, *byte);
            }
        }

        fn get(&self, address: u64, length: usize) -> Vec<u8> {
            (0..length)
                .map(|i| *self.bytes.get(&(address + i as u64)).unwrap_or(&0))
                .collect()
        }

        fn word(&self, address: u64) -> u32 {
            u32::from_le_bytes(self.get(address, 4).try_into().unwrap())
        }

        fn descriptor(&mut self, at: u64, buffer: u32, size: u32, write: u32, read: u32) {
            for (i, value) in [buffer, size, write, read].into_iter().enumerate() {
                self.put(at + DESC_BUFFER + 4 * i as u64, &value.to_le_bytes());
            }
        }

        /// An initialized control block with a 16-byte up and an 8-byte down buffer.
        fn initialized() -> FakeMemory {
            let mut memory = FakeMemory::default();
            memory.put(CB, b"SEGGER RTT\0\0\0\0\0\0");
            memory.descriptor(CB + UP_DESCRIPTOR, UP_BUFFER, 16, 0, 0);
            memory.descriptor(CB + DOWN_DESCRIPTOR, DOWN_BUFFER, 8, 0, 0);
            memory
        }
    }

    impl Memory for FakeMemory {
        fn read(&mut self, address: u64, length: usize) -> t32rcl::Result<Vec<u8>> {
            if self.fail {
                return Err(t32rcl::Error::Timeout);
            }
            self.log.push(format!("read {address:#x} {length}"));
            Ok(self.get(address, length))
        }
        fn write(&mut self, address: u64, data: &[u8]) -> t32rcl::Result<()> {
            self.log.push(format!("write {address:#x} {data:?}"));
            self.put(address, data);
            Ok(())
        }
        fn write_u32(&mut self, address: u64, value: u32) -> t32rcl::Result<()> {
            self.log.push(format!("write_u32 {address:#x} {value}"));
            self.put(address, &value.to_le_bytes());
            Ok(())
        }
    }

    const UP_RD: u64 = CB + UP_DESCRIPTOR + DESC_RD_OFF;
    const UP_WR: u64 = CB + UP_DESCRIPTOR + DESC_WR_OFF;
    const DOWN_WR: u64 = CB + DOWN_DESCRIPTOR + DESC_WR_OFF;
    const DOWN_RD: u64 = CB + DOWN_DESCRIPTOR + DESC_RD_OFF;

    #[test]
    fn offsets_match_the_32_bit_layout() {
        assert_eq!(DOWN_DESCRIPTOR, UP_DESCRIPTOR + 24);
        assert_eq!(DESC_SIZE, DESC_BUFFER + 4);
    }

    #[test]
    fn uninitialized_control_block_waits() {
        let mut memory = FakeMemory::default();
        let mut channel = RttChannel::new(CB);
        assert!(channel.read_up(&mut memory).unwrap().is_empty());
        assert!(!channel.initialized);
        assert_eq!(channel.write_down(&mut memory, b"x").unwrap(), 0);
        assert!(memory.log.iter().all(|entry| entry.starts_with("read")));
        // Initialization is picked up on the next poll.
        memory = FakeMemory::initialized();
        memory.put(u64::from(UP_BUFFER), b"hi");
        memory.put(UP_WR, &2u32.to_le_bytes());
        assert_eq!(channel.read_up(&mut memory).unwrap(), b"hi");
        assert!(channel.initialized);
    }

    #[test]
    fn empty_up_buffer_writes_nothing() {
        let mut memory = FakeMemory::initialized();
        let mut channel = RttChannel::new(CB);
        assert!(channel.read_up(&mut memory).unwrap().is_empty());
        assert!(!memory.log.iter().any(|entry| entry.starts_with("write")));
    }

    #[test]
    fn contiguous_up_read_advances_rd_off() {
        let mut memory = FakeMemory::initialized();
        memory.put(u64::from(UP_BUFFER) + 3, b"hello");
        memory.put(UP_RD, &3u32.to_le_bytes());
        memory.put(UP_WR, &8u32.to_le_bytes());
        let mut channel = RttChannel::new(CB);
        assert_eq!(channel.read_up(&mut memory).unwrap(), b"hello");
        assert_eq!(memory.word(UP_RD), 8);
    }

    #[test]
    fn wrapped_up_read_uses_two_copies() {
        let mut memory = FakeMemory::initialized();
        memory.put(u64::from(UP_BUFFER) + 13, b"abc");
        memory.put(u64::from(UP_BUFFER), b"de");
        memory.put(UP_RD, &13u32.to_le_bytes());
        memory.put(UP_WR, &2u32.to_le_bytes());
        let mut channel = RttChannel::new(CB);
        assert_eq!(channel.read_up(&mut memory).unwrap(), b"abcde");
        assert_eq!(memory.word(UP_RD), 2);
        let reads: Vec<&String> = memory
            .log
            .iter()
            .filter(|entry| {
                entry.starts_with(&format!("read {UP_BUFFER:#x}"))
                    || entry.starts_with(&format!("read {:#x}", u64::from(UP_BUFFER) + 13))
            })
            .collect();
        assert_eq!(
            reads,
            [
                &format!("read {:#x} 3", u64::from(UP_BUFFER) + 13),
                &format!("read {UP_BUFFER:#x} 2")
            ]
        );
    }

    #[test]
    fn wrapped_up_read_ending_at_zero_needs_one_copy() {
        let mut memory = FakeMemory::initialized();
        memory.put(u64::from(UP_BUFFER) + 14, b"yz");
        memory.put(UP_RD, &14u32.to_le_bytes());
        memory.put(UP_WR, &0u32.to_le_bytes());
        let mut channel = RttChannel::new(CB);
        assert_eq!(channel.read_up(&mut memory).unwrap(), b"yz");
        assert_eq!(memory.word(UP_RD), 0);
    }

    #[test]
    fn invalid_descriptor_resets_initialization() {
        for (buffer, size, write, read) in [
            (0, 16, 0, 1),
            (UP_BUFFER, 1, 0, 0),
            (UP_BUFFER, 16, 16, 0),
            (UP_BUFFER, 16, 0, 16),
        ] {
            let mut memory = FakeMemory::initialized();
            memory.descriptor(CB + UP_DESCRIPTOR, buffer, size, write, read);
            let mut channel = RttChannel::new(CB);
            assert!(channel.read_up(&mut memory).unwrap().is_empty());
            assert!(!channel.initialized);
        }
    }

    #[test]
    fn down_write_publishes_wr_off_last() {
        let mut memory = FakeMemory::initialized();
        let mut channel = RttChannel::new(CB);
        assert_eq!(channel.write_down(&mut memory, b"ls\n").unwrap(), 3);
        assert_eq!(memory.get(u64::from(DOWN_BUFFER), 3), b"ls\n");
        assert_eq!(memory.word(DOWN_WR), 3);
        assert!(
            memory
                .log
                .last()
                .unwrap()
                .starts_with(&format!("write_u32 {DOWN_WR:#x} 3"))
        );
    }

    #[test]
    fn wrapped_down_write_uses_two_copies() {
        let mut memory = FakeMemory::initialized();
        memory.put(DOWN_WR, &6u32.to_le_bytes());
        memory.put(DOWN_RD, &5u32.to_le_bytes());
        let mut channel = RttChannel::new(CB);
        // size 8, WrOff 6, RdOff 5: 6 bytes free.
        assert_eq!(channel.write_down(&mut memory, b"abcdefgh").unwrap(), 6);
        assert_eq!(memory.get(u64::from(DOWN_BUFFER) + 6, 2), b"ab");
        assert_eq!(memory.get(u64::from(DOWN_BUFFER), 4), b"cdef");
        assert_eq!(memory.word(DOWN_WR), 4);
        let writes: Vec<&String> = memory
            .log
            .iter()
            .filter(|e| e.starts_with("write"))
            .collect();
        assert_eq!(writes.len(), 3);
        assert!(writes[2].starts_with("write_u32"));
    }

    #[test]
    fn full_down_buffer_accepts_nothing() {
        let mut memory = FakeMemory::initialized();
        memory.put(DOWN_WR, &4u32.to_le_bytes());
        memory.put(DOWN_RD, &5u32.to_le_bytes());
        let mut channel = RttChannel::new(CB);
        assert_eq!(channel.write_down(&mut memory, b"x").unwrap(), 0);
        assert!(!memory.log.iter().any(|entry| entry.starts_with("write")));
    }

    #[test]
    fn empty_input_touches_no_memory() {
        let mut memory = FakeMemory::initialized();
        let mut channel = RttChannel::new(CB);
        assert_eq!(channel.write_down(&mut memory, b"").unwrap(), 0);
        assert!(memory.log.is_empty());
    }

    #[test]
    fn memory_errors_propagate() {
        let mut memory = FakeMemory::initialized();
        memory.fail = true;
        let mut channel = RttChannel::new(CB);
        assert!(channel.read_up(&mut memory).is_err());
    }

    #[test]
    fn arguments_default_to_the_configuration() {
        let args = parse_args(&["--cb".into(), "0x20000400".into(), "--replay".into()]);
        assert_eq!(args.control_block, Some(0x2000_0400));
        assert!(args.replay);
        assert_eq!(args.node, "localhost");
        assert_eq!(args.port, None);
        let error = RttArgs::try_parse_from(["tracebridge rtt", "--cb", "zz"]).unwrap_err();
        assert!(error.to_string().contains("invalid address: zz"));
        assert!(RttArgs::try_parse_from(["tracebridge rtt", "--protocol", "UDP"]).is_err());
    }
}
