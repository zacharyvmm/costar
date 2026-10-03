# Scheduling Architecture

## Deadlines, masking and owed ticks

**Callbacks always run at their deadline.**  A peripheral callback
(`sim_schedule_event`) runs at its tick, even when interrupts are masked
and even when a budget tick is owed (a busy task used up its budget at a
World step's limit).  What waits is task work:

| Situation | Runs at its time | Deferred |
|---|---|---|
| Interrupts masked | virtual time, the running task's CPU budget, callbacks | IRQ/ISR delivery, kernel tick servicing, task switches: until the unmask |
| Budget tick owed | callbacks (and the IRQs they raise, if unmasked) | resuming tasks: until the tick is charged, at the next tick |

Callbacks due at the current tick also run before a charged tick moves
time on.

### Interrupt masking

One rule for every scheduler path.  Interrupts are masked inside a
critical section, after `portDISABLE_INTERRUPTS()`, and when host code
masks a machine between steps.  While they are masked:

- **Kept:** virtual time keeps advancing.  The running task's CPU budget
  is still accounted: on a FreeRTOS machine, exhausting it suspends the
  task to the engine, which charges the tick of CPU time, and then the
  same task resumes.  Peripheral callbacks (`sim_schedule_event`) keep
  running at their deadlines.
- **Deferred until the unmask:** IRQ/ISR delivery, kernel tick servicing
  (the ticks accumulate, uncounted by the kernel, so no delayed task wakes
  and no time slice ends), and task switches (latched, the same task
  resumes).  At the unmask, wherever it happens (a task, a peripheral
  callback, host code between steps), the held-off ticks are serviced at
  once, so the kernel's tick count is current for what runs next.  Pending
  IRQs are then delivered, and the latched switch happens: at once in a
  task, after the callback returns in a step, or at the next step (which
  the machine is woken for) after host code unmasks between steps.
- **A retired task's mask dies with it:** the critical nesting and mask a
  task holds are its own, as a port saves them per task.  A task retired
  while holding them (deleted — also when a budget tick cuts its own
  deletion short inside the kernel's critical section —, finished,
  faulted) releases them once, at retirement; the next task starts
  unmasked.  This holds whoever retires it: a peripheral callback or host
  code that deletes a task suspended inside its own critical section
  releases that task's mask too.  The mask is per context: each task,
  and scheduler context (host code between steps, a peripheral
  callback), holds its own critical nesting and its own disable request,
  and enters and exits only its own critical sections; interrupts are
  masked while any context contributes.  (`portENABLE_INTERRUPTS()` is
  the CPU's one flag, as on hardware: it clears every disable request.)
  One release, used by every retirement path (return, exit, fault,
  self-deletion, deletion by another task, a callback or host code),
  removes exactly the retiring task's contribution.  Host code's or a
  callback's contribution survives any task's retirement, with its
  pending switch: the task FreeRTOS selects next waits for that unmask.
  When the release does unmask, the tick interrupts the mask held off are
  serviced at once, at that point (a deletion inside the kernel's
  critical section unmasks at that section's exit), so the callback or
  host code that continues sees the kernel's tick count current; a
  switch they request is latched until the callback returns.
- **A deleted task:** if host code (or a callback) deletes the selected
  task while interrupts are masked by host code, FreeRTOS selects another at once (the
  deleted one is gone; its TCB is left to the kernel's cleanup), but the
  task it selects does not run before the unmask: the machine idles
  masked meanwhile, as for a latched switch.
- **A task that deleted itself:** `vTaskDelete(NULL)` inside a critical
  section puts the TCB on the termination list at once, but the switch
  away waits for the unmask, so the task runs on until then.  (Holding
  the scheduler lock, the kernel's `configASSERT()` stops it inside
  `vTaskDelete()`.)  Its TCB is the idle task's to free, and the engine
  never hands it to the kernel again, whatever the task does next: if it
  faults or returns it is retired without being suspended or deleted a
  second time (only a scheduler lock it holds is released), and if it
  waits (sleeps, blocks on host I/O) its fiber stops there for good, as
  the switch away from it would have done.
- **Wake-ups:** a World wakes a masked machine only for what the masked
  path can execute: callback deadlines, and the next tick while the
  running task's budget is used up.  It never wakes it immediately for
  deferred work.  An expired virtual timer latches its IRQ once.

The native and Zephyr schedulers have no tick interrupt and never preempt
a task.  For them, masking defers IRQ/ISR delivery only.

## Who owns scheduling?

**The RTOS kernel owns every scheduling decision.** costar is the fiber
substrate — it provides stackful coroutines and advances virtual time, but
never selects which thread runs next.

### FreeRTOS

The unmodified FreeRTOS kernel (`tasks.c`, `queue.c`, `list.c`, `timers.c`)
runs inside Rust-managed fibers, one fiber per task.

- **Task creation.** `xTaskCreate()` / `xTaskCreateStatic()` create the
  task's fiber from the `traceTASK_CREATE` hook, at start-up or at runtime
  from a running task.  Firmware does not call any simulator API to create
  tasks.  (The old `sim_create_task()` + `sim_bridge_register()` pattern
  still works and maps onto the same fiber, in either order; the pair is
  matched on entry point, parameter, name (as far as FreeRTOS keeps it,
  `configMAX_TASK_NAME_LEN - 1` bytes, compared as raw bytes, so a name
  cut inside a UTF-8 character still matches) and priority (clamped below
  `configMAX_PRIORITIES` as FreeRTOS clamps it), and only while the task is
  alive.  Calls that differ in any of these are independent tasks.
  A task
  created with `sim_create_task()` alone is scheduled by FreeRTOS like a
  native Rust task, below.)
- **Switching.** `portYIELD()` suspends the running fiber; the engine then
  calls `vTaskSwitchContext()` — what PendSV does on a Cortex-M — and
  resumes the fiber of the task FreeRTOS placed in `pxCurrentTCB`.  A yield
  requested inside a critical section or from ISR context is pended until
  interrupts are unmasked.  The same holds for every switch the engine
  makes itself (after a tick, for input, for the parked idle task of a
  World step); see "Interrupt masking" above.
- **Time.** Virtual time advances only when the idle task runs (every
  application task is blocked): the engine jumps to the next delayed-task
  wake-up or peripheral event and runs the tick interrupts in between.  A
  task that exhausts its instrumentation budget is charged one tick of CPU
  time, so a busy loop still lets time pass and higher-priority tasks
  preempt it.  The switch follows the kernel's tick handler: with
  `configUSE_TIME_SLICING` 0 (the shipped configuration) an equal-priority
  task does not take over from a busy one.  In a World step, a budget
  exhausted at the step's limit is charged at the start of the next step
  (bounded or not), before any task runs, so the order matches standalone
  stepping.  Input that arrived during that tick is still taken at it;
  another step within the same tick runs nothing.
- **Host I/O and the delay ABI.** A task in `sim_host_block_on_fd()` is
  suspended in the kernel until the host poller reports its descriptor
  ready, and `sim_task_delay_until()` blocks it on FreeRTOS's delayed list.
  FreeRTOS keeps scheduling the machine's other tasks meanwhile.  Setting
  up a wait is one step with blocking in it: from registering what ends
  the wait until the task has suspended, its budget preemption is
  deferred (a tick it used up is that task's debt, charged once its wait
  ends, whatever other tasks' waits do meanwhile), so no
  peripheral callback or tick can run in between and, say, cancel a wait
  whose task has not blocked yet.  The same holds for every kernel batch
  the engine runs on a task's or its own behalf (tick servicing and
  charging, also the held-off ticks serviced at an unmask, starting the
  kernel, adopting tasks, ending waits, the scheduler step itself): a
  budget tick used up inside one is charged right after it, never in the
  middle.  Deleting
  a task that waits on a descriptor cancels the wait.  Deregistering a
  descriptor (`sim_host_deregister_fd()`) ends every wait on it — every
  task waiting on it, as readiness wakes every one of them —, on every
  scheduler: the waiter returns from `sim_host_block_on_fd()` without
  readiness (as it does at once for a descriptor the poller does not
  monitor), and the machine is no longer kept running for it.  The end
  is latched for that wait: registering the descriptor again before the
  waiter resumes does not revive it.  A task that stops for good (it
  faults, exits, finishes or is deleted) leaves no I/O registration
  behind — no kernel wait, poller association, readiness or cancellation
  latch — and a fault is terminal: nothing makes the task runnable again. On every
  scheduler (FreeRTOS, native, Zephyr) a waiter whose descriptor is
  already ready runs before virtual time moves to a peripheral callback,
  so a chain of callbacks cannot starve host I/O.
- **Native Rust tasks.** A task from `spawn_rust_task()` on a FreeRTOS
  machine gets a FreeRTOS task of its own (priority clamped to
  `configMAX_PRIORITIES - 1`) the next time the engine steps the machine,
  whether it was spawned before or after the firmware booted.  FreeRTOS
  schedules it like the firmware's tasks: `TaskContext::sleep_until()`
  blocks on the delayed list and `yield_now()` behaves like `taskYIELD()`
  (pended inside a critical section).  Adopting it switches to it only if
  it outranks the running task, as `xTaskCreate()` would.  A wait the
  task began before it was adopted (`sleep_until()`/`sleep_for()`,
  `sim_task_delay_until()`, `sim_host_block_on_fd()`) is not cut short:
  every wait primitive rechecks its condition after each resume and, if
  unmet, waits again through the task's current scheduler.  The same holds
  for any resume, e.g. the firmware suspending and resuming the task's TCB:
  only the wait's own condition ends it.  Descriptor readiness is latched
  for the waiting task until it consumes it.  A panic in it
  is isolated like a faulted task.  If the firmware boots after the
  machine already ran native tasks, the engine starts FreeRTOS then; the
  kernel's tick count starts at the current virtual time, whether the
  engine or the firmware (`vTaskStartScheduler()`) starts the scheduler.
- **Readying between steps.** Every path that makes a task ready goes
  through `prvAddTaskToReadyList()`, whose trace hook tells the engine: a
  task readied between scheduling steps (a resume, notification,
  semaphore give, ... from host code after the scheduler ran) makes the
  machine run again, unless the scheduler has ended.
- **No C code under an engine borrow.**  The engine never calls into C
  (the kernel, the port, a hook, an ISR, a callback, a task) while it holds
  any of its own state borrowed (the task table, the CPU budget, ...): it
  reads what it needs from C first.  C can call back into the engine, and
  under edge instrumentation any C function can suspend its fiber for a
  budget tick, leaving the borrow held while the scheduler runs.  Debug
  builds check this: `sim_budget_poll()` checks every piece of engine
  state, and with `SIM_INSTRUMENT_EDGES=1` the edge hook checks the task
  table at every edge (`SIM_EDGE_BORROW_CHECK=0` turns that off), failing
  with the name of the borrowed state.
- **Thread exit.**  The standalone (thread-local) task table leaks the
  tasks still in it when its thread exits, instead of dropping them: a
  task that never ran still owns its captures, and their destructors may
  call back into the simulator while its thread-local state is being
  destroyed.  A simulator call made then finds the state gone and fails
  (`spawn_rust_task()` and `sim_create_task()` return 0, before any C
  call) rather than aborting; instrumented C running then polls no budget
  and checks no borrow, and a trace event is dropped.  A `Simulator`'s own table drops its tasks normally.
- **Configuration.** `configUSE_PREEMPTION` is 1 and `configASSERT()` is
  enabled: a failed kernel assertion records a `PortFatal` trace event and
  stops the task.
- **One scheduler.** `sim_start_scheduler()` (which standalone
  `vTaskStartScheduler()` reaches) runs `sim_scheduler_tick()` to
  completion: both start FreeRTOS the same way and share the machine's
  virtual clock and scheduler state.  `sim_zephyr_scheduler_tick()` on a
  machine that runs FreeRTOS also defers to it.
- **End of simulation.** Standalone firmware ends when nothing can happen
  any more, or when a task calls `vTaskEndScheduler()`.  After that, later
  steps (a World may keep stepping the machine) report completion; the
  kernel is not restarted.

FreeRTOS owns: task priorities, ready lists, delayed lists, queues,
semaphores, mutexes, event groups, task notifications, software timers,
and every scheduling policy (preemptive, cooperative, round-robin).

### Inside a World

A World steps each machine with a tick limit derived from World time
(`Machine::begin_firmware_step`).  The kernel is brought up to that
limit right away (charging a budget tick a busy task owes), so host code in `Firmware::step` that acts on the
firmware before running the scheduler (resuming a task, giving a
semaphore) acts at the step's World time.  One `sim_scheduler_tick()` call runs every
task due up to that tick, never moves firmware time past it, and reports the
next wake-up tick, which the machine converts back to World time.  Firmware
time therefore follows the World clock exactly; the conversion uses
`configTICK_RATE_HZ`.

Firmware trace events are recorded in FreeRTOS ticks.  World trace output
(`Machine::drain_trace_prefixed`, `World::drain_all_traces`) converts them to
World microseconds with the same mapping, so every line of a World trace, and
every scenario `before_ms` deadline, uses one time domain.

Each machine has its own copy of the kernel's state: the task lists and
tick count are swapped on activation, and the idle task, timer task and
timer command queue are allocated from the machine's own kernel heap.  Its
interrupt state (critical-section depth, `portDISABLE_INTERRUPTS()`, a
pended yield, a running ISR) is its own too, and a task that faults with interrupts
masked leaves them unmasked for the rest of the machine.

The FreeRTOS kernel keeps its state in C statics, so only one machine's
kernel can be active at a time.  A process-wide lock
(`FREERTOS_KERNEL_LOCK`) serialises FreeRTOS execution across threads, e.g.
across gRPC sessions: sessions stay isolated, but FreeRTOS work does not
scale across cores.

The last FreeRTOS thread-local storage slot (`SIM_TLS_HANDLE_INDEX`) holds
the simulator's fiber handle and is reserved; slot 0 is the application's.
Writing the reserved slot fails `configASSERT()` and the write is dropped.

### Zephyr

The unmodified Zephyr kernel (`sched.c`, `thread.c`, `timeout.c`, etc.)
runs inside Rust-managed fibers (one per thread since Phase 17). When
Zephyr switches threads via `arch_swap()` → `nct_swap_threads()`, the
current fiber yields and the drain loop resumes the next thread's fiber.
The `next_id` passed to `nct_swap_threads` is chosen by Zephyr's
scheduler, not by costar.

Zephyr owns: thread priorities, ready queue, timeout queue, scheduler
lock, timeslicing, and every scheduling policy (cooperative, preemptive,
time-sliced, meta-IRQ).

## What costar owns

| Component | Owner | Notes |
|-----------|-------|-------|
| Thread selection | **RTOS kernel** | costar never picks which thread runs |
| Fiber lifecycle | costar | Creates/destroys corosensei fibers per thread |
| Virtual time | costar | Advances `nsi_simu_time` to next deadline |
| Event queue | costar | Peripheral callbacks dispatched at virtual-time deadlines |
| IRQ controller | costar | Tracks pending IRQs; runs the ISR registered with `sim_irq_set_handler()` when interrupts are unmasked |
| Virtual devices | costar | UART, timer, GPIO — RTOS-agnostic |
| Trace sink | costar | Deterministic event recording |

## Interrupts

Firmware registers an ISR per IRQ line with `sim_irq_set_handler(irq, isr)`.
An IRQ raised by a device (a virtual timer expiring, a GPIO edge) or by
`sim_irq_raise()` is delivered as on hardware:

- with interrupts unmasked it is taken immediately, even in the middle of a
  task (the ISR runs on that task's fiber, as a real ISR runs on the
  interrupted stack);
- inside a critical section or with `portDISABLE_INTERRUPTS()` it stays
  pending and is taken when interrupts are unmasked.  Masking is checked
  before every interrupt: an ISR that calls `portDISABLE_INTERRUPTS()`
  holds off the IRQs still pending, and a context switch it requested,
  until interrupts are unmasked.  That holds in scheduler context too:
  while an ISR left interrupts masked the engine switches tasks neither
  after taking interrupts or input nor for the yield of the task the ISR
  interrupted; any switch it holds back (an ISR's, a tick's, the task's
  own) is latched and happens as soon as interrupts are unmasked;
- ISRs do not nest, and are taken lowest IRQ number first;
- every scheduler (FreeRTOS, native, Zephyr step and loop) takes IRQ
  input that has arrived before it selects or resumes a task, so a task
  never continues on device state an ISR has yet to update;
- a task whose fiber stops for good — it exits (even from inside an ISR
  it was running), finishes or faults — releases the interrupt state it
  held (an ISR in progress, a critical section, a mask), once, on every
  backend: later IRQs are still delivered.  A mask an ISR sets after
  that holds, and since the retired task must be switched away from at
  once, the task FreeRTOS selects instead waits for the unmask (the
  machine idles masked);
- `sim_irq_raise()` and `IrqController::raise()` mean "arrived now, at the
  current firmware time", for firmware and in-firmware device code.  Input
  from outside the firmware between World steps (a World, a host test)
  carries its arrival time: `Machine::raise_irq(irq, world_at)` converts
  the World time to a firmware tick and calls `IrqController::raise_at()`.
  Firmware time is tick-granular, so input between two ticks arrives at
  the later one: it is never taken before `world_at`, and the machine is
  woken at that tick.  Input raised before the machine's first firmware
  step is converted once that step fixes the World-to-firmware clock, and
  input staged or a virtual timer armed after the firmware's scheduler
  ran in a step (say, by `Firmware::step` itself) still wakes the machine.
  This holds for every backend: the native scheduler (and the Zephyr
  scheduler, step or loop, with or without threads) treats a scheduled
  arrival as a deadline like a peripheral callback, and the World wakes a
  native machine for it.  A native machine handles one deadline (sleeper,
  callback, IRQ input) at a time and never one past a World step's limit;
  while idle its clock keeps up with the World, so every ISR reads its
  arrival tick.  So does a native machine that never goes idle (a busy
  task, or one yielding until its ISR sets a flag, also behind the Zephyr
  scheduler step): at the start of every World step, before any of its
  tasks resumes, its clock is brought up to the step's limit, handling
  every deadline due by then at its own tick, in order (callbacks and
  ISRs run there; tasks they wake run at the limit).  The World steps such
  a machine at each World event and otherwise once per firmware tick,
  never busily within one: a task that is ready and has not run yet (new,
  or just woken) is stepped at once, a task that yielded and is still
  runnable at the next tick.  Without a World (standalone, unbounded) the
  native scheduler is cooperative: a task that only ever yields keeps
  time where it is, and input scheduled for later waits until every task
  blocks.  Every firmware deadline a World wakes a machine for, on
  any backend, is converted to World time through the machine's one
  firmware clock anchor (fixed at its first firmware step), and the Zephyr
  scheduler step keeps its time in the machine like the others.
  The World's wake-up for a FreeRTOS machine comes from one function
  (`freertos::pending_work_tick`) covering every source of pending work:
  the last step's deadlines, scheduled IRQs, armed timers, and — at once —
  an IRQ that can be taken, an expired timer, a due peripheral callback
  (`sim_schedule_event`, kept per machine), an ISR's pending yield or a
  task readied since the step.  Masked work (including a readied task,
  whose switch the mask holds off) does not wake the machine, a
  step within a tick whose budget is owed runs
  nothing, so no source wakes the machine before the next tick, and
  after `vTaskEndScheduler()` firmware never wakes it again.
  The machine first handles whatever was due before then, and the ISR and
  the tasks it wakes run at that instant, not at the machine's last
  firmware time, even if interrupts are masked when the step starts and
  unmasked before firmware time gets there.  The same line raised earlier
  (say, by a timer) is still taken at once, and the input still arrives at
  its instant;
- `sim_irq_clear()` acknowledges an interrupt that has arrived, even one
  not yet taken because interrupts are masked, and `sim_irq_pending()`
  reports only those: input staged for later in the step is neither
  cancelled nor visible early;
- an instrumentation budget exhausted inside an ISR does not switch tasks
  mid-ISR: the tick interrupt it stands for is taken when the ISR returns.
- an interrupt storm stops the machine: firmware that keeps one instant
  busy without end (an ISR re-arming its timer with zero delay or
  re-raising its own IRQ, a peripheral callback rescheduling itself for
  now) would never let time move on a real CPU.  Once one tick has taken
  more than the machine's storm limit — 1024 ISRs in one delivery, or 1024
  peripheral callbacks and deadlines coming due again at one tick — the
  engine records one `irq_storm` trace event and one `PortFatal` fault and
  stops that machine, like other fatal port errors: it is never woken or
  run again (every later step reports completion), while a World keeps
  running its other machines.  No guest code runs after the stop: a task
  whose IRQ (or unmask) started the storm never returns from that call —
  its fiber is suspended for good — and no scheduler (native, FreeRTOS,
  Zephyr) resumes a task, takes an IRQ, fires a timer or runs a callback
  on a stopped machine.  A World computes no firmware wake for it,
  whatever its backend.  A peripheral callback in flight when its IRQ
  storms the machine is host-side device code, not a task: it runs to its
  end, but nothing it requests takes effect — once the machine has
  stopped, raising an IRQ, arming a timer, scheduling a callback, tracing
  and sending on a device are no-ops, and the dispatcher runs no further
  callback.  Work held off by the interrupt mask is no storm: an ISR that
  masks interrupts, even on the last delivery the limit allows, leaves the
  rest pending for the unmask.  Firmware that legitimately takes more
  work at one instant raises the limit with `Simulator::set_storm_limit`.

An ISR may use `...FromISR()` APIs and `portYIELD_FROM_ISR()`; a task it
wakes preempts the interrupted task as soon as the ISR returns.  That
includes an ISR taken in scheduler context at the start of a step: FreeRTOS
selects the woken task before the task left running by the previous step
resumes.

No task switch ever happens in the middle of an ISR, on any scheduler
(native, Zephyr step and loop, FreeRTOS): a yield an ISR asks for —
`portYIELD_FROM_ISR()`, `sim_port_yield()`, or a native
`TaskContext::yield_now()` — is latched and performed when the ISR returns.
An ISR cannot wait: a sleep (`TaskContext::sleep_*`), `sim_task_delay_until()`
or a host I/O wait called from one is firmware misuse, handled like a failed
`configASSERT()` (a `PortFatal` fault that stops the interrupted task, whose
retirement releases the ISR; in scheduler context, a diagnostic and an abort).

Armed virtual timers are scheduling deadlines, so a system blocked waiting
for a timer interrupt advances straight to the timer's expiry.

## Preemption caveat

Task code runs in zero virtual time.  FreeRTOS preempts at every kernel call
(e.g. `xQueueSend()` waking a higher-priority task switches immediately) and
at tick interrupts, which occur when the CPU is idle or a task's
instrumentation budget runs out.  Without instrumentation, a task that
never calls the kernel cannot be preempted.  Preemption-dependent races
(e.g., "must preempt within N cycles of interrupt") won't reproduce without
compiler instrumentation (Tier 3 edge hooks).

The virtual CPU also charges time for zero-time work: after 10,000 task
slices at one tick (`SLICES_PER_TICK` in `sim-ffi/src/freertos.rs`) the
engine advances one tick, as if a tick interrupt fired.  This keeps tasks
that yield to each other forever from freezing virtual time.  It is part of
costar's CPU model, not FreeRTOS behaviour: on hardware, those slices would
take real CPU time instead.

Non-preemption-dependent races — priority ordering, timeout expiry,
deadlock, queue ordering — use genuine RTOS scheduler logic and reproduce
accurately.

## Peripheral event flow

```
C app calls sim_schedule_event(at, callback)
  → EVENT_QUEUE[at].push(callback)

Drain loop (when only the idle task can run):
  deadline = min(RTOS next wake, EVENT_QUEUE next key)
  advance virtual time to deadline (RTOS tick interrupts run on the way)
  if event deadline: dispatch callback → may call sim_irq_raise()
  deliver_pending_irqs()
  let the RTOS pick the next thread, resume its fiber
```

The event queue is thread-local in `sim-ffi`, accessible from any RTOS
context via the `sim_schedule_event()` C ABI. It works identically for
FreeRTOS and Zephyr — peripherals don't know which RTOS is running.
