#pragma once

#include <array>
#include <memory>
#include <optional>
#include <string>
#include <type_traits>
#include <utility>
#include <vector>

#include <boost/context/detail/fcontext.hpp>

#include "costar.h"
#include "costar/time.hpp"

namespace costar {

// How much of its host stack a thread may use, given the size its RTOS gave
// it: `declared × allowance + extra` bytes.  Host frames are bigger than a
// microcontroller's, and ISRs and libc run on the thread's stack, hence the
// allowance (ARCHITECTURE.md §2).
struct StackCheck {
  bool enabled = true;
  double allowance = 2.0;
  size_t extra = 8 * 1024;

  bool operator==(const StackCheck&) const = default;
};

// One thread's use of its host stack.
struct ThreadStack {
  std::string name;  // the RTOS's name for it, else "thread <n>" (n: its place in creation order since boot)
  size_t declared;   // bytes its RTOS gave it (0: unknown)
  size_t limit;      // bytes the check allows (0: not checked)
  size_t used;       // the most it has used so far (high-water mark)

  bool operator==(const ThreadStack&) const = default;
};

// A virtual CPU: threads (fibers), interrupt lines, and one-shot timers.
//
// It has no scheduler.  run() resumes the thread the RTOS last switched to and
// follows its switch requests until the CPU halts, polls, or exits.  Firmware
// reaches the other public methods through costar.h (see abi.cpp); they are
// always called from inside a running thread.
class Cpu {
 public:
  enum class State { Awake, Halted, Busy, Exited };

  explicit Cpu(const Time& now);
  ~Cpu();
  Cpu(const Cpu&) = delete;
  Cpu& operator=(const Cpu&) = delete;

  // Create the reset thread and power on.  Until then the CPU is off (Exited).
  void boot(void (*entry)(void*), void* arg);
  // Power off: discard every thread, pending IRQ and timer.  Not from
  // firmware: the running thread would lose its stack.
  void power_off();

  // Run firmware until the CPU stops being awake.
  State run();

  // Raise the IRQs whose timers are due and end a finished busy period.
  void advance_to(Time now);
  // A device raises an interrupt line; wakes a halted CPU.
  void raise_irq(unsigned irq);

  State state() const { return state_; }
  // True while firmware runs (inside run()), not in an image's constructors
  // or destructors on the host thread.
  bool running() const { return in_run_; }
  // When this CPU next needs the world's attention (kNever if never).
  Time next_event() const;

  // ── Thread stacks ───────────────────────────────────────────────────
  // Every thread's host stack is painted when it is created.  Each time a
  // thread returns to run() (to switch, halt, poll or stop, or for
  // on_host_stack()), a painted canary just past its limit is checked; when
  // the canary is gone, the CPU stops.
  // For threads created or described from now on.
  void set_stack_check(const StackCheck& check) { stack_check_ = check; }
  // Every live thread, in creation order, with its high-water mark.
  std::vector<ThreadStack> stacks() const;
  // The thread whose overrun stopped the CPU (once).
  std::optional<ThreadStack> take_overrun() { return std::exchange(overrun_, std::nullopt); }
  // Live threads that have used more than their limit, by a full scan, and
  // not reported before: threads that jumped over their canary.
  std::vector<ThreadStack> scan_overruns();

  // ── Called by firmware ──────────────────────────────────────────────
  // Run `work` on the host stack rather than the running thread's, as a
  // system call switches to a kernel stack: costar's own work for firmware
  // (tracing, logging, ...) then neither counts against the thread's stack
  // nor depends on how the trace is consumed.  `work` must not touch the
  // CPU.  On the host thread (an image's constructors), it just runs.
  template <class F>
  void on_host_stack(F&& work) {
    if (!in_run_) {
      work();
      return;
    }
    yield({.kind = Request::Call, .call = [](void* f) { (*static_cast<std::remove_reference_t<F>*>(f))(); },
           .work = &work});
    if (pending_break_) take_break();
  }
  // For a debugger (costar run --break-on): stop in it with debug_break(why)
  // inside the firmware call whose host work is running now (see
  // on_host_stack()), once that work is done and the thread is back on its
  // own stack, so the debugger shows the firmware's frames.  False, doing
  // nothing, if no call's host work is running.
  bool break_in_call(std::string why);
  void init(costar_isr_fn isr, void (*switch_context)());
  void halt();
  void poll(Time cost);
  [[noreturn]] void stop();
  unsigned irq_lock();
  void irq_unlock(unsigned key);
  void timer_set(unsigned irq, Time at);
  costar_thread thread_new(void (*entry)(void*), void* arg, size_t stack_hint);
  void thread_switch(costar_thread next);
  void thread_free(costar_thread thread);
  void thread_describe(costar_thread thread, const char* name, size_t stack_size);
  static void thread_stack(costar_thread thread, void** base, size_t* size);

 private:
  // What the running thread asks the trampoline in run() to do.
  struct Request {
    enum Kind { Switch, Halt, Poll, Exit, Call } kind;
    costar_thread next = nullptr;
    Time cost = 0;
    void (*call)(void*) = nullptr;  // Call: call(work) on the host stack, then resume
    void* work = nullptr;
  };

  costar_thread create_thread(void (*entry)(void*), void* arg, size_t stack_hint);
  void take_break();
  static void thread_main(boost::context::detail::transfer_t from_run);
  void yield(Request request);
  // Before a started thread's stack is discarded, ASan must see it switched
  // away from for good, or its fake stack leaks.  Without ASan, nothing.
  void retire(costar_thread thread);
  void retire_all();
  [[noreturn, gnu::noinline]] void retired(boost::context::detail::fcontext_t to);
  void deliver_interrupts();
  // The bytes `thread` may use for a declared stack of `declared` bytes
  // (0: unchecked).
  size_t limit_for(const costar_thread_s* thread, size_t declared) const;
  // After `thread` returned to run(): note an overrun if its canary is gone.
  void check_canary(costar_thread thread);
  static ThreadStack stack_of(const costar_thread_s& thread);

  const Time& now_;
  State state_ = State::Exited;
  bool in_run_ = false;
  Time busy_until_ = 0;

  std::vector<std::unique_ptr<costar_thread_s>> threads_;
  unsigned threads_created_ = 0;  // since boot
  StackCheck stack_check_;
  std::optional<ThreadStack> overrun_;
  bool in_call_ = false;                      // running a Request::Call's work
  std::optional<std::string> pending_break_;  // break_in_call()
  costar_thread running_ = nullptr;
  boost::context::detail::fcontext_t trampoline_ = nullptr;  // where the running thread returns to
  const void* host_stack_ = nullptr;  // the stack run() runs on, for AddressSanitizer
  size_t host_stack_size_ = 0;
  boost::context::detail::fcontext_t retirer_ = nullptr;  // where a retiring thread returns to
  const void* retirer_stack_ = nullptr;
  size_t retirer_stack_size_ = 0;
  Request request_{Request::Exit};

  uint64_t pending_irqs_ = 0;
  bool irqs_enabled_ = true;
  bool in_isr_ = false;
  std::array<Time, COSTAR_IRQ_LINES> timers_;
  costar_isr_fn isr_ = nullptr;
  void (*switch_context_)() = nullptr;
};

}  // namespace costar
