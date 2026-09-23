# Scheduling Architecture

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
  still works and maps onto the same fiber.)
- **Switching.** `portYIELD()` suspends the running fiber; the engine then
  calls `vTaskSwitchContext()` — what PendSV does on a Cortex-M — and
  resumes the fiber of the task FreeRTOS placed in `pxCurrentTCB`.  A yield
  requested inside a critical section or from ISR context is pended until
  interrupts are unmasked.
- **Time.** Virtual time advances only when the idle task runs (every
  application task is blocked): the engine jumps to the next delayed-task
  wake-up or peripheral event and runs the tick interrupts in between.  A
  task that exhausts its instrumentation budget is charged one tick of CPU
  time, so a busy loop still lets time pass and higher-priority tasks
  preempt it.
- **Host I/O and the delay ABI.** A task in `sim_host_block_on_fd()` is
  suspended in the kernel until the host poller reports its descriptor
  ready, and `sim_task_delay_until()` blocks it on FreeRTOS's delayed list.
  FreeRTOS keeps scheduling the machine's other tasks meanwhile.  Deleting
  a task that waits on a descriptor cancels the wait.
- **Configuration.** `configUSE_PREEMPTION` is 1 and `configASSERT()` is
  enabled: a failed kernel assertion records a `PortFatal` trace event and
  stops the task.
- **End of simulation.** Standalone firmware ends when nothing can happen
  any more, or when a task calls `vTaskEndScheduler()`.

FreeRTOS owns: task priorities, ready lists, delayed lists, queues,
semaphores, mutexes, event groups, task notifications, software timers,
and every scheduling policy (preemptive, cooperative, round-robin).

### Inside a World

A World steps each machine with a tick limit derived from World time
(`Machine::begin_firmware_step`).  One `sim_scheduler_tick()` call runs every
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
  pending and is taken when interrupts are unmasked;
- ISRs do not nest, and are taken lowest IRQ number first;
- an IRQ staged from outside between two World steps (a device model, a
  test) arrives at the World's current instant, the step's limit: the
  machine first handles whatever was due before then, and the ISR and the
  tasks it wakes run at that instant, not at the machine's last firmware
  time.  The same line raised earlier in the step (say, by a timer) is
  still taken at once, and the staged input still arrives at its instant;
- `sim_irq_clear()` acknowledges an interrupt that has arrived, even one
  not yet taken because interrupts are masked, and `sim_irq_pending()`
  reports only those: input staged for later in the step is neither
  cancelled nor visible early;
- an instrumentation budget exhausted inside an ISR does not switch tasks
  mid-ISR: the tick interrupt it stands for is taken when the ISR returns.

An ISR may use `...FromISR()` APIs and `portYIELD_FROM_ISR()`; a task it
wakes preempts the interrupted task as soon as the ISR returns.  Armed
virtual timers are scheduling deadlines, so a system blocked waiting for a
timer interrupt advances straight to the timer's expiry.

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
