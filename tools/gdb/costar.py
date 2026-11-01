"""GDB commands for costar programs (docs/DEBUGGING.md).

    (gdb) source tools/gdb/costar.py       # installed: share/costar/gdb/costar.py
    (gdb) costar machines                  # every machine: virtual time, CPU state, threads
    (gdb) costar threads [MACHINE]         # a machine's RTOS threads, and where each one is
    (gdb) costar bt [MACHINE:]THREAD [N]   # the backtrace of a suspended thread

RTOS threads are costar fibers (Boost.Context), which GDB doesn't know
about: `info threads` and `bt` only see the host thread and the fiber
running on it.  These commands find machines through costar_debug_worlds
(src/costar/debug.hpp), the Worlds the program lists for debuggers (costar
run and test, costar-grpc's sessions), and show a suspended fiber's stack by
loading its saved registers for a moment.

A MACHINE is a name from the scenario, or m<id> as in traces; with more
than one World (costar-grpc sessions), prefix w<k>. as listed by
`costar machines`.  It defaults to the machine whose code is running.
"""
import re
import struct

import gdb

# ── Reading the engine's data ─────────────────────────────────────────
# src/costar/debug.cpp is always built with debug information, so GDB knows
# World, Machine, Cpu and costar_thread_s.  Containers are read with
# libstdc++'s layout.


def _vector(v):
    impl = v["_M_impl"]
    start = impl["_M_start"]
    return [(start + i).dereference() for i in range(int(impl["_M_finish"] - start))]


def _pointee(unique_ptr):
    """What a std::unique_ptr points to (its first member is the pointer)."""
    target = unique_ptr.type.strip_typedefs().template_argument(0)
    return unique_ptr.address.cast(target.pointer().pointer()).dereference().dereference()


def _string(s):
    return s["_M_dataplus"]["_M_p"].string(length=int(s["_M_string_length"]))


def _symbol(address):
    """The function at `address`, from the symbol tables (no debug info needed)."""
    text = gdb.execute(f"info symbol {address:#x}", to_string=True)
    if text.startswith("No symbol"):
        return None
    return re.split(r" \+ \d+ in section | in section ", text)[0].strip()


def _worlds():
    try:
        entry = gdb.parse_and_eval("costar_debug_worlds")
    except gdb.error:
        raise gdb.GdbError("costar_debug_worlds isn't there: is this a costar program?") from None
    worlds = []
    while int(entry) != 0:
        worlds.append(entry["world_"].dereference())
        entry = entry["next_"]
    return list(reversed(worlds))  # oldest first


class Thread:
    def __init__(self, machine, index, value):
        self.machine = machine
        self.index = index
        self.value = value
        top = int(value["stack"]["sp"])
        self.stack = (top - int(value["stack"]["size"]), top)

    @property
    def address(self):
        return int(self.value.address)

    def is_current(self):
        """The thread the RTOS last switched to."""
        return int(self.machine.cpu["running_"]) == self.address

    def is_here(self):
        """Running on the selected GDB thread right now."""
        sp = int(gdb.newest_frame().read_register("rsp"))
        return self.stack[0] <= sp < self.stack[1]

    def state(self):
        if self.is_here():
            return "running here"
        if bool(self.value["finished"]):
            return "finished"
        if not bool(self.value["started"]):
            return "not started"
        return "current" if self.is_current() else "suspended"

    def name(self):
        """The RTOS's name for the thread (costar_thread_describe()), or "-"."""
        return _string(self.value["name"]) or "-"

    def describe(self):
        """The thread's code: the task function for RTOS ports costar knows."""
        entry = int(self.value["entry"])
        name = _symbol(entry) or f"{entry:#x}"
        arg = int(self.value["arg"])
        if name == "prvTaskEntry" and arg:  # ports/freertos: arg is a PortThread_t { xThread, pxCode, ... }
            code = struct.unpack("<Q", bytes(gdb.selected_inferior().read_memory(arg + 8, 8)))[0]
            return f"FreeRTOS task {_symbol(code) or hex(code)}"
        if name == "nct_entry":  # ports/zephyr/runner.c: arg is the nct thread's slot
            return f"Zephyr thread (slot {arg})"
        return name


class Machine:
    def __init__(self, world_index, world, value):
        self.world_index = world_index
        self.world = world
        self.value = value
        self.cpu = value["cpu_"]
        self.id = int(value["id_"])
        self.name = _string(value["name_"]) or f"m{self.id}"

    def label(self, worlds):
        return f"w{self.world_index}.{self.name}" if worlds > 1 else self.name

    def threads(self):
        return [Thread(self, i, _pointee(t)) for i, t in enumerate(_vector(self.cpu["threads_"]))]

    def has_image(self):
        return bool(self.value["image_file_"]["_M_payload"]["_M_engaged"])  # a std::optional

    def cpu_state(self):
        """As Inspect shows it: awake, halted, busy, stopped (by its
        firmware), off, or passive (no firmware)."""
        if not self.has_image():
            return "passive"
        state = str(self.cpu["state_"]).rsplit("::", 1)[-1].lower()
        if state == "exited":
            return "stopped" if bool(self.value["powered_"]) else "off"
        return state


def _machines():
    worlds = _worlds()
    machines = []
    for w, world in enumerate(worlds):
        machines += [Machine(w, world, _pointee(m)) for m in _vector(world["machines_"])]
    return machines, len(worlds)


def _find_machine(spec):
    machines, worlds = _machines()
    if not machines:
        raise gdb.GdbError("no machines (yet): no World is listed for debuggers (docs/DEBUGGING.md)")
    if not spec:
        here = [m for m in machines if any(t.is_here() for t in m.threads())]
        if here:
            return here[0]
        with_threads = [m for m in machines if m.threads()]
        if len(with_threads) == 1:
            return with_threads[0]
        raise gdb.GdbError("no machine's code is running here: name one (see `costar machines`)")
    world = None
    match = re.fullmatch(r"w(\d+)\.(.+)", spec)
    if match:
        world, spec = int(match.group(1)), match.group(2)
    found = [m for m in machines if (world is None or m.world_index == world) and spec in (m.name, f"m{m.id}")]
    if not found:
        raise gdb.GdbError(f"no machine {spec!r} (see `costar machines`)")
    if len(found) > 1:
        raise gdb.GdbError(f"more than one machine {spec!r}: prefix its world, as in w{found[0].world_index}.{spec}")
    return found[0]


# ── Looking at a suspended fiber ──────────────────────────────────────
# jump_fcontext (Boost.Context, x86_64 System V) saves a fiber's registers
# on its own stack and leaves `context` pointing at them:
#   +0x00 MXCSR, x87 control word   +0x08 stack guard (unused here)
#   +0x10 r12  +0x18 r13  +0x20 r14  +0x28 r15  +0x30 rbx  +0x38 rbp
#   +0x40 return address; the fiber's stack pointer is context + 0x48.
# A Boost built with CET shadow stacks keeps the shadow stack pointer first,
# moving everything up by 8: that layout is tried if the first doesn't give
# a return address in known code.

_SAVED = ("r12", "r13", "r14", "r15", "rbx", "rbp", "rip")


def _saved_registers(thread):
    context = int(thread.value["context"])
    words = struct.unpack("<10Q", bytes(gdb.selected_inferior().read_memory(context, 80)))
    for shift in (0, 1):
        registers = dict(zip(_SAVED, words[2 + shift:9 + shift]))
        if _symbol(registers["rip"]) is not None:
            registers["rsp"] = context + 0x48 + 8 * shift
            return registers
    raise gdb.GdbError(f"thread {thread.index}'s saved context at {context:#x} doesn't look like one")


class _OnFiber:
    """Within this, frame 0 is where `thread` was suspended."""

    def __init__(self, thread):
        self.thread = thread

    def __enter__(self):
        if "x86-64" not in gdb.newest_frame().architecture().name():
            raise gdb.GdbError("costar bt reads x86_64 fiber contexts only")
        registers = _saved_registers(self.thread)
        gdb.newest_frame().select()
        self.saved = {r: int(gdb.newest_frame().read_register(r)) for r in (*_SAVED, "rsp")}
        for register, value in registers.items():
            gdb.execute(f"set ${register} = {value:#x}", to_string=True)
        return self

    def __exit__(self, *exc):
        gdb.newest_frame().select()
        for register, value in self.saved.items():
            gdb.execute(f"set ${register} = {value:#x}", to_string=True)
        gdb.newest_frame().select()


def _where(thread, depth=3):
    """The innermost frames of the firmware's own code where a suspended
    thread waits (past costar's switch and the image stub's costar_*),
    innermost first."""
    if thread.state() not in ("suspended", "current"):
        return ""
    names = []
    try:
        with _OnFiber(thread):
            frame = gdb.newest_frame()
            while frame is not None and len(names) < depth:
                name = frame.name() or ""
                if "/costar-" in (gdb.solib_name(frame.pc()) or "") and not name.startswith("costar_"):
                    names.append(name or f"{frame.pc():#x}")
                frame = frame.older()
    except gdb.error:
        pass
    return " < ".join(names)


# ── Commands ──────────────────────────────────────────────────────────


class Costar(gdb.Command):
    """costar machines | costar threads [MACHINE] | costar bt [MACHINE:]THREAD [N]"""

    def __init__(self):
        super().__init__("costar", gdb.COMMAND_STACK, gdb.COMPLETE_NONE, True)


class Machines(gdb.Command):
    """List costar's machines: name, id, virtual time, CPU state, threads.
A * marks the machine whose code is running on the selected thread."""

    def __init__(self):
        super().__init__("costar machines", gdb.COMMAND_STACK)

    def invoke(self, argument, from_tty):
        machines, worlds = _machines()
        if not machines:
            print("no machines (yet): no World is listed for debuggers (docs/DEBUGGING.md)")
        for m in machines:
            threads = m.threads()
            here = "*" if any(t.is_here() for t in threads) else " "
            now = int(m.world["now_"])
            print(f"{here} {m.label(worlds):<16} m{m.id:<3} {now:>14} ns  cpu {m.cpu_state():<8} "
                  f"{len(threads)} thread{'s' if len(threads) != 1 else ''}")


class Threads(gdb.Command):
    """List a machine's RTOS threads (costar fibers): state, the RTOS's name for
it, its code, and where a suspended one is.  "current" is the thread the
RTOS last switched to."""

    def __init__(self):
        super().__init__("costar threads", gdb.COMMAND_STACK)

    def invoke(self, argument, from_tty):
        machine = _find_machine(argument.strip())
        threads = machine.threads()
        print(f"{machine.name} (m{machine.id}), {int(machine.world['now_'])} ns, cpu {machine.cpu_state()}")
        for t in threads:
            where = _where(t)
            print(f"  {t.index:>2}  {t.state():<12} {t.name():<12} {t.describe()}" + (f"  in {where}" if where else ""))


class Backtrace(gdb.Command):
    """Backtrace of a machine's RTOS thread: costar bt [MACHINE:]THREAD [bt arguments].
THREAD is its number in `costar threads`."""

    def __init__(self):
        super().__init__("costar bt", gdb.COMMAND_STACK)

    def invoke(self, argument, from_tty):
        words = argument.split(None, 1)
        if not words:
            raise gdb.GdbError("usage: costar bt [MACHINE:]THREAD [bt arguments]")
        spec, _, index = words[0].rpartition(":")
        machine = _find_machine(spec)
        threads = machine.threads()
        if not index.isdigit() or int(index) >= len(threads):
            raise gdb.GdbError(f"{machine.name} has threads 0 to {len(threads) - 1}")
        thread = threads[int(index)]
        rest = words[1] if len(words) > 1 else ""
        state = thread.state()
        if state == "running here":
            gdb.execute(f"bt {rest}")
        elif state in ("suspended", "current"):
            with _OnFiber(thread):
                gdb.execute(f"bt {rest}")
        else:
            raise gdb.GdbError(f"thread {thread.index} is {state}: it has no stack to show")


Costar()
Machines()
Threads()
Backtrace()
