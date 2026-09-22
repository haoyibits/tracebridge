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
| `tracebridge adapter` | DAP proxy in front of `t32debugadapter`; the IDE starts it |

Supported hosts: macOS (Apple silicon, Intel) and Linux (x86_64, aarch64).

## Install

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
$EDITOR trace32.toml        # program, elf, [target], flash.script
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

## Configuration

`trace32.toml` marks the project root. Every relative path in it is relative
to the directory that contains it. `tracebridge init` writes this template:

| Section | Keys |
|---|---|
| `[project]` | `program` (TRACE32 program name), `elf` |
| `[target]` | `cpu`, `cores`, `mem_access`, `jtag_clock`, `dual_port` |
| `[rtos]` | `config`, `menu`, `show_tasks` (empty = no RTOS awareness) |
| `[flash]` | `script` (must support `PREPAREONLY`), `args` |
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
| `PROGRAM_NAME`, `T32_CPU`, `T32_CORES`, `T32_MEMACCESS`, `T32_JTAG_CLOCK`, `T32_DUALPORT`, `T32_FLASH_SCRIPT` | the same keys | **used** (clears the value) |
| `T32_FLASH_ARGS` | `flash.args`, split like a shell command line | **used** (no arguments) |
| `T32_RCL_PORT`, `T32_DAP_PORT`, `T32_DAP_BACKEND_PORT`, `T32_DAP_BACKEND_TIMEOUT`, `T32_TIMEOUT` | ports and timeouts | ignored |
| `T32_DAP_DEBUG=1` | debug logging of `t32debugadapter` | |

Runtime files (the PowerView log, the toolbar script) are written to
`<project>/.tracebridge/`, which contains its own `.gitignore`.

### Flash script contract

The project owns its flash script. It must support Lauterbach's `PREPAREONLY`
convention: set up the target, declare the flash, and return without
programming. tracebridge then runs:

```text
FLASH.ReProgram ALL /Erase
Data.LOAD.Elf <elf>
FLASH.ReProgram OFF
SYStem.Down
SYStem.Up
```

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

## Migrating from the Python trace32-bridge

1. Install tracebridge. Python and `lauterbach-trace32-rcl` are no longer
   needed.
2. Move `trace32.toml` from the toolkit directory to the project root and
   delete `root = ".."`: the project root is now the directory that contains
   the file (tracebridge rejects `project.root`).
3. Make `flash.script` relative to the project root (for example
   `flash.script = "tools/flash.cmm"`), or keep a `~~/` TRACE32 path.
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
  or `load` before `rtt` or the debugger.
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
