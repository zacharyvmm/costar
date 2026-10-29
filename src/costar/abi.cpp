// costar.h, as seen from the engine: each call names the machine that made it.
#include "costar/abi.hpp"

#include "costar/machine.hpp"

namespace costar {

namespace {

Machine& on(void* machine) { return *static_cast<Machine*>(machine); }
Cpu& cpu(void* machine) { return on(machine).cpu(); }

const costar_host kHost = {
    .version = COSTAR_HOST_VERSION,
    .now = [](void* m) { return on(m).now(); },
    .cpu_init = [](void* m, costar_isr_fn isr, void (*switch_context)()) { cpu(m).init(isr, switch_context); },
    .cpu_halt = [](void* m) { cpu(m).halt(); },
    .cpu_poll = [](void* m, costar_time cost) { cpu(m).poll(cost); },
    .irq_lock = [](void* m) { return cpu(m).irq_lock(); },
    .irq_unlock = [](void* m, unsigned key) { cpu(m).irq_unlock(key); },
    .timer_set = [](void* m, unsigned irq, costar_time at) { cpu(m).timer_set(irq, at); },
    .thread_new = [](void* m, void (*entry)(void*), void* arg,
                     size_t stack_hint) { return cpu(m).thread_new(entry, arg, stack_hint); },
    .thread_switch = [](void* m, costar_thread next) { cpu(m).thread_switch(next); },
    .thread_free = [](void* m, costar_thread thread) { cpu(m).thread_free(thread); },
    .trace = [](void* m, const char* label, uint32_t value) { on(m).trace(label, value); },
};

}  // namespace

const costar_host& host_table() { return kHost; }

}  // namespace costar
