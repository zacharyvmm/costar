#include "costar/cpu.hpp"

#include <algorithm>
#include <bit>
#include <cstdlib>
#include <utility>

#include <boost/context/fiber.hpp>
#include <boost/context/protected_fixedsize_stack.hpp>

namespace ctx = boost::context;

// One RTOS thread.  `context` is where the thread resumes; `trampoline` is
// where it returns to (Cpu::run) while it is running.
struct costar_thread_s {
  ctx::fiber context;
  ctx::fiber trampoline;
};

namespace costar {

namespace {
// RTOS stacks are sized for the target; host code (libc, tracing) needs more.
constexpr size_t kMinHostStack = 64 * 1024;
}  // namespace

Cpu::Cpu(const Time& now) : now_(now) { timers_.fill(kNever); }

Cpu::~Cpu() = default;  // Destroying a suspended fiber unwinds its stack.

void Cpu::boot(void (*entry)(void*), void* arg) {
  running_ = thread_new(entry, arg, 0);
  state_ = State::Awake;
}

Cpu::State Cpu::run() {
  while (state_ == State::Awake) {
    running_->context = std::move(running_->context).resume();
    switch (request_.kind) {
      case Request::Switch: running_ = request_.next; break;
      case Request::Halt: state_ = State::Halted; break;
      case Request::Poll:
        state_ = State::Busy;
        busy_until_ = now_ + request_.cost;
        break;
      case Request::Exit: state_ = State::Exited; break;
    }
  }
  return state_;
}

void Cpu::advance_to(Time now) {
  for (unsigned irq = 0; irq < COSTAR_IRQ_LINES; ++irq) {
    if (timers_[irq] <= now) {
      timers_[irq] = kNever;
      pending_irqs_ |= uint64_t{1} << irq;
    }
  }
  const bool interrupted = state_ == State::Halted && pending_irqs_ != 0;
  const bool done_busy = state_ == State::Busy && busy_until_ <= now;
  if (interrupted || done_busy) state_ = State::Awake;
}

Time Cpu::next_event() const {
  switch (state_) {
    case State::Awake: return now_;
    case State::Exited: return kNever;
    case State::Busy: return std::min(busy_until_, *std::ranges::min_element(timers_));
    case State::Halted: return *std::ranges::min_element(timers_);
  }
  return kNever;
}

// ── Called by firmware ──────────────────────────────────────────────────

void Cpu::init(costar_isr_fn isr, void (*switch_context)()) {
  isr_ = isr;
  switch_context_ = switch_context;
}

void Cpu::halt() {
  if (pending_irqs_ == 0) yield({Request::Halt});
  deliver_interrupts();
}

void Cpu::poll(Time cost) {
  yield({Request::Poll, nullptr, cost});
  deliver_interrupts();
}

unsigned Cpu::irq_lock() {
  const unsigned key = irqs_enabled_ ? COSTAR_IRQS_ENABLED : 0;
  irqs_enabled_ = false;
  return key;
}

void Cpu::irq_unlock(unsigned key) {
  irqs_enabled_ = key == COSTAR_IRQS_ENABLED;
  deliver_interrupts();
}

void Cpu::timer_set(unsigned irq, Time at) {
  if (irq >= COSTAR_IRQ_LINES) std::abort();
  timers_[irq] = at;
}

costar_thread Cpu::thread_new(void (*entry)(void*), void* arg, size_t stack_hint) {
  auto thread = std::make_unique<costar_thread_s>();
  costar_thread self = thread.get();
  thread->context = ctx::fiber(
      std::allocator_arg, ctx::protected_fixedsize_stack(std::max(stack_hint, kMinHostStack)),
      [this, self, entry, arg](ctx::fiber&& trampoline) {
        self->trampoline = std::move(trampoline);
        entry(arg);
        request_ = {Request::Exit};  // RTOS threads never return; the boot thread may.
        return std::move(self->trampoline);
      });
  threads_.push_back(std::move(thread));
  return self;
}

void Cpu::thread_switch(costar_thread next) {
  if (next != running_) yield({Request::Switch, next});
}

void Cpu::thread_free(costar_thread thread) {
  if (thread == running_) std::abort();
  auto it = std::ranges::find_if(threads_, [thread](const auto& t) { return t.get() == thread; });
  if (it == threads_.end()) std::abort();
  std::unique_ptr<costar_thread_s> doomed = std::move(*it);
  threads_.erase(it);
}  // `doomed` unwinds the thread's stack here.

// ── Internals ───────────────────────────────────────────────────────────

// Hand control from the running thread back to run().
void Cpu::yield(Request request) {
  request_ = request;
  costar_thread self = running_;
  self->trampoline = std::move(self->trampoline).resume();
}

// An interrupt point: run the ISR for every pending IRQ, then let the port
// switch threads if any ISR asked for it.
void Cpu::deliver_interrupts() {
  if (!irqs_enabled_ || in_isr_ || pending_irqs_ == 0 || isr_ == nullptr) return;

  in_isr_ = true;
  irqs_enabled_ = false;
  bool switch_needed = false;
  while (pending_irqs_ != 0) {
    const unsigned irq = std::countr_zero(pending_irqs_);
    pending_irqs_ &= pending_irqs_ - 1;
    switch_needed |= isr_(irq) != 0;
  }
  irqs_enabled_ = true;
  in_isr_ = false;

  if (switch_needed) switch_context_();
}

}  // namespace costar
