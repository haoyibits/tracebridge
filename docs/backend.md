# Backend used by rustvisor

The public surface is `tracebridge::backend`. It grows from concrete calls in
rustvisor's `tools/board-tests`; it is not a generic test workflow framework.

```rust,no_run
use std::path::Path;
use tracebridge::backend::{Project, Session, CheckOptions};

# fn example() -> Result<(), Box<dyn std::error::Error>> {
let mut project = Project::load(Path::new("trace32.toml"))?;
project.images("build/el2.elf".into(), vec!["build/guest.elf".into()], "hypervisor".into());
// PowerView must already be running (tracebridge open).
let mut session = Session::connect(project)?;
session.flash()?;
session.verify()?;
let report = session.check(Path::new("checks/boot.toml"), &CheckOptions {
    variant: Some("default".into()), halt: true, dry_run: false,
})?;
if !report.passed() { return Err(report.summary().into()); }
session.resume()?;
# Ok(())
# }
```

Loading a project does not connect, start PowerView or touch a target. Connecting
uses RCL attach/version negotiation; it does not issue a target reset. PowerView
launch remains with the CLI so its generated toolbar points to the correct CLI
executable, rather than to an embedding runner.

`images` explicitly replaces both the primary and extra images, including an
empty extra list. `flash` and `load` use the existing sequences, including flash
script selection, derivative arguments, PRACTICE-idle checks and cleanup of
FLASH.ReProgram on failure. `load` retains the CLI's symbol-load-and-resume
behavior. `reset` uses project CPU, core and memory-access settings.

Check failures are typed `CheckReport` values with `passed() == false` and exit
code 3; inability to execute is an error. verify/run-until mismatches are errors.
Clock and trace operations return their existing structured JSON data. Commands
and state mutations invalidate PER metadata before execution, including partial
failures. There is no reconnect/retry policy that could repeat an operation.

Bulk RAM reads use RCL memory requests and decode little-endian u32 values. The
caller must supply known readable ordinary RAM; the backend bounds count,
alignment and arithmetic, while rustvisor bounds target-provided pointers to the
measured Guest's RAM. CP15/MMIO keep their existing expression-based reads and
are not automatically converted to bulk accesses.

`loadable_content` reuses verify's ELF parser. `evidence_inputs` resolves the
selected flash script and clock description with the effective configuration;
the runner keys their contents, its linked executable and project criteria.
Image output paths are excluded from the configuration identity so a new
per-run evidence directory does not invalidate otherwise identical content.

The CLI behavior and wire protocol are checked by existing tests. Public API
integration tests in `tests/backend_e2e.rs` use fake PowerView, never a board.

## Board validation, 2026-10-28

The linked backend was exercised by rustvisor's new runner on the SR6P6 board:
33 forced image runs and 32 intermediate resets passed; cache/no-flash behavior
and five representative latency data-path variants passed. All 14,076 words
read by the bulk RAM interface matched the original scalar Data.Long reader.
A stale diagnostic PC criterion was fixed in rustvisor; backend code did not
need a board-specific change. This verifies the exercised paths, not a new full
performance baseline. Evidence and scope are documented in the sibling
rustvisor-sdk `docs/02-verification.md` section 2.2 and
`target/board-tests/board-validation-20261028/`.
