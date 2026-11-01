"""Sessions on a costar-grpc server (proto/costar/v1/costar.proto).

A Session is one simulated world on the server.  Everything here is in
virtual time, so a test that drives a session the same way gets the same
trace every run: waits stop at trace records, not after wall-clock delays,
and inputs land at virtual times.
"""
from __future__ import annotations

import os
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Iterable, Optional, Pattern, Union

import grpc

from costar.v1 import costar_pb2 as pb
from costar.v1 import costar_pb2_grpc as pb_grpc

Data = Union[str, bytes]

# ── Virtual time: integers in nanoseconds ─────────────────────────────


def us(n: float) -> int:
    """Microseconds, in nanoseconds."""
    return round(n * 1_000)


def ms(n: float) -> int:
    """Milliseconds, in nanoseconds."""
    return round(n * 1_000_000)


def seconds(n: float) -> int:
    """Seconds, in nanoseconds."""
    return round(n * 1_000_000_000)


# How long wait_for() waits by default, in virtual time.
DEFAULT_WAIT = seconds(10)


class CostarError(Exception):
    """A call the server refused, or a simulation that failed."""

    def __init__(self, message: str, code: Optional[grpc.StatusCode] = None):
        super().__init__(message)
        self.code = code


class WaitTimeout(CostarError, AssertionError):
    """wait_for() reached its virtual-time deadline without a match."""


def _call(method, request, **kwargs):
    try:
        return method(request, **kwargs)
    except grpc.RpcError as error:
        raise CostarError(f"{error.code().name}: {error.details()}", error.code()) from None


def _bytes(data: Data) -> bytes:
    return data.encode() if isinstance(data, str) else bytes(data)


# ── What the server returns ───────────────────────────────────────────


@dataclass(frozen=True)
class Record:
    """One trace record.  ``text`` is set for text records (console output,
    frames as "<device> <hex>", ...); ``value`` for the others."""

    index: int
    at: int
    machine: str
    machine_id: int
    label: str
    value: int
    text: Optional[str]

    def __str__(self) -> str:
        """The record as costar's trace files write it."""
        if self.text is None:
            return f"{self.at} m{self.machine_id} {self.label} {self.value}"
        if not self.text:
            return f"{self.at} m{self.machine_id} {self.label}"
        return f"{self.at} m{self.machine_id} {self.label} {self.text}"


@dataclass(frozen=True)
class Status:
    session: int
    name: str
    now: int
    running: bool
    run_until: int
    trace_length: int
    error: str
    realtime: bool

    @staticmethod
    def _of(status: pb.SessionStatus) -> "Status":
        return Status(status.session, status.name, status.now_ns, status.running, status.run_until_ns,
                      status.trace_length, status.error, status.realtime)


@dataclass(frozen=True)
class Device:
    name: str
    kind: str  # "channel", "registers" or "memory"
    irq: Optional[int]
    queued_frames: int = 0
    registers: dict = field(default_factory=dict)  # register -> value
    memory: bytes = b""


@dataclass(frozen=True)
class Thread:
    """A live RTOS thread and its use of its stack, in bytes."""

    name: str  # the RTOS's name, or "thread <n>"
    declared: int  # the stack size its RTOS gave it (0: unknown)
    limit: int  # what costar's stack check allows (0: not checked)
    used: int  # the most it has used so far


@dataclass(frozen=True)
class Machine:
    id: int
    name: str
    cpu_state: str  # "awake", "halted", "busy", "stopped" (by firmware) or "off"
    devices: dict  # name -> Device
    threads: list = field(default_factory=list)  # Thread, in creation order


@dataclass(frozen=True)
class Snapshot:
    """A session's machines and devices at one instant."""

    now: int
    machines: dict  # name -> Machine

    def device(self, target: str) -> Device:
        """The device "machine.device"."""
        machine, _, device = target.partition(".")
        return self.machines[machine].devices[device]

    def register(self, target: str, reg: int) -> int:
        """A register of the registers device "machine.device" (0 if never written)."""
        return self.device(target).registers.get(reg, 0)

    @staticmethod
    def _of(snapshot: pb.Snapshot) -> "Snapshot":
        kinds = {pb.DeviceState.CHANNEL: "channel", pb.DeviceState.REGISTERS: "registers",
                 pb.DeviceState.MEMORY: "memory"}
        machines = {}
        for m in snapshot.machines:
            devices = {d.name: Device(d.name, kinds[d.kind], None if d.irq < 0 else d.irq, d.queued_frames,
                                      dict(d.registers), bytes(d.memory)) for d in m.devices}
            threads = [Thread(t.name, t.declared_bytes, t.limit_bytes, t.used_bytes) for t in m.threads]
            machines[m.name] = Machine(m.id, m.name, m.cpu_state, devices, threads)
        return Snapshot(snapshot.now_ns, machines)


# ── The server ────────────────────────────────────────────────────────


class Client:
    """A connection to a costar-grpc server.  A context manager."""

    def __init__(self, address: str, *, connect_timeout: float = 10.0):
        self.address = address
        self._channel = grpc.insecure_channel(address)
        try:
            grpc.channel_ready_future(self._channel).result(timeout=connect_timeout)
        except grpc.FutureTimeoutError:
            self._channel.close()
            raise CostarError(f"cannot connect to costar-grpc at {address}") from None
        self._stub = pb_grpc.CostarStub(self._channel)

    def create_session(self, scenario: Union[str, os.PathLike, None] = None, *, text: Optional[str] = None,
                       images: Iterable[Union[str, os.PathLike]] = ()) -> "Session":
        """A new session from a scenario file, or from ``text`` (then
        ``scenario``, if given, only names it: relative paths in it resolve
        from its directory).  Images are looked up next to the scenario, then
        in ``images``.  The server reads the files, so paths are the server's."""
        if text is None:
            if scenario is None:
                raise ValueError("create_session() needs a scenario file or text")
            text = Path(scenario).read_text()
        request = pb.CreateSessionRequest(scenario_toml=text, image_dirs=[str(Path(d).resolve()) for d in images])
        if scenario is not None:
            request.scenario_path = str(Path(scenario).resolve())
        return Session(self, Status._of(_call(self._stub.CreateSession, request)))

    def sessions(self) -> list:
        """The status of every session on the server."""
        return [Status._of(s) for s in _call(self._stub.ListSessions, pb.Empty()).sessions]

    def close(self) -> None:
        self._channel.close()

    def __enter__(self) -> "Client":
        return self

    def __exit__(self, *exc) -> None:
        self.close()


# ── Sessions ──────────────────────────────────────────────────────────


class Session:
    """One simulated world on the server.  A context manager that destroys
    the session on exit."""

    def __init__(self, client: Client, status: Status):
        self.client = client
        self.id = status.session
        self.name = status.name
        self._stub = client._stub
        snapshot = self.inspect()
        self._machines = sorted(snapshot.machines.values(), key=lambda m: m.id)
        # wait_for() looks at records from here on: after its last match.
        self._cursor = 0

    def __repr__(self) -> str:
        return f"<costar.Session {self.id} {self.name!r}>"

    def __enter__(self) -> "Session":
        return self

    def __exit__(self, *exc) -> None:
        self.destroy()

    def destroy(self) -> None:
        _call(self._stub.DestroySession, pb.SessionRef(session=self.id))

    def clone(self) -> "Session":
        """A new, paused session with this one's history, at this one's exact
        point in the run: a branch."""
        copy = Session(self.client, Status._of(_call(self._stub.CloneSession, self._ref())))
        copy._cursor = self._cursor
        return copy

    # ── Running ───────────────────────────────────────────────────────

    def status(self) -> Status:
        return Status._of(_call(self._stub.Status, self._ref()))

    @property
    def now(self) -> int:
        """Virtual time, in nanoseconds."""
        return self.status().now

    def run(self, until: Optional[int] = None, *, realtime: bool = False) -> Status:
        """Start running in the background toward ``until`` (None: until
        nothing is left to happen); see pause() and status()."""
        return self._run(until, realtime=realtime, wait=False)

    def run_until(self, until: Optional[int]) -> Status:
        """Run to virtual time ``until`` and return once there (or once
        nothing is left to happen, for None).  Raises if the simulation fails."""
        return self._checked(self._run(until, wait=True))

    def run_for(self, duration: int) -> Status:
        """Run for ``duration`` more of virtual time."""
        return self.run_until(self.now + duration)

    def pause(self) -> Status:
        return Status._of(_call(self._stub.Pause, self._ref()))

    def seek(self, time: int) -> Status:
        """Rebuild the world and replay its history to ``time``; inputs
        after ``time`` are dropped.  wait_for() then looks at records after
        ``time`` only."""
        status = Status._of(_call(self._stub.Seek, pb.SeekRequest(session=self.id, time_ns=time)))
        self._cursor = status.trace_length
        return status

    # ── Waiting for the firmware ──────────────────────────────────────

    def wait_for(self, label: Optional[str] = None, text: Union[str, Pattern, None] = None, *,
                 machine: Optional[str] = None, value: Optional[int] = None,
                 timeout: int = DEFAULT_WAIT) -> Record:
        """Run until a trace record matches, and return it.

        A record matches if it has the label, ``text`` (a regular expression,
        searched for in the record's text: a console line, a frame's
        "<device> <hex>"), the machine (by name) and the value given.  The
        search starts after the record the previous wait_for() returned, so
        records that are already there count.  The session stops at the
        instant of the record, so what the test does next lands at a
        deterministic point.  Raises WaitTimeout if nothing matches within
        ``timeout`` of virtual time (the session is then at the deadline).
        """
        pattern = re.compile(text) if isinstance(text, str) else text
        machine_id = self._machine_id(machine) if machine is not None else None

        def matches(r: Record) -> bool:
            return ((label is None or r.label == label) and (machine_id is None or r.machine_id == machine_id)
                    and (value is None or r.value == value)
                    and (pattern is None or (r.text is not None and pattern.search(r.text) is not None)))

        status = self.status()
        deadline = status.now + timeout
        checked = self._cursor
        stop_on = pb.TraceMatch(machine=machine or "", label=label or "", value=value)
        while True:
            for record in self.trace(checked, status.trace_length):
                if matches(record):
                    self._cursor = record.index + 1
                    return record
            checked = max(checked, status.trace_length)
            self._checked(status)
            if status.now >= deadline:
                what = ", ".join(f"{k}={v!r}" for k, v in
                                 (("label", label), ("text", text), ("machine", machine), ("value", value))
                                 if v is not None)
                raise WaitTimeout(f"{self.name}: no record with {what or 'anything'} by {deadline} ns "
                                  f"(waited {timeout} ns of virtual time)")
            # Stop at each record that could match; the server can't test the
            # regular expression.
            status = self._run(deadline, wait=True, stop_on=stop_on)

    def trace(self, start: int = 0, end: Optional[int] = None) -> list:
        """Trace records ``start`` to ``end`` (default: all so far)."""
        if end is None:
            end = self.status().trace_length
        records = []
        if start >= end:
            return records
        stream = self._stub.Watch(pb.WatchRequest(session=self.id, from_index=start))
        try:
            for r in stream:
                records.append(Record(r.index, r.at_ns, self._machines[r.machine].name, r.machine, r.label, r.value,
                                      r.text if r.HasField("text") else None))
                if r.index + 1 >= end:
                    break
        except grpc.RpcError as error:
            raise CostarError(f"{error.code().name}: {error.details()}", error.code()) from None
        finally:
            stream.cancel()
        return records

    # ── Inputs ────────────────────────────────────────────────────────
    # Each takes effect at virtual time ``at``, or now if that is earlier
    # (the default), and is recorded: seek() and clone() replay it exactly.

    def deliver(self, endpoint: str, data: Data, *, at: int = 0) -> Status:
        """A frame arrives at the channel "machine.device", as if the host
        typed ``data`` into a UART."""
        return self._input(endpoint, at, deliver=_bytes(data))

    write = deliver

    def send(self, endpoint: str, data: Data, *, at: int = 0) -> Status:
        """The channel "machine.device" transmits a frame onto its wires
        (from a passive machine standing for a host, say)."""
        return self._input(endpoint, at, send=_bytes(data))

    def set_register(self, target: str, reg: int, value: int, *, at: int = 0) -> Status:
        return self._input(target, at, set_register=pb.RegisterWrite(reg=reg, value=value))

    def write_memory(self, target: str, offset: int, data: Data, *, at: int = 0) -> Status:
        return self._input(target, at, write_memory=pb.MemoryWrite(offset=offset, data=_bytes(data)))

    def power(self, machine: str, action: str, *, at: int = 0) -> Status:
        """Switch a machine "off" or "on", or "reset" it."""
        actions = {"reset": pb.POWER_RESET, "off": pb.POWER_OFF, "on": pb.POWER_ON}
        if action not in actions:
            raise ValueError(f"power action must be one of {', '.join(actions)}")
        return self._input(machine, at, power=actions[action])

    def inspect(self) -> Snapshot:
        return Snapshot._of(_call(self._stub.Inspect, self._ref()))

    # ── Plants in Python ──────────────────────────────────────────────

    def lockstep(self, period: int, until: int, step: Callable[["PlantStep"], None]) -> Status:
        """Run to ``until``, calling ``step`` every ``period`` of virtual
        time from now on, as the world steps a built-in plant.

        Each call gets a PlantStep for its time t: the devices as they are
        just before t, and inputs that take effect at t, before the firmware
        runs at t.  The firmware can't tell it from a compiled plant, and
        since the inputs are recorded, seek() and clone() replay them.  (The
        plant's own Python state isn't rewound by a seek.)  The first step is
        at the current time; if the session already ran to it, the world's
        own events at that instant come first.
        """
        if period <= 0:
            raise ValueError("a plant's period must be positive")
        t = self.now
        while t <= until:
            if t > self.now:
                self.run_until(t - 1)
            step(PlantStep(self, t, self.inspect()))
            t += period
        return self.run_until(until)

    # ── Internals ─────────────────────────────────────────────────────

    def _ref(self) -> pb.SessionRef:
        return pb.SessionRef(session=self.id)

    def _machine_id(self, name: str) -> int:
        for m in self._machines:
            if m.name == name:
                return m.id
        raise ValueError(f"{self.name} has no machine named {name!r}")

    def _run(self, until: Optional[int], *, realtime: bool = False, wait: bool,
             stop_on: Optional[pb.TraceMatch] = None) -> Status:
        request = pb.RunRequest(session=self.id, realtime=realtime, wait=wait)
        if until is not None:
            request.until_ns = until
        if stop_on is not None:
            request.stop_on.CopyFrom(stop_on)
        return Status._of(_call(self._stub.Run, request))

    def _checked(self, status: Status) -> Status:
        if status.error:
            raise CostarError(f"{self.name}: the simulation failed at {status.now} ns: {status.error}")
        return status

    def _input(self, target: str, at: int, **action) -> Status:
        return Status._of(_call(self._stub.Input, pb.InputRequest(session=self.id, target=target, at_ns=at, **action)))


class PlantStep:
    """What a Python plant sees at one step (see Session.lockstep)."""

    def __init__(self, session: Session, time: int, snapshot: Snapshot):
        self.session = session
        self.time = time
        self.snapshot = snapshot  # just before ``time``

    def register(self, target: str, reg: int) -> int:
        return self.snapshot.register(target, reg)

    def set_register(self, target: str, reg: int, value: int) -> None:
        self.session.set_register(target, reg, value, at=self.time)

    def deliver(self, endpoint: str, data: Data) -> None:
        self.session.deliver(endpoint, data, at=self.time)

    def send(self, endpoint: str, data: Data) -> None:
        self.session.send(endpoint, data, at=self.time)

    def write_memory(self, target: str, offset: int, data: Data) -> None:
        self.session.write_memory(target, offset, data, at=self.time)
