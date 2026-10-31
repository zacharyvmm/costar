// costar.h, as seen from the engine: each call names the machine that made it.
#include "costar/abi.hpp"

#include "costar/machine.hpp"

namespace costar {

namespace {

Machine& on(void* machine) {
  Machine& m = *static_cast<Machine*>(machine);
  m.count_call();
  return m;
}
Cpu& cpu(void* machine) { return on(machine).cpu(); }

const costar_host kHost = {
    .version = COSTAR_HOST_VERSION,
    .now = [](void* m) { return on(m).now(); },
    .cpu_init = [](void* m, costar_isr_fn isr, void (*switch_context)()) { cpu(m).init(isr, switch_context); },
    .cpu_halt = [](void* m) { cpu(m).halt(); },
    .cpu_poll = [](void* m, costar_time cost) { cpu(m).poll(cost); },
    .cpu_stop = [](void* m, int status) { on(m).stop(status); },
    .irq_lock = [](void* m) { return cpu(m).irq_lock(); },
    .irq_unlock = [](void* m, unsigned key) { cpu(m).irq_unlock(key); },
    .timer_set = [](void* m, unsigned irq, costar_time at) { cpu(m).timer_set(irq, at); },
    .thread_new = [](void* m, void (*entry)(void*), void* arg,
                     size_t stack_hint) { return cpu(m).thread_new(entry, arg, stack_hint); },
    .thread_switch = [](void* m, costar_thread next) { cpu(m).thread_switch(next); },
    .thread_free = [](void* m, costar_thread thread) { cpu(m).thread_free(thread); },
    .thread_stack = [](void*, costar_thread thread, void** base, size_t* size) { Cpu::thread_stack(thread, base, size); },
    .dev = [](void* m, const char* name) { return on(m).devices().find(name); },
    .dev_irq = [](void* m, int dev) { return on(m).dev_irq(dev); },
    .chan_send = [](void* m, int dev, const void* frame,
                    size_t length) { return on(m).chan_send(dev, {static_cast<const uint8_t*>(frame), length}); },
    .chan_recv = [](void* m, int dev, void* frame,
                    size_t capacity) { return on(m).chan_recv(dev, {static_cast<uint8_t*>(frame), capacity}); },
    .reg_read = [](void* m, int dev, uint32_t reg) { return on(m).reg_read(dev, reg); },
    .reg_write = [](void* m, int dev, uint32_t reg, uint32_t value) { on(m).reg_write(dev, reg, value); },
    .mem_size = [](void* m, int dev) { return on(m).mem_size(dev); },
    .mem_read = [](void* m, int dev, uint32_t offset, void* buffer,
                   size_t length) { return on(m).mem_read(dev, offset, {static_cast<uint8_t*>(buffer), length}); },
    .mem_write = [](void* m, int dev, uint32_t offset, const void* data,
                    size_t length) { return on(m).mem_write(dev, offset, {static_cast<const uint8_t*>(data), length}); },
    .mem_erase = [](void* m, int dev, uint32_t offset, size_t length) { return on(m).mem_erase(dev, offset, length); },
    .trace = [](void* m, const char* label, uint32_t value) { on(m).trace(label, value); },
    .log = [](void* m, const char* text, size_t length) { on(m).log({text, length}); },
};

}  // namespace

const costar_host& host_table() { return kHost; }

}  // namespace costar
