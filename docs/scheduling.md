# Scheduling Architecture

## Who owns scheduling?

**The RTOS kernel owns every scheduling decision.** costar is the fiber
substrate — it provides stackful coroutines and advances virtual time, but
never selects which thread runs next.

### FreeRTOS

The unmodified FreeRTOS kernel (`tasks.c`, `queue.c`, `list.c`, `timers.c`)
runs inside Rust-managed fibers. When a task calls `vTaskDelay()` or
`xQueueReceive()`, FreeRTOS manipulates its own ready/delayed lists, then
calls `portYIELD()` → `sim_port_yield()`. The Rust drain loop sees the
fiber yielded and resumes whichever fiber FreeRTOS marked current.

FreeRTOS owns: task priorities, ready lists, delayed lists, queues,
semaphores, mutexes, event groups, task notifications, software timers,
and every scheduling policy (preemptive, cooperative, round-robin).

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

On real hardware, an ISR return immediately preempts to the highest-priority
ready thread. In costar's cooperative model, the current thread runs until
its next yield, sleep, or blocking call. Preemption-dependent races (e.g.,
"must preempt within N cycles of interrupt") won't reproduce without
compiler instrumentation (Tier 3 edge hooks).

Non-preemption-dependent races — priority ordering, timeout expiry,
deadlock, queue ordering — use genuine RTOS scheduler logic and reproduce
accurately.

## Peripheral event flow

```
C app calls sim_schedule_event(at, callback)
  → EVENT_QUEUE[at].push(callback)

Drain loop:
  deadline = min(RTOS next wake, EVENT_QUEUE next key)
  advance virtual time to deadline
  if event deadline: dispatch callback → may call sim_irq_raise()
  if RTOS deadline:  sys_clock_announce() → wakes sleeping threads
  deliver_pending_irqs()
  resume RTOS-selected thread fiber
```

The event queue is thread-local in `sim-ffi`, accessible from any RTOS
context via the `sim_schedule_event()` C ABI. It works identically for
FreeRTOS and Zephyr — peripherals don't know which RTOS is running.
