#pragma once

#include <array>
#include <memory>
#include <vector>

#include <boost/context/detail/fcontext.hpp>

#include "costar.h"
#include "costar/time.hpp"

namespace costar {

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
  // When this CPU next needs the world's attention (kNever if never).
  Time next_event() const;

  // ── Called by firmware ──────────────────────────────────────────────
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
  static void thread_stack(costar_thread thread, void** base, size_t* size);

 private:
  // What the running thread asks the trampoline in run() to do.
  struct Request {
    enum Kind { Switch, Halt, Poll, Exit } kind;
    costar_thread next = nullptr;
    Time cost = 0;
  };

  static void thread_main(boost::context::detail::transfer_t from_run);
  void yield(Request request);
  void deliver_interrupts();

  const Time& now_;
  State state_ = State::Exited;
  bool in_run_ = false;
  Time busy_until_ = 0;

  std::vector<std::unique_ptr<costar_thread_s>> threads_;
  costar_thread running_ = nullptr;
  boost::context::detail::fcontext_t trampoline_ = nullptr;  // where the running thread returns to
  Request request_{Request::Exit};

  uint64_t pending_irqs_ = 0;
  bool irqs_enabled_ = true;
  bool in_isr_ = false;
  std::array<Time, COSTAR_IRQ_LINES> timers_;
  costar_isr_fn isr_ = nullptr;
  void (*switch_context_)() = nullptr;
};

}  // namespace costar
