# tracebridge

Drive Lauterbach TRACE32 PowerView from the command line, and debug with it
from VS Code or RustRover. One static binary, no Python, nothing copied into
your project except a `trace32.toml`.

*中文：[README.zh-CN.md](README.zh-CN.md)*

| Command | What it does |
|---|---|
| `tracebridge init` | Create a commented `trace32.toml` in the current directory |
| `tracebridge config` | Show the resolved paths and ports, flag anything missing |
| `tracebridge open` | Start PowerView (or reuse a running one) |
| `tracebridge flash` | Program the ELF with the project's flash script, load symbols, run |
| `tracebridge load` | Load symbols without programming, run |
| `tracebridge rtt` | Bidirectional SEGGER RTT terminal (Ctrl-C to quit) |
| `tracebridge vscode` | Add the `TRACE32: Attach` debug configuration to `.vscode/` |
| `tracebridge rustrover` | Add the `TRACE32: Attach` run configuration to `.run/` |
| `tracebridge chips <name>` | Show which flash script `flash` would use for a chip |
| `tracebridge adapter` | DAP proxy in front of `t32debugadapter`; the IDE starts it |

Supported hosts: macOS (Apple silicon, Intel) and Linux (x86_64, aarch64).

## Install

With Homebrew (macOS or Linux):

```sh
brew install haoyibits/tap/tracebridge
```

Or with the install script:

```sh
curl -fsSL https://github.com/haoyibits/tracebridge/releases/latest/download/install.sh | sh
```

This installs `tracebridge` into `~/.local/bin` (set `TRACEBRIDGE_INSTALL_DIR`
to change it, `TRACEBRIDGE_VERSION=v0.1.0` to pin a release). Make sure the
directory is on your `PATH`.

Manual install: download `tracebridge-<target>.tar.gz` from the releases page,
check it against the `.sha256` file, and copy `tracebridge` somewhere on your
`PATH`.

**macOS and browser downloads:** a binary downloaded with a web browser is
quarantined and Gatekeeper refuses to run it ("cannot be opened because the
developer cannot be verified"). Remove the attribute once:

```sh
xattr -d com.apple.quarantine /path/to/tracebridge
```

`install.sh` (curl) does not set the attribute and removes it if present.

From source: `cargo install --path crates/tracebridge`.

## Requirements

- TRACE32 PowerView with `t32debugadapter` (the default layout under `~/t32`
  is detected automatically).
- The Remote API over TCP. Either enable it in `config.t32`:

  ```text
  RCL=NETTCP
  PORT=20000
  ```

  or leave it out: when `config.t32` has no `RCL=` section, tracebridge starts
  PowerView with `--t32-api-rcl=TCP:<rcl_port>`.

## Quick start

```sh
cd ~/work/my_app            # the project root
tracebridge init            # writes trace32.toml
$EDITOR trace32.toml        # program, elf, [target], [flash]
tracebridge config          # every line should say "ok"
tracebridge flash           # start PowerView, program, load symbols, run
tracebridge vscode          # or: tracebridge rustrover
```

Run the commands anywhere inside the project: like git, tracebridge finds the
nearest `trace32.toml` in the current directory or a parent. `--config <path>`
selects a file explicitly.

Day to day:

1. Build the ELF with your own build system.
2. `tracebridge flash` (or `tracebridge load` when only the symbols changed).
   When tracebridge starts PowerView itself, it also adds **Flash** and
   **Load ELF** buttons to the PowerView toolbar.
3. Debug from the IDE (below), or use PowerView directly.
4. `tracebridge rtt` for the target's RTT console.
5. `tracebridge debug` to check registers, memory, faults and whether the
   board runs the ELF you built, without resetting it.

## Configuration

`trace32.toml` marks the project root. Every relative path in it is relative
to the directory that contains it. `tracebridge init` writes this template:

| Section | Keys |
|---|---|
| `[project]` | `program` (TRACE32 program name), `elf` |
| `[target]` | `cpu`, `cores`, `mem_access`, `jtag_clock`, `dual_port` |
| `[rtos]` | `config`, `menu`, `show_tasks` (empty = no RTOS awareness) |
| `[flash]` | `chip` or `script` (see below), `args` |
| `[trace32]` | `sys`, `host`, `executable`, `binary`, `config`, `debug_adapter`, `rcl_port`, `dap_port`, `dap_backend_port`, `dap_backend_timeout`, `operation_timeout` |
| `[rtt]` | `symbol`, `control_block_address`, `poll_interval` |

Environment variables override the file for one run. The precedence is
environment, then `trace32.toml`, then defaults:

| Variable | Overrides | Empty value |
|---|---|---|
| `T32_SYS`, then `T32SYS` | `trace32.sys` | ignored |
| `T32_HOST`, `T32_EXE`, `T32_BIN`, `T32_CONFIG`, `T32_DEBUG_ADAPTER` | `trace32.*` paths | ignored |
| `PROJECT_ROOT`, `ELF` | project directory, `project.elf` | ignored |
| `RTT_SYMBOL` | `rtt.symbol` | ignored |
| `T32_FLASH_CHIP` | `flash.chip` | ignored |
| `PROGRAM_NAME`, `T32_CPU`, `T32_CORES`, `T32_MEMACCESS`, `T32_JTAG_CLOCK`, `T32_DUALPORT`, `T32_FLASH_SCRIPT` | the same keys | **used** (clears the value) |
| `T32_FLASH_ARGS` | `flash.args`, split like a shell command line | **used** (no arguments) |
| `T32_RCL_PORT`, `T32_DAP_PORT`, `T32_DAP_BACKEND_PORT`, `T32_DAP_BACKEND_TIMEOUT`, `T32_TIMEOUT` | ports and timeouts | ignored |
| `T32_DAP_DEBUG=1` | debug logging of `t32debugadapter` | |

Runtime files (the PowerView log, the toolbar script) are written to
`<project>/.tracebridge/`, which contains its own `.gitignore`.

### Choosing the flash script

`tracebridge flash` needs a flash script that supports Lauterbach's
`PREPAREONLY` convention: set up the target, declare the flash, and return
without programming. When the system is already up, tracebridge first runs
`SYStem.Down`: flash scripts reset and initialize the chip only when the
system is down, and otherwise run the flash algorithm in whatever state the
application left (for example with an MPU that makes the algorithm's RAM
execute-never). After the script, tracebridge runs:

```text
FLASH.ReProgram ALL /Erase
Data.LOAD.Elf <elf>
FLASH.ReProgram OFF
SYStem.Down
SYStem.Up
```

Name the chip instead of a file and tracebridge picks the script:

```toml
[flash]
chip = "STM32H743ZI"    # empty: use target.cpu
script = ""             # a path here wins over chip
```

It looks, in this order, in

1. **your library**, `~/.config/tracebridge/flash/*.cmm`
   (`$XDG_CONFIG_HOME/tracebridge/flash`), for scripts you share between
   projects, such as a vendor-provided or modified script that is not part of
   the TRACE32 release;
2. **the TRACE32 installation**, `<trace32.sys>/demo/*/flash/*.cmm`, about a
   thousand chip scripts.

A script qualifies when its header has a matching `; @Chip:` line (Lauterbach's
format, wildcards allowed, e.g. `; @Chip: STM32H7*`) and it supports
`PREPAREONLY`. An exact pattern beats a wildcard and a longer wildcard beats a
shorter one; the internal-flash script (`stm32f4xx.cmm`) beats memory variants
(`stm32f4xx-qspi.cmm`, `-spi`, `-emmc`, `-optionbyte`, ...). Remaining ties
are reported; set `flash.script` then.

Use the full part number (as for `SYStem.CPU`, e.g. `STM32F407VG`). TRACE32's
scripts cover a whole family and take the derivative as `CPU=<name>`; without
it they fall back to a default derivative. When the chosen script accepts
`CPU=` and `flash.args` does not set it, tracebridge passes `CPU=<chip>`.
Other arguments such as `DUALPORT=` keep the script's default, so
`flash.args` is usually empty. tracebridge sets `target.jtag_clock` itself
after the script has run.

```sh
tracebridge chips STM32H743ZI        # which script, and related ones
tracebridge flash --chip SR6P6       # one-off override
tracebridge flash --script ~~/demo/arm/flash/stm32h7-qspi.cmm
```

To add a script to the library, copy it there and make sure its header has a
`; @Chip: <name>` line. When you modify an official script, keep its arguments
(`PREPAREONLY`, `DUALPORT=`, ...) as they are, so the library copy is used
exactly like the original:

```sh
mkdir -p ~/.config/tracebridge/flash
cp sr6p6.cmm ~/.config/tracebridge/flash/
```

Keep scripts that come with a TRACE32-only license out of public
repositories; the library is the place for them.

## Debugging from an IDE

Both integrations run the same DAP proxy (`tracebridge adapter`) in front of
`t32debugadapter`. PowerView must be running (`tracebridge flash`, `load` or
`open`). The proxy handles **Restart** (reset through the Remote API, then
continue) and answers Locals requests with an empty list, because some
`t32debugadapter` versions crash on FreeRTOS interrupt frames; watch
expressions, registers, the call stack, breakpoints and stepping work
normally.

### VS Code

```sh
tracebridge vscode
```

Merges into `.vscode/launch.json` and `.vscode/tasks.json` (existing entries
are kept; the files are backed up first as `*.bak.<timestamp>`). Open **Run and
Debug**, choose **TRACE32: Attach**, press F5. A hidden task starts the
adapter automatically.

### RustRover (and other JetBrains IDEs)

```sh
tracebridge rustrover
```

1. Install the **LSP4IJ** plugin (Settings → Plugins → Marketplace). It adds
   Debug Adapter Protocol support to JetBrains IDEs.
2. The command writes `.run/TRACE32 Attach.run.xml`; the IDE lists it as the
   **TRACE32: Attach** run configuration.
3. Select it and press **Debug**. LSP4IJ starts the adapter and connects when
   it is ready.

Breakpoints can be set in files matching `*.c *.h *.cpp *.hpp *.cc *.s *.S
*.rs` (the configuration's **Mappings** tab).

The paths in the generated files are absolute (the executable and
`trace32.toml`); rerun the command after moving the project or reinstalling
tracebridge somewhere else.

## RTT

```sh
tracebridge rtt                 # control block from the symbol _SEGGER_RTT
tracebridge rtt --cb 0x20000000 # explicit control block address
tracebridge rtt --output-only   # do not forward keyboard input
tracebridge rtt --help
```

Channel 0 is used in both directions, through TRACE32 run-time memory access
(`dual_port = "ON"`). The terminal switches to character-at-a-time input and
restores its settings on exit.

## Inspecting the target: `tracebridge debug`

`tracebridge debug` reads registers, memory and fault state through the
PowerView that is already running, over the Remote API. It **never resets the
target**: connecting issues no `SYStem.Up`, `SYStem.Mode Go` or reset, and it
does not start PowerView (run `tracebridge open`, `flash` or `load` first).

```sh
tracebridge debug                       # interactive session
tracebridge debug status                # one command, then exit
tracebridge debug reg HSR HSCTLR.C C15:0x4025
tracebridge debug mem my_buffer 8 --json
```

The session has line editing, history (`.tracebridge/debug_history`), Tab
completion of command names, and a prompt that shows the debugger state
(`t32 [up, halted]>`, `t32 [down]>`). A failing command does not end the
session; after a lost connection, `reconnect` connects again.

`--json` prints one JSON document per command. Exit codes: 0 ok, 1 error
(including register not found), 2 usage, 3 a check failed or `verify` found a
difference.

| Command | Kind | What it does |
|---|---|---|
| `status` | R | Debugger mode, run state, power, CPU; when halted, PC with symbol+offset and the decoded CPSR |
| `reg <name\|address>…` | R | Registers by PER-file name (`HSR`), field (`HSCTLR.C`, also prints the BITFLD choice), full PER path, or raw address (`C15:0x4025`, `AD:0x40000000`) |
| `mem <address\|symbol> [count]` | R | `count` 32-bit words (default 1) as a hex dump |
| `fault` | R | AArch32 Hyp fault report: vector slot (also when PC is in a table other than the one HVBAR points at), HSR decoded, HDFAR or HIFAR, ELR_hyp with symbol, SPSR_hyp |
| `eval <expression>` | R | The value of any PRACTICE expression |
| `verify [elf] [--t32]` | R | Does target memory hold the ELF's loadable content? |
| `check <file> [--variant V] [--dry-run]` | R | Data-driven acceptance check (below) |
| `check <file> --halt` | **S** | The same, but stops the core first when checks read CP15 or core registers |
| `watch <file\|name…>` | UI | A PER.Watch window with exactly these registers (PowerView build 176763, 09/2025, or newer) |
| `attach` | S | `SYStem.Mode Attach`: no reset; the core keeps running or stays halted. PowerView then reports mode "up" |
| `down` | S | `SYStem.Down` |
| `break`, `go` | S | Halt or resume the core |
| `cmd <PRACTICE command>` | S | Runs **any** command, including reset and flash |
| `help [command]`, `reconnect`, `quit` | | Session commands |

There is no `up`, `reset` or `flash` in the session; use the top-level
commands for those.

**How values are read.** Everything is evaluated by PowerView as PRACTICE
functions (`Data.Long(...)`, `Register(...)`, `PER.VALUE(...)`,
`sYmbol.BEGIN(...)`), so addresses and access classes mean exactly what they
mean on the PowerView command line. Register names are looked up with
TRACE32's own `PER.ADDRESS()`/`PER.VALUE()` in the CPU's PER file: `HSR` is
searched as `.HSR`, `A.B` as `.A.B` and then as a full path. Names are case
sensitive, and path elements with spaces are quoted:
`'"TMR (Timer Unit)".TMR_0.CTRL'` (quote the whole
argument for the shell). Before the first PER lookup, and again after the
debugger state changes, tracebridge runs `PER.Set.CONDitions` so that
registers inside IF conditions of the PER file can be found.

Coprocessor (CP15) and core registers can only be read from a halted core.
The read-only commands never halt it; they say so instead. Only `verify` uses
the raw memory API, and only for plain memory (`AD:`).

**`verify`** compares every `PT_LOAD` segment with file content at its load
address (LMA, `p_paddr`), not its run address, so initialized data that the
startup code copies to RAM is compared in NVM. It prints `match`, or the first
differing address and the number of differing bytes. `--t32` also runs
TRACE32's own comparison (`Data.LOAD.Elf <elf> /DIFF /PHYSLOAD /NoRegister
/NosYmbol /NoClear`, which changes neither memory, PC nor the loaded symbols)
and reports whether the two agree.

**`check`** runs a TOML file that lives in your project. tracebridge contains
no board facts. [`docs/check-example.toml`](docs/check-example.toml) shows
every form:

```toml
description = "Boot acceptance"

[[check]]
name = "system control"
read = { reg = "SCTRL" }                 # or addr = "AD:0x...", core = "PC", expr = "..."
expect = { eq = 0x00C50078 }             # eq/ne (+ mask), nonzero, range, in_symbol, one_of
variants = ["debug"]                     # optional: only with --variant debug
```

`eq`, `ne` and `one_of` accept `"sym:<name>[+offset]"` in place of a number.
The output is one line per check and a summary. `--dry-run` resolves every
register name (`PER.ADDRESS`) and symbol but reads no register or memory
value, so it also works while the core runs (only `PER.Set.CONDitions`
evaluates the PER file's conditions). When a check reads CP15 or core registers and the core is
running, `check` stops with an error unless `--halt` is given; with `--halt`,
it runs `Break`, says so, and leaves the core halted (`tracebridge debug go`
resumes it).

### Allowing only the read-only commands (Claude Code)

The R commands have no side effects, so an AI assistant can run them without
asking. In the project's `.claude/settings.json`:

```json
{
  "permissions": {
    "allow": [
      "Bash(tracebridge debug status:*)",
      "Bash(tracebridge debug reg:*)",
      "Bash(tracebridge debug mem:*)",
      "Bash(tracebridge debug fault:*)",
      "Bash(tracebridge debug eval:*)",
      "Bash(tracebridge debug verify:*)",
      "Bash(tracebridge debug check checks/boot.toml)",
      "Bash(tracebridge debug check checks/boot.toml --dry-run)",
      "Bash(tracebridge debug check checks/boot.toml --variant release)"
    ]
  }
}
```

`check` is allowed with **exact** command lines rather than a `:*` prefix: a
prefix rule would also allow `check … --halt`, which stops the core. `watch`
(UI) only opens a PowerView window and may be added too. Everything else
(`attach`, `down`, `break`, `go`, `cmd`, `check --halt`, `flash`, `load`) then
still asks. For these rules to match, run the commands inside the project
(no `--config` before `debug`) and put `--json` after the command's arguments.

## Migrating from the Python trace32-bridge

1. Install tracebridge. Python and `lauterbach-trace32-rcl` are no longer
   needed.
2. Move `trace32.toml` from the toolkit directory to the project root and
   delete `root = ".."`: the project root is now the directory that contains
   the file (tracebridge rejects `project.root`).
3. Move the flash script into the library (`~/.config/tracebridge/flash/`,
   with a `; @Chip:` header) and set `flash.chip`, or keep `flash.script` as
   a path relative to the project root or a `~~/` TRACE32 path.
4. Relative `trace32.sys`, `binary`, `config` and `debug_adapter` values are
   now relative to the project root instead of the current directory.
5. Run `tracebridge vscode`. It replaces the old `T32: Flash`, `T32: Load
   ELF`, `T32: RTT Viewer` and `T32: Start Debug Adapter` tasks with a single
   hidden adapter task and updates `TRACE32: Attach`. Use the CLI for flash,
   load and RTT.
6. Delete the copied toolkit directory (`t32.py`, `trace32_bridge/`, `.run/`).

Other changes: messages and the PowerView echo lines say `tracebridge`,
runtime files live in `.tracebridge/`, `rtt --protocol` is gone (TCP only),
and Ctrl-C in `rtt` exits with status 0.

## Troubleshooting

- **`port 20000 is open but is not a usable TRACE32 RCL endpoint`**: another
  program uses the port, or `RCL=` in `config.t32` is not `NETTCP`.
- **PowerView does not become ready**: check `tracebridge config` (it warns
  about `RCL=`/`PORT=` mismatches) and `.tracebridge/powerview.log`.
- **`no PowerView on RCL port …`**: start it with `tracebridge open`, `flash`
  or `load` before `rtt`, `debug` or the IDE debugger.
- **`… can only be read while the core is halted`** (`debug reg`, `fault`,
  `check`): CP15 and core registers need a stopped core. Run
  `tracebridge debug break`, or `check --halt`.
- **Timeouts during flash**: raise `trace32.operation_timeout` and look at the
  PowerView AREA window.
- **RTT waits forever**: the firmware has not initialized RTT yet (press
  Continue in the debugger), or `project.program` does not match the loaded
  program name; pass `--cb` to bypass the symbol.

## Development

```sh
cargo test                 # unit, fake-server and end-to-end tests
cargo clippy --all-targets -- -D warnings
```

The workspace has two crates: `crates/t32rcl`, a pure Rust port of the parts
of Lauterbach's `lauterbach-trace32-rcl` Python library (TCP only) that
tracebridge uses, and `crates/tracebridge`, the CLI. `t32rcl` is tested
byte-for-byte against traffic recorded from the Python library; see
`crates/t32rcl/tools/` and the `capture_proxy` and `smoke` examples for
recording against a real PowerView.

## License

MIT. `crates/t32rcl` contains code ported from lauterbach-trace32-rcl
(Copyright (c) 2020 Lauterbach GmbH, MIT); see [NOTICE](NOTICE).
