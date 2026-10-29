#include "costar/cpu.hpp"

#include <algorithm>
#include <bit>
#include <cerrno>
#include <cstdlib>
#include <utility>

#include <boost/context/protected_fixedsize_stack.hpp>

namespace ctx = boost::context;

// One RTOS thread: a guard-page-protected stack and the point it resumes at.
// Freeing a thread discards its stack without unwinding it, as deleting a
// thread does on real hardware.
struct costar_thread_s {
  costar::Cpu* cpu;
  void (*entry)(void*);
  void* arg;
  ctx::stack_context stack;
  ctx::detail::fcontext_t context;

  ~costar_thread_s() { ctx::protected_fixedsize_stack{}.deallocate(stack); }
};

namespace costar {

namespace {
// RTOS stacks are sized for the target; host code (libc, tracing) needs more.
constexpr size_t kMinHostStack = 64 * 1024;
}  // namespace

Cpu::Cpu(const Time& now) : now_(now) { timers_.fill(kNever); }

Cpu::~Cpu() = default;

void Cpu::boot(void (*entry)(void*), void* arg) {
  running_ = thread_new(entry, arg, 0);
  state_ = State::Awake;
}

void Cpu::power_off() {
  if (in_run_) std::abort();
  threads_.clear();
  running_ = nullptr;
  trampoline_ = nullptr;
  request_ = {Request::Exit};
  state_ = State::Exited;
  busy_until_ = 0;
  pending_irqs_ = 0;
  irqs_enabled_ = true;
  in_isr_ = false;
  timers_.fill(kNever);
  isr_ = nullptr;
  switch_context_ = nullptr;
}

Cpu::State Cpu::run() {
  in_run_ = true;
  while (state_ == State::Awake) {
    running_->context = ctx::detail::jump_fcontext(running_->context, running_).fctx;
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
  in_run_ = false;
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

void Cpu::raise_irq(unsigned irq) {
  if (irq >= COSTAR_IRQ_LINES) std::abort();
  pending_irqs_ |= uint64_t{1} << irq;
  if (state_ == State::Halted) state_ = State::Awake;
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

void Cpu::stop() {
  yield({Request::Exit});
  std::abort();  // run() never resumes a stopped CPU.
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
  thread->cpu = this;
  thread->entry = entry;
  thread->arg = arg;
  thread->stack = ctx::protected_fixedsize_stack(std::max(stack_hint, kMinHostStack)).allocate();
  thread->context = ctx::detail::make_fcontext(thread->stack.sp, thread->stack.size, &Cpu::thread_main);
  threads_.push_back(std::move(thread));
  return threads_.back().get();
}

void Cpu::thread_switch(costar_thread next) {
  if (next != running_) yield({Request::Switch, next});
}

void Cpu::thread_free(costar_thread thread) {
  if (thread == running_) std::abort();
  auto it = std::ranges::find_if(threads_, [thread](const auto& t) { return t.get() == thread; });
  if (it == threads_.end()) std::abort();
  threads_.erase(it);
}

// ── Internals ───────────────────────────────────────────────────────────

void Cpu::thread_stack(costar_thread thread, void** base, size_t* size) {
  const size_t guard = ctx::stack_traits::page_size();  // the lowest page is the guard
  *base = static_cast<char*>(thread->stack.sp) - thread->stack.size + guard;
  *size = thread->stack.size - guard;
}

// First code to run on a new thread's stack.
void Cpu::thread_main(ctx::detail::transfer_t from_run) {
  auto* self = static_cast<costar_thread>(from_run.data);
  Cpu& cpu = *self->cpu;
  cpu.trampoline_ = from_run.fctx;
  errno = 0;
  self->entry(self->arg);
  cpu.request_ = {Request::Exit};  // RTOS threads never return; the boot thread may.
  ctx::detail::jump_fcontext(cpu.trampoline_, nullptr);
  std::abort();  // run() never resumes a finished thread.
}

// Hand control from the running thread back to run(); returns when run()
// resumes this thread.  errno belongs to the host thread, so each RTOS
// thread keeps its own across switches, as it would with real TLS.
void Cpu::yield(Request request) {
  const int saved_errno = errno;
  request_ = request;
  trampoline_ = ctx::detail::jump_fcontext(trampoline_, nullptr).fctx;
  errno = saved_errno;
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
