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
  matched on entry point and parameter, and only while the task is alive.
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
  stepping.
- **Host I/O and the delay ABI.** A task in `sim_host_block_on_fd()` is
  suspended in the kernel until the host poller reports its descriptor
  ready, and `sim_task_delay_until()` blocks it on FreeRTOS's delayed list.
  FreeRTOS keeps scheduling the machine's other tasks meanwhile.  Deleting
  a task that waits on a descriptor cancels the wait.
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
pended yield) is its own too, and a task that faults with interrupts
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
| IRQ controller | costar | Tracks pending IRQs, delivers when unlocked |
| Virtual devices | costar | UART, timer, GPIO — RTOS-agnostic |
| Trace sink | costar | Deterministic event recording |

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
