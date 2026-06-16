# Debugging costar Simulations

This document covers techniques for debugging firmware running inside costar's
stackful-fiber simulator.  Because all simulated tasks live on `corosensei`
coroutines rather than host threads, some debugging approaches differ from
normal native programs.

## Quick Reference

| Problem | Tool | Section |
|---------|------|---------|
| Simulator hangs (infinite loop) | Watchdog, Tier 3 instrumentation | [Hangs](#detecting-and-diagnosing-simulator-hangs) |
| Trace divergence | `costar replay`, `--symbolicate`, `--diff` | [Trace Analysis](#trace-analysis) |
| C crash / segfault | ASan, binary search, guard pages | [Crash Investigation](#crash-investigation) |
| Coroutine-level debugging | GDB with coroutine frame inspection | [GDB Integration](#gdb-integration) |
| LLDB (macOS) | LLDB with coroutine stacks | [LLDB Integration](#lldb-integration) |
| Panic across C ABI boundary | `catch_unwind` boundary, trace inspection | [Panic Diagnosis](#panic-diagnosis) |
| RefCell re-entrancy panic | `sim_abi.h` conventions | [RefCell Issues](#refcell-re-entrancy) |

## Trace Analysis

### Symbolicated Traces

Use `--symbolicate` to resolve task IDs to human-readable names in trace output:

```bash
# Symbolicated golden trace
cargo run -- --symbolicate --golden

# Symbolicated human-readable trace
cargo run -- --symbolicate
```

Output shows `name="Sender"` / `name="Receiver"` alongside each task-resume
and task-yield event:

```
           0 task-created id=1 name="Sender"
           0 task-resume id=1 name="Sender" reason=scheduler
           0 task-yield id=1 name="Sender" reason=SleepUntil(1)
```

Task names come from `TaskCreated` events emitted by `sim_create_task()`.
For tasks created by the RTOS kernel (e.g., idle tasks, timer daemon), use
`sim_register_symbol(task_id, name)` from C code.

### Replaying a Trace

The `replay` subcommand reads a JSONL or human-readable trace file and
replays it with symbolication:

```bash
# Capture a JSONL trace
cargo run -- --golden --trace-format jsonl > simulation.jsonl

# Replay with symbolication
cargo run -- replay simulation.jsonl

# Step through events one at a time (press Enter, 'q' to quit)
cargo run -- replay simulation.jsonl --step
```

The replay command auto-detects JSONL vs human-readable format and resolves
task names from `TaskCreated` events.

### Comparing Traces

```bash
# Compare current output against expected golden trace
cargo run -- --golden --diff tests/traces/expected_queue_ping_pong.trace

# JSONL comparison
cargo run -- --golden --trace-format jsonl --diff expected.jsonl
```

Exit code 0 on match, 1 on mismatch.  Use `--verbose` for detailed output.

## Detecting and Diagnosing Simulator Hangs

### Wall-Clock Watchdog

```bash
# Warn if simulation exceeds 5 seconds of wall-clock time
cargo run -- --watchdog 5
```

The watchdog is a coarse indicator — it detects the hang but doesn't tell you
*where*.

### Tier 1: Function-Entry Instrumentation

Build with `SIM_INSTRUMENT_FUNCTIONS=1` to insert budget checks at every C
function entry:

```bash
SIM_INSTRUMENT_FUNCTIONS=1 cargo run
```

When the budget counter (default 1,000,000 entries) is exceeded, the fiber
yields with `BudgetExceeded`.  If the simulation still hangs, the CPU-bound
code is either:
- A tight loop with no function calls (go to Tier 3), or
- Calling functions deeper than the budget limit (increase with
  `sim_budget_set_limit()`).

### Tier 2: Manual Loop Hooks

Insert `SIM_LOOP_POLL()` in tight loops:

```c
#include "sim_abi.h"

while (1) {
    SIM_LOOP_POLL();  // yields if budget exceeded
    // tight work
}
```

### Tier 3: Edge Instrumentation (Clang only)

Build with `SIM_INSTRUMENT_EDGES=1` to insert callbacks at every basic-block
edge:

```bash
SIM_INSTRUMENT_EDGES=1 cargo run -- --mode tight-loop
```

This is the only tier that can preempt a bare `while(1){}` loop.  The edge
counter throttle defaults to every 10,000 edges — adjust with
`sim_budget_set_limit()` for finer granularity.

If the tight-loop demo hangs without edge instrumentation, the burner task
has an un-preempted infinite loop.  With edge instrumentation, it produces
335 interleaved events (burner + watchdog).

## Crash Investigation

### Address Sanitizer

Run with ASan for memory bugs:

```bash
# Nightly Rust required
cargo +nightly test-asan
```

This catches use-after-free, buffer overflows, and double-frees in both
Rust and C code.

### Leak Sanitizer

```bash
cargo +nightly test-lsan
```

Detects memory leaks.  Some leaks from C firmware initialisation are
expected (simulator lifetime is the process lifetime); focus on leaks
that grow with simulation time.

### Binary-Search Crash Isolation

When a crash occurs deep inside a coroutine, use `_exit()` to binary-search
the crash location.  Insert `_exit(0)` calls at checkpoints and move them
forward/backward until the crash disappears/appears.  See
`references/crash-isolation-with-exit.md` for the full technique.

### Stack Guard Pages

Enable stack guard pages for coroutine stacks by setting
`RUST_MIN_STACK` or using `mmap` with `PROT_NONE` guard regions.  This
catches stack overflows from C firmware that allocates too-small stacks.

## GDB Integration

### Starting costar under GDB

```bash
# Build with debug info (default)
cargo build

# Run under GDB
gdb --args target/debug/sim-runner --mode deterministic
```

### Coroutine Stack Inspection

When a coroutine suspends, its stack is frozen.  You can inspect it:

```
(gdb) info threads
# Only one host thread.  Coroutines are not OS threads.

(gdb) bt
# Shows the current host stack, which is either:
# - The scheduler drain loop (if between fiber resumes), or
# - Inside a coroutine (if a fiber is running)
```

To identify which task is running, check `CURRENT_TASK_ID`:

```
(gdb) p/x sim_ffi::CURRENT_TASK_ID
```

### Debugging Coroutine Body Code

When a C function runs inside a coroutine, set breakpoints on the C
function name:

```
(gdb) b vTaskA
(gdb) c
```

The breakpoint fires when the coroutine containing `vTaskA` is resumed.

### Panic Backtraces

Set `RUST_BACKTRACE=1` for Rust panic backtraces:

```bash
RUST_BACKTRACE=full cargo run
```

Panics inside fibers are caught by the `catch_unwind` boundary — the task
is marked `Faulted` and the simulation continues.  The panic message is
recorded in the `Fatal(PanicCrossedCAbi)` trace event.

## LLDB Integration (macOS)

### Starting costar under LLDB

```bash
# Build with debug info (default)
cargo build

# Run under LLDB
lldb target/debug/sim-runner -- --mode deterministic
```

### LLDB Commands

```
(lldb) b vTaskA
(lldb) run
(lldb) bt
(lldb) frame variable
(lldb) expr sim_now_ticks()
```

LLDB on macOS works identically to GDB for coroutine debugging — the
coroutine stacks appear as regular call frames during resume.

## Panic Diagnosis

### Panic Across C ABI

If a Rust panic unwinds across a C function call (crossing the FFI
boundary), the process will abort with a fatal error.  The simulator's
`catch_unwind` boundary around `fiber.resume()` catches the panic and:

1. Marks the task `Faulted` (no further resumption).
2. Records `Fatal(PanicCrossedCAbi)` in the trace.
3. Continues the simulation with other tasks.

To diagnose which task panicked, check the trace for the `Fatal` event
immediately after the last `TaskResume` for the faulted task.

### RefCell Re-entrancy

**Symptom**: `thread 'main' panicked at 'already borrowed: BorrowMutError'`

**Root cause**: A C ABI function called from within a fiber tries to
borrow `SIM_GLOBAL` while the scheduler already holds a borrow.

**Fix**: Use re-entrant-safe primitives instead:
- `sim_now_ticks()` → `SIM_NOW` (AtomicU64)
- `sim_trace_u32()` → `TL_TRACE` (thread-local)
- `sim_port_yield()` → `ACTIVE_YIELDER` (thread-local cell)
- `sim_host_block_on_fd()` → `CURRENT_TASK_ID` (AtomicU64)

Never call `sim_create_task()` or any function touching `SIM_GLOBAL`
from within a fiber.

## Common Debugging Workflows

### "My simulation hangs after N events"

1. Run with `--watchdog 5` to confirm the hang.
2. Check the trace for the last event — which task was running?
3. Enable Tier 1 instrumentation: `SIM_INSTRUMENT_FUNCTIONS=1 cargo run`
4. If still hanging: enable Tier 3 instrumentation (requires Clang):
   `SIM_INSTRUMENT_EDGES=1 cargo run`
5. If the hang is in a C tight loop, add `SIM_LOOP_POLL()` macros.

### "My golden trace test fails on a different OS"

1. Capture the actual trace: `cargo run -- --golden > actual.trace`
2. Compare with expected: `diff -u expected.trace actual.trace`
3. Common causes:
   - CRLF vs LF line endings (Windows): use `tr -d '\r'` before diffing
   - Nondeterministic iteration order: check for `HashMap` usage
   - Wall-clock time leaking into virtual time: check for `Instant::now()`
   - Conditional compilation: verify `#[cfg]` gates are consistent

### "My Zephyr app crashes with CODE_UNREACHABLE"

This is expected when any Zephyr thread's entry function returns.  Zephyr
threads should block forever instead of returning:

```c
static void my_thread(void *a, void *b, void *c) {
    // ... work ...
    k_sleep(K_FOREVER);  // block forever, do NOT return
}
```

The main thread can return — it triggers `CODE_UNREACHABLE` → `_exit(0)`,
which terminates the simulation cleanly.

## Sanitizer Documentation

### ASan (Address Sanitizer)

```bash
# Run tests with ASan
cargo +nightly test-asan
# Alias defined in .cargo/config.toml:
# test-asan = ["test", "--target=x86_64-unknown-linux-gnu", "-Zsanitizer=address"]
```

ASan catches:
- Heap buffer overflow
- Stack buffer overflow
- Use-after-free
- Double-free
- Use-after-return (requires `ASAN_OPTIONS=detect_stack_use_after_return=1`)

Note: ASan is only available on `x86_64-unknown-linux-gnu` target.
C code in the FreeRTOS payload is also instrumented.

### LSan (Leak Sanitizer)

```bash
cargo +nightly test-lsan
# Alias: test-lsan = ["test", "--target=x86_64-unknown-linux-gnu", "-Zsanitizer=leak"]
```

LSan reports memory that was allocated but never freed.  Ignore leaks from
`Box::leak` (used intentionally for task names and trace labels) and from
static globals (e.g., `inventory::submit!` registrations).

### CI Integration

Sanitizer jobs run in CI on every push/PR (`sanitizers` matrix in
`.github/workflows/ci.yml`).  Jobs are allowed to pass with warnings
(ASan may report leaks from C code / system libs).  Inspect
`sanitizer-output.log` for real issues.
