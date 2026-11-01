# Debugging firmware with GDB

Firmware on costar is host code: an image is a shared object loaded into the
`costar` process (or `costar-grpc`, or your own program). So GDB debugs it
like any other code: breakpoints by function name, stepping, watchpoints,
`print`. What costar adds:
- ways to stop at a virtual time or at a trace record (`--break-at-ms`,
  `--break-on`);
- a GDB extension, [`tools/gdb/costar.py`](../tools/gdb/costar.py), that
  shows machines and RTOS threads. GDB can't see those threads by itself,
  because they are costar fibers, not OS threads.

Runs are deterministic, so a failure you saw once happens again at the same
virtual time and the same trace record, every run. That makes "rerun and
stop just before it goes wrong" a reliable workflow.

## Build for debugging

```bash
cmake -S . -B build -DCMAKE_BUILD_TYPE=Debug   # -g -O0 for the engine and FreeRTOS images
cmake --build build -j
```

Without a debug build you still get function names in backtraces (from the
symbol tables), but no line numbers or variables. Zephyr images always have
debug information: Zephyr builds with `-g`. The extension works in any
build, because costar always compiles the engine types it reads with
debug information (see [How it works](#how-it-works)).

## Stopping at the right moment

```bash
gdb -x tools/gdb/costar.py --args \
    build/costar run tests/scenarios/link_nodes.toml --images build/firmware --break-on responder:got_ping=2
```

```
(gdb) run
costar: break on 12000000 m1 got_ping 2

Program received signal SIGTRAP, Trace/breakpoint trap.
(gdb) bt
...
#14 0x00007ffff7f5faf0 in costar_trace () from /tmp/costar-freertos_link_node-cINZ3O.so
#15 0x00007ffff7f55259 in prvHandler () from /tmp/costar-freertos_link_node-cINZ3O.so
#16 0x00007ffff7f5f0f0 in prvTaskEntry () from /tmp/costar-freertos_link_node-cINZ3O.so
```

- **`--break-on [MACHINE:]LABEL[=VALUE]`** (repeatable) stops when a trace
  record with that label is recorded, optionally only on one machine and
  only with one value. The stop happens inside the call that records it.
  For the firmware's own records (`costar_trace`, console text, frames it
  sends, register writes), that's the firmware's call, so its frames are on
  the stack: `up` (or `frame 15` above) and look around. Records the host
  makes (a frame arriving, a plant setting a register) stop in costar's
  code.
- **`--break-at-ms N`** stops when virtual time reaches N ms, before
  anything due then runs. Every RTOS thread is suspended at that point. It's
  a good moment to set breakpoints on firmware functions that run too often
  to break on from the start:

  ```
  (gdb) run                    # stops at 25 ms
  (gdb) break vTaskSwitchContext
  (gdb) continue
  ```

`continue` carries on, and the run finishes with the same trace it would
have had anyway. Both options stop with `SIGTRAP`, and only when a debugger
is attached. Without one, `costar run` prints `costar: break ... (no
debugger attached: carrying on)` and goes on, which is a quick way to check
that a break triggers where you expect. They are options of `costar run`
only. Don't combine them with `--timeout-s`, which keeps counting while you
sit at a breakpoint.

Breakpoints by function name work before the image is loaded: GDB makes
them pending (`set breakpoint pending on` answers its question in scripts).
Each machine loads its own copy of its image, so a breakpoint has one
location per machine running that function. Use `costar machines` to see
which machine you stopped in.

### The replay workflow

1. A scenario fails: `costar test` says `expect.event 2: expected
   responder got_ping 3 at or before 25000000 ns, but it never happened`, or
   a golden trace differs at line 40.
2. Pick the last record that was still right from the trace (say
   `22000000 m1 rx link 5003`). Rerun under GDB with `--break-at-ms 22`,
   or with `--break-on responder:rx`, which stops at every `rx` record:
   `continue` until you reach that one.
3. Step through what the firmware does next. The rerun is the same run, so
   the bug is there every time.

## The `costar` commands

Load them with `source tools/gdb/costar.py`, or `-x tools/gdb/costar.py`
on the command line. An installed costar has them in
`share/costar/gdb/costar.py`. To have them always, put the `source` line in
`~/.gdbinit`.

| Command | Shows |
|---|---|
| `costar machines` | every machine: name, `m<id>`, virtual time, CPU state, thread count. `*` marks the machine whose code is running on the selected thread. |
| `costar threads [MACHINE]` | a machine's RTOS threads: state, name, code, and where each suspended thread waits |
| `costar bt [MACHINE:]THREAD [N]` | the backtrace of one of them; extra arguments go to `bt` |

```
(gdb) costar threads
board (m0), 1000000 ns, cpu awake
   0  suspended    -            costar::(anonymous namespace)::run_reset_vector(void*)  in xPortStartScheduler < vTaskStartScheduler < main
   1  suspended    sender       FreeRTOS task prvSender  in vPortYield < vPortExitCritical < xQueueGenericSend
   2  running here receiver     FreeRTOS task prvReceiver
   3  suspended    IDLE         FreeRTOS task prvIdleTask  in vPortYield < vPortTaskLoopIteration < prvIdleTask
   4  suspended    Tmr Svc      FreeRTOS task prvTimerTask  in vPortYield < prvProcessTimerOrBlockTask < prvTimerTask
(gdb) costar bt 1
```

- **States.** `running here` is the thread whose code is on the selected
  GDB thread's stack now. `current` is the thread the RTOS last switched to,
  when its code isn't running (the CPU is halted, or GDB stopped in
  costar's code). `suspended` threads wait for the RTOS to switch back to
  them. `not started` threads have never run.
- **Names.** The name is the one the RTOS gave the thread
  (`costar_thread_describe()`, ARCHITECTURE.md §2): FreeRTOS's task name,
  or a Zephyr thread's with `CONFIG_THREAD_NAME=y`; `-` if it has none.
  FreeRTOS tasks are also shown by their task function, and Zephyr threads
  by their slot in the native_sim runner. Thread 0 is the reset thread,
  which ran the image's reset vector.
- **Naming machines.** `MACHINE` is the scenario's name or `m<id>`, and
  defaults to the machine whose code is running. In a program with several
  Worlds (`costar-grpc` sessions, tests), `costar machines` lists them as
  `w0.initiator`, `w1.initiator`, …, and those names select one.

`costar bt` works by loading the thread's saved registers into the selected
GDB thread for a moment, running `bt`, and putting them back. It reads
x86_64 contexts only.

### VS Code

With the C/C++ extension (`cppdbg`):

```json
{
  "name": "costar run",
  "type": "cppdbg",
  "request": "launch",
  "program": "${workspaceFolder}/build/costar",
  "args": ["run", "tests/scenarios/link_nodes.toml", "--images", "build/firmware",
           "--break-on", "responder:got_ping=2"],
  "cwd": "${workspaceFolder}",
  "MIMode": "gdb",
  "setupCommands": [
    { "text": "-enable-pretty-printing", "ignoreFailures": true },
    { "text": "set breakpoint pending on" },
    { "text": "source ${workspaceFolder}/tools/gdb/costar.py" }
  ]
}
```

Breakpoints set in firmware sources in the editor bind when the image
loads. In the Debug Console, `-exec costar threads` runs the commands.

## Other programs

- **`costar-grpc`, the Python client.** Run the program under GDB
  (`gdb --args build/costar-grpc --listen 127.0.0.1:9321`). Every session
  is a World of its own, on its own thread, so GDB may stop on any of them.
- **Your own programs and tests.** The `costar` commands see only the
  Worlds a program lists: keep a `costar::DebugEntry entry(&world);`
  (`costar/debug.hpp`) for each World you want to see, declared after it so
  it goes first, or create sessions with `Session(scenario, plants, true)`.
- **Attaching (`gdb -p`).** Images loaded before you attach have already
  had their copies deleted, so GDB can't read their symbols. Start the
  program under GDB, or attach before the sessions you care about are
  created.

## Sanitizers

[README: Sanitizers and coverage](../README.md#sanitizers-and-coverage).
- **Stopping on a report.** Under GDB, set
  `ASAN_OPTIONS=abort_on_error=1:detect_leaks=0` to stop on an ASan report
  with the firmware's frames on the stack. LeakSanitizer can't run under a
  debugger.
- **Stack traces.** AddressSanitizer reports already name firmware
  functions: in ASan builds the image copies stay on disk until they're
  unloaded.

## How it works

- **Finding the machines.** GDB can only find what something global points
  to. `costar_debug_worlds` (`src/costar/debug.hpp`) is a list of Worlds,
  much as `_r_debug` lists loaded objects for GDB. A World doesn't list
  itself: the program running it does, with a `costar::DebugEntry` that
  lives as long as the World. `costar run`, `costar test` and costar-grpc's
  sessions are listed; the engine never reads the list (ARCHITECTURE.md §3).
- **Reading the engine's data.** The extension reads Worlds, Machines, Cpus
  and threads with GDB's knowledge of their types. That needs debug
  information, so `src/costar/debug.cpp` and `cpu.cpp` are always compiled
  with `-g`, whatever the build type. It adds debug information only, not
  code.
- **Breaks.** A break raises `SIGTRAP` from inside the trace record's call,
  or from a host event at the break time, if `/proc/self/status` shows a
  tracer. That event is scheduled before the world is built, so it's the
  first thing at its instant: events at one instant run in the order they
  were scheduled, and CPUs run after them. costar's own work for a firmware call (recording the trace, say)
  runs on the host stack, not the thread's (ARCHITECTURE.md §2), so a break
  on a record the firmware made waits until that work is done and the call
  is back on the firmware thread's stack (`Cpu::break_in_call()`): `bt` then
  shows the firmware's frames.
- **Symbols for images.** Each image is a private copy
  (`/tmp/costar-<image>-XXXXXX.so`) loaded with `dlopen`. GDB learns about
  it from the dynamic linker as it loads, so its symbols and pending
  breakpoints work. Under a debugger, costar keeps the copy on disk until
  the image is unloaded, so GDB can read it again later. A run killed under
  the debugger leaves its copies behind, as in ASan builds.
