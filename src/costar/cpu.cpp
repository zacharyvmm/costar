#include "costar/cpu.hpp"

#include <algorithm>
#include <bit>
#include <cerrno>
#include <cmath>
#include <cstdlib>
#include <utility>

#include <boost/context/protected_fixedsize_stack.hpp>

#if defined(__SANITIZE_ADDRESS__)
#define COSTAR_ASAN 1
#elif defined(__has_feature)
#if __has_feature(address_sanitizer)
#define COSTAR_ASAN 1
#endif
#endif
#ifdef COSTAR_ASAN
#include <pthread.h>
#include <sanitizer/asan_interface.h>
#include <sanitizer/common_interface_defs.h>
#endif

namespace ctx = boost::context;

namespace {

// AddressSanitizer must be told about every switch between stacks: start
// before jumping (saving the leaving stack's fake frames, or nullptr when the
// leaving thread is done for good), finish after arriving.  Without ASan
// these do nothing.
void start_switch([[maybe_unused]] void** fake_stack, [[maybe_unused]] const void* bottom,
                  [[maybe_unused]] size_t size) {
#ifdef COSTAR_ASAN
  __sanitizer_start_switch_fiber(fake_stack, bottom, size);
#endif
}

void finish_switch([[maybe_unused]] void* fake_stack, [[maybe_unused]] const void** from_bottom,
                   [[maybe_unused]] size_t* from_size) {
#ifdef COSTAR_ASAN
  __sanitizer_finish_switch_fiber(fake_stack, from_bottom, from_size);
#endif
}

// A discarded stack's memory may be mapped again for another: forget what
// ASan knew about it.
void forget_stack([[maybe_unused]] const ctx::stack_context& stack) {
#ifdef COSTAR_ASAN
  ASAN_UNPOISON_MEMORY_REGION(static_cast<char*>(stack.sp) - stack.size, stack.size);
#endif
}

// Thread stacks are painted with this, so that what a thread has used shows.
// The functions below read and write other threads' stacks, where ASan may
// have poisoned live frames' redzones: they are not instrumented, and use
// volatile words so that no loop becomes a (checked) memset call.
constexpr uint64_t kPaint = 0x6b617473'6b617473;  // "stakstak"

[[gnu::no_sanitize("address")]] void paint(char* from, char* to) {
  for (auto* word = reinterpret_cast<volatile uint64_t*>(from); word < reinterpret_cast<volatile uint64_t*>(to); ++word) {
    *word = kPaint;
  }
}

[[gnu::no_sanitize("address")]] bool painted(const char* from, const char* to) {
  for (auto* word = reinterpret_cast<const volatile uint64_t*>(from);
       word < reinterpret_cast<const volatile uint64_t*>(to); ++word) {
    if (*word != kPaint) return false;
  }
  return true;
}

// The lowest bytes of a thread stack belong to its RTOS, not to its frames.
// An RTOS told about the host stack (costar_thread_stack()) may keep markers
// at its base: Zephyr's CONFIG_STACK_SENTINEL writes a word there under
// CONFIG_ARCH_POSIX_UPDATE_STACK_INFO.  They are not stack use.
constexpr size_t kRtosMarkers = 64;

// Bytes in use below `top`: up to the deepest word that is no longer paint.
[[gnu::no_sanitize("address")]] size_t high_water(const char* base, const char* top) {
  auto* word = reinterpret_cast<const volatile uint64_t*>(base);
  while (word < reinterpret_cast<const volatile uint64_t*>(top) && *word == kPaint) ++word;
  return static_cast<size_t>(top - reinterpret_cast<const char*>(const_cast<const uint64_t*>(word)));
}

}  // namespace

// One RTOS thread: a guard-page-protected stack and the point it resumes at.
// Freeing a thread discards its stack without unwinding it, as deleting a
// thread does on real hardware.
struct costar_thread_s {
  costar::Cpu* cpu;
  void (*entry)(void*);
  void* arg;
  ctx::stack_context stack;
  ctx::detail::fcontext_t context;
  void* fake_stack = nullptr;  // ASan's, while the thread is switched out
  bool started = false;        // it has run, so ASan knows its stack
  bool finished = false;       // its entry returned: switched away for good
  unsigned index = 0;          // creation order since boot
  std::string name = {};       // the RTOS's, if it said
  size_t declared = 0;         // stack bytes its RTOS gave it (0: unknown)
  size_t limit = 0;            // bytes it may use (0: unchecked); its canary lies just past them
  bool overran = false;        // reported

  const void* bottom() const { return static_cast<char*>(stack.sp) - stack.size; }
  char* top() const { return static_cast<char*>(stack.sp); }
  // The lowest usable byte: the lowest page is the guard.
  char* base() const { return top() - stack.size + ctx::stack_traits::page_size(); }
  size_t used() const { return high_water(base() + kRtosMarkers, top()); }
  ~costar_thread_s() {
    forget_stack(stack);
    ctx::protected_fixedsize_stack{}.deallocate(stack);
  }
};

namespace costar {

namespace {
// RTOS stacks are sized for the target; host code (libc, tracing) needs more.
constexpr size_t kMinHostStack = 64 * 1024;
// The painted words just past a thread's limit that must stay painted.
constexpr size_t kCanary = 512;
// Passed to a suspended thread to make it switch away for good (retire()).
int kRetire;
}  // namespace

Cpu::Cpu(const Time& now) : now_(now) { timers_.fill(kNever); }

Cpu::~Cpu() { retire_all(); }

void Cpu::boot(void (*entry)(void*), void* arg) {
  running_ = create_thread(entry, arg, 0);
  state_ = State::Awake;
}

void Cpu::power_off() {
  if (in_run_) std::abort();
  retire_all();
  threads_.clear();
  threads_created_ = 0;
  overrun_.reset();
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
    void* fake_stack = nullptr;
    start_switch(&fake_stack, running_->bottom(), running_->stack.size);
    running_->context = ctx::detail::jump_fcontext(running_->context, running_).fctx;
    finish_switch(fake_stack, nullptr, nullptr);
    if (request_.kind == Request::Call) request_.call(request_.work);
    check_canary(running_);
    if (overrun_) {
      state_ = State::Exited;  // as costar_cpu_stop()
      break;
    }
    switch (request_.kind) {
      case Request::Call: break;  // resume the thread
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
  if (!in_run_) return;  // on the host thread (an image's constructor or destructor)
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
  costar_thread created = nullptr;
  on_host_stack([&] { created = create_thread(entry, arg, stack_hint); });
  return created;
}

costar_thread Cpu::create_thread(void (*entry)(void*), void* arg, size_t stack_hint) {
  auto thread = std::make_unique<costar_thread_s>();
  thread->cpu = this;
  thread->entry = entry;
  thread->arg = arg;
  thread->index = threads_created_++;
  // Room for the limit, its canary, and an overrun until the thread next returns to run().
  const size_t limit = limit_for(nullptr, stack_hint);
  const size_t size = std::max({stack_hint, kMinHostStack, limit == 0 ? 0 : limit + kCanary + kMinHostStack});
  thread->stack = ctx::protected_fixedsize_stack(size).allocate();
  paint(thread->base(), thread->top());
  thread->declared = stack_hint;
  thread->limit = limit;
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
  // A thread that jumped over its canary shows when its stack is scanned.
  const bool overran = thread->limit != 0 && !thread->overran && thread->used() > thread->limit;
  if (overran) overrun_ = stack_of(*thread);
  retire(thread);
  threads_.erase(it);
  if (overran) stop();
}

void Cpu::thread_describe(costar_thread thread, const char* name, size_t stack_size) {
  if (name != nullptr && *name != '\0') thread->name = name;
  if (stack_size == 0) return;
  thread->declared = stack_size;
  thread->limit = limit_for(thread, stack_size);
}

std::vector<ThreadStack> Cpu::stacks() const {
  std::vector<ThreadStack> out;
  for (const auto& thread : threads_) out.push_back(stack_of(*thread));
  return out;
}

std::vector<ThreadStack> Cpu::scan_overruns() {
  std::vector<ThreadStack> out;
  for (const auto& thread : threads_) {
    if (thread->limit == 0 || thread->overran || thread->used() <= thread->limit) continue;
    thread->overran = true;
    out.push_back(stack_of(*thread));
  }
  return out;
}

// ── Internals ───────────────────────────────────────────────────────────

ThreadStack Cpu::stack_of(const costar_thread_s& thread) {
  return {.name = thread.name.empty() ? "thread " + std::to_string(thread.index) : thread.name,
          .declared = thread.declared,
          .limit = thread.limit,
          .used = thread.used()};
}

// declared × allowance + extra, in whole words; 0 when unchecked, or when
// that is more than `thread`'s host stack holds with its canary (then only
// the guard page catches an overrun).  For a thread not yet created
// (nullptr), the stack will be made big enough.
size_t Cpu::limit_for(const costar_thread_s* thread, size_t declared) const {
  if (!stack_check_.enabled || declared == 0) return 0;
  const double bytes = std::ceil(static_cast<double>(declared) * stack_check_.allowance) +
                       static_cast<double>(stack_check_.extra);
  if (!(bytes < 0x1p40)) return 0;  // nonsense, or bigger than any stack worth checking
  const size_t limit = (static_cast<size_t>(bytes) + 7) & ~size_t{7};
  if (thread != nullptr && kRtosMarkers + limit + kCanary > static_cast<size_t>(thread->top() - thread->base())) return 0;
  return limit;
}

void Cpu::check_canary(costar_thread thread) {
  if (thread->limit == 0 || thread->overran || overrun_) return;
  const char* canary = thread->top() - thread->limit;
  if (painted(canary - kCanary, canary)) return;
  thread->overran = true;
  overrun_ = stack_of(*thread);
}

void Cpu::thread_stack(costar_thread thread, void** base, size_t* size) {
  const size_t guard = ctx::stack_traits::page_size();  // the lowest page is the guard
  *base = static_cast<char*>(thread->stack.sp) - thread->stack.size + guard;
  *size = thread->stack.size - guard;
}

// First code to run on a new thread's stack.
void Cpu::thread_main(ctx::detail::transfer_t from_run) {
  auto* self = static_cast<costar_thread>(from_run.data);
  Cpu& cpu = *self->cpu;
  finish_switch(nullptr, &cpu.host_stack_, &cpu.host_stack_size_);
  self->started = true;
  cpu.trampoline_ = from_run.fctx;
  errno = 0;
  self->entry(self->arg);
  cpu.request_ = {Request::Exit};  // RTOS threads never return; the boot thread may.
  self->finished = true;
  start_switch(nullptr, cpu.host_stack_, cpu.host_stack_size_);
  ctx::detail::jump_fcontext(cpu.trampoline_, nullptr);
  std::abort();  // run() never resumes a finished thread.
}

// Hand control from the running thread back to run(); returns when run()
// resumes this thread.  errno belongs to the host thread, so each RTOS
// thread keeps its own across switches, as it would with real TLS.
void Cpu::yield(Request request) {
  const int saved_errno = errno;
  costar_thread self = running_;
  request_ = request;
  start_switch(&self->fake_stack, host_stack_, host_stack_size_);
  const ctx::detail::transfer_t back = ctx::detail::jump_fcontext(trampoline_, nullptr);
  const void* from_stack = nullptr;
  size_t from_size = 0;
  finish_switch(self->fake_stack, &from_stack, &from_size);
  if (back.data == &kRetire) {
    self->finished = true;
    retired(back.fctx);
  }
  trampoline_ = back.fctx;
  host_stack_ = from_stack;
  host_stack_size_ = from_size;
  errno = saved_errno;
}

// A retiring thread, never to run again: free its fake stack and go back to
// retire().  yield()'s locals may live in that fake stack, so this frame of
// its own reads none of them.
void Cpu::retired(ctx::detail::fcontext_t to) {
  retirer_ = to;
  start_switch(nullptr, retirer_stack_, retirer_stack_size_);
  ctx::detail::jump_fcontext(retirer_, nullptr);
  std::abort();
}

// From the host thread (power off, destruction) or from the running thread
// (freeing another): switch into `thread` once, and it switches straight
// back as if finished.
void Cpu::retire([[maybe_unused]] costar_thread thread) {
#ifdef COSTAR_ASAN
  if (!thread->started || thread->finished) return;
  if (in_run_) {
    retirer_stack_ = running_->bottom();
    retirer_stack_size_ = running_->stack.size;
  } else {  // whichever host thread this is
    pthread_attr_t attr;
    void* bottom = nullptr;
    if (pthread_getattr_np(pthread_self(), &attr) != 0) std::abort();
    pthread_attr_getstack(&attr, &bottom, &retirer_stack_size_);
    pthread_attr_destroy(&attr);
    retirer_stack_ = bottom;
  }
  void* fake_stack = nullptr;
  start_switch(&fake_stack, thread->bottom(), thread->stack.size);
  ctx::detail::jump_fcontext(thread->context, &kRetire);
  finish_switch(fake_stack, nullptr, nullptr);
#endif
}

void Cpu::retire_all() {
  for (const auto& thread : threads_) retire(thread.get());
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
