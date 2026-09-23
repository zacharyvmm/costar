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

Each machine has its own copy of the kernel's state: the task lists and
tick count are swapped on activation, and the idle task, timer task and
timer command queue are allocated from the machine's own kernel heap.  Its
interrupt state (critical-section depth, `portDISABLE_INTERRUPTS()`, a
pended yield) is its own too, and a task that faults with interrupts
masked leaves them unmasked for the rest of the machine.

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
