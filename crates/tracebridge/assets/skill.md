---
name: tracebridge
description: Use this skill in a project that has a trace32.toml, or when the user wants to flash or load the project's ELF with TRACE32, start PowerView for the project, read SEGGER RTT output, decode an AArch32 Hyp fault, read a peripheral register by its PER name, compute the clock tree, run a check file, or compare target memory with the ELF. tracebridge is the user's CLI around TRACE32 PowerView. Generic debugging (halt, run, step, core registers, memory, breakpoints, source, stack, symbols) goes through t32mcp and lauterbach-trace32-debug-basic instead.
metadata:
  version: "{version}"
---

# tracebridge next to t32mcp

Two tools share one PowerView and its Remote API port (`trace32.rcl_port` in
trace32.toml, default 20000, the same port t32mcp uses):

- t32mcp, Lauterbach's MCP server, with the `lauterbach-*` skills: generic
  debugging. Follow their tool order.
- tracebridge, this CLI: the project's setup, flashing, and the analyses
  Lauterbach does not ship.

The user installed this skill to split the work this way, so running
`tracebridge` from the shell is what the user asks for, despite t32mcp's
advice not to drive the debugger by other means.

## Which tool

| Task | Use |
|---|---|
| State, halt, run, step, core registers, memory, breakpoints, source, locals, stack, symbols | t32mcp: `trace32_get_status`, `lauterbach-trace32-debug-basic` scripts |
| One PRACTICE command or expression | t32mcp `trace32_run_command`, e.g. `PRINT Register(PC)` |
| Resolved configuration, paths, ports | `tracebridge config` |
| Start PowerView for this project | `tracebridge open` |
| Program the flash with the project's ELF (and `extra_elf`) | `tracebridge flash` |
| Attach without reset and load symbols | `tracebridge load` |
| Which flash script a chip uses | `tracebridge chips <chip>` |
| A register by PER name or field, with its address | `tracebridge debug reg <name>... --json` |
| AArch32 Hyp fault: vector, HSR decoded, fault addresses | `tracebridge debug fault --json` |
| Target memory against the ELF's loadable content | `tracebridge debug verify --json` |
| A check file of expected register values | `tracebridge debug check <file> --json` |
| Clock tree with frequencies | `tracebridge debug clock --json` |
| Target output over SEGGER RTT | `timeout 10 tracebridge rtt --output-only` |

## Rules

- Run tracebridge inside the project: it finds trace32.toml upwards from the
  current directory. Put `--json` after the command's arguments.
- Ask the user before `flash`, `load`, `open`, `debug check --halt`, and
  the `debug` commands `attach`, `down`, `break`, `go`, `cmd`. `flash` resets
  the target and erases flash; it can take minutes, so raise the shell timeout.
- The read-only commands (`config`, `chips`, and `debug` `reg`, `fault`,
  `verify`, `check` without `--halt`, `clock`) never change the target.
- `tracebridge debug` exit codes: 0 ok, 1 error, 2 usage error, 3 a check
  failed or `verify` found differences.
- Do not guess chip details. The flash scripts and clock descriptions live in
  `~/.config/tracebridge/`; `tracebridge chips` and `debug clock` say which
  one they use.

## Gotchas

- One PowerView runs one PRACTICE script at a time. `flash` and `load` stop
  with "PowerView is busy" while a script is active (also one t32mcp started),
  and t32mcp reports busy while `flash` runs its flash script. Wait, and never
  end a script you did not start without asking.
- After `flash` and `load` the target runs. Halt it (`set_halted.cmm`) before
  reading core registers through t32mcp.
- `rtt` keeps reading until it is stopped; always bound it with `timeout`.
- "no PowerView on RCL port": run `tracebridge open`, then retry; t32mcp
  reconnects by itself.
