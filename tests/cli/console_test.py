"""Terminals: costar run --console on one, and pty bridges.

- With --console on a terminal, standard input goes into raw mode (keys reach
  the firmware as typed, Ctrl-C included, with no local echo), Ctrl-] ends the
  run, a signal ends it too, and the terminal is put back as it was.  So
  does standard output's reader going away (no SIGPIPE), and a signal still
  ends a run whose standard output is full.
- A pty bridge is a terminal a program attaches to through its link; a signal
  ends the run cleanly: the trace is written and the link removed.

    console_test.py <costar> <console_echo.toml> <images dir>
"""
import array
import fcntl
import os
import pty
import select
import signal
import subprocess
import sys
import tempfile
import termios
import time

costar, scenario, images = sys.argv[1:4]


def read_until(fd, expected, timeout=5.0):
    data = b""
    deadline = time.monotonic() + timeout
    while expected not in data:
        left = deadline - time.monotonic()
        if left <= 0 or not select.select([fd], [], [], left)[0]:
            raise AssertionError(f"expected {expected!r}, got {data!r}")
        data += os.read(fd, 1024)
    return data


def start(stdout=None):
    """A console run on a new terminal (standard output too, unless given),
    once the terminal is in raw mode: (master, terminal, its settings
    before, process)."""
    master, terminal = pty.openpty()
    before = termios.tcgetattr(terminal)
    process = subprocess.Popen(
        [costar, "run", scenario, "--images", images, "--console", "echo.uart"],
        stdin=terminal, stdout=terminal if stdout is None else stdout, stderr=subprocess.PIPE)
    banner = process.stderr.readline()
    assert b"Ctrl-] ends the run" in banner, banner
    deadline = time.monotonic() + 5
    while termios.tcgetattr(terminal)[3] & termios.ICANON:  # wait for raw mode
        assert time.monotonic() < deadline, "the terminal never went into raw mode"
        time.sleep(0.01)
    return master, terminal, before, process


def finish(master, terminal, before, process):
    try:
        status = process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        raise AssertionError("the run did not end")
    assert status == 0, f"exit status {status}: {process.stderr.read()!r}"
    assert termios.tcgetattr(terminal) == before, "the terminal was not put back"
    os.close(master)
    os.close(terminal)


def console(end):
    master, terminal, before, process = start()
    os.write(master, b"hi")  # no newline: raw mode passes keys at once
    assert read_until(master, b"HI") == b"HI", "a key was echoed locally"
    os.write(master, b"\x03")  # Ctrl-C is a byte for the firmware, not a signal
    read_until(master, b"\x03")
    assert process.poll() is None, "Ctrl-C ended the run"

    end(master, process)
    finish(master, terminal, before, process)


def output_reader_gone():
    """Standard output's reader goes: the next write ends the run (no
    SIGPIPE), and the terminal is put back."""
    read, write = os.pipe()
    master, terminal, before, process = start(stdout=write)
    os.close(write)
    os.close(read)
    os.write(master, b"hi")
    finish(master, terminal, before, process)


def output_blocked():
    """Standard output is a full pipe nobody reads: a signal still ends the
    run, at once, and the terminal is put back."""
    read, write = os.pipe()
    fcntl.fcntl(write, fcntl.F_SETPIPE_SZ, 4096)  # a page: it fills quickly
    master, terminal, before, process = start(stdout=write)
    os.close(write)
    os.set_blocking(master, False)
    for _ in range(16):  # 8 KiB, a frame at a time (the echo takes 1 KiB)
        try:
            os.write(master, b"x" * 512)
        except BlockingIOError:
            pass  # costar stopped reading: it is waiting to write
        time.sleep(0.02)
    time.sleep(0.3)
    waiting = array.array("i", [0])
    fcntl.ioctl(read, termios.FIONREAD, waiting)
    assert waiting[0] > 0 and process.poll() is None, f"{waiting[0]} bytes written, exit {process.poll()}"
    process.send_signal(signal.SIGTERM)
    finish(master, terminal, before, process)
    os.close(read)


def terminal_not_reopenable():
    """A terminal costar can't open again (non-blocking, for its writes) is
    an error, not a console whose writes could block a stop; the terminal is
    left as it was."""
    if os.geteuid() == 0:
        return  # permissions don't stop root
    master, terminal = pty.openpty()
    before = termios.tcgetattr(terminal)
    os.chmod(os.ttyname(terminal), 0)
    process = subprocess.Popen(
        [costar, "run", scenario, "--images", images, "--console", "echo.uart"],
        stdin=terminal, stdout=terminal, stderr=subprocess.PIPE)
    try:
        status = process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        raise AssertionError("the run did not end")
    error = process.stderr.read()
    assert status == 1 and b"can't open the console's terminal again" in error, (status, error)
    assert termios.tcgetattr(terminal) == before, "the terminal was not put back"
    os.close(master)
    os.close(terminal)


def other_controlling_terminal():
    """Standard output is /dev/tty as opened on terminal A, while costar's
    controlling terminal is B: opening it again would give B, so it's an
    error, and A is left as it was."""
    master_a, terminal_a = pty.openpty()
    master_b, terminal_b = pty.openpty()
    before = termios.tcgetattr(terminal_a)
    path_a, path_b = os.ttyname(terminal_a), os.ttyname(terminal_b)

    def switch_terminals():
        os.setsid()
        signal.signal(signal.SIGHUP, signal.SIG_IGN)  # giving up a terminal hangs it up
        a = os.open(path_a, os.O_RDWR)  # the session's controlling terminal
        alias = os.open("/dev/tty", os.O_RDWR)  # A, through the alias
        fcntl.ioctl(a, termios.TIOCNOTTY)
        os.open(path_b, os.O_RDWR)  # B controls the session now
        signal.signal(signal.SIGHUP, signal.SIG_DFL)
        os.dup2(alias, 0)
        os.dup2(alias, 1)

    process = subprocess.Popen(
        [costar, "run", scenario, "--images", images, "--console", "echo.uart"],
        stderr=subprocess.PIPE, preexec_fn=switch_terminals)
    try:
        status = process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        raise AssertionError("the run did not end")
    error = process.stderr.read()
    assert status == 1 and b"can't open the console's terminal again" in error, (status, error)
    assert termios.tcgetattr(terminal_a) == before, "terminal A was not put back"
    for fd in (master_a, terminal_a, master_b, terminal_b):
        os.close(fd)


def pty_bridge():
    with tempfile.TemporaryDirectory() as directory:
        bridged = os.path.join(directory, "bridged.toml")
        with open(scenario) as original, open(bridged, "w") as out:
            out.write(original.read() + '[[bridge]]\nendpoint = "echo.uart"\nkind = "pty"\nlink = "uart"\n')
        link = os.path.join(directory, "uart")
        trace = os.path.join(directory, "trace")
        process = subprocess.Popen([costar, "run", bridged, "--images", images, "--trace", trace],
                                   stderr=subprocess.PIPE)
        said = process.stderr.readline()
        assert b"echo.uart is bridged at /dev/pts/" in said, said
        terminal = os.open(link, os.O_RDWR | os.O_NOCTTY)
        os.write(terminal, b"ok")
        read_until(terminal, b"OK")
        os.close(terminal)

        process.send_signal(signal.SIGINT)
        status = process.wait(timeout=5)
        assert status == 0, f"exit status {status}: {process.stderr.read()!r}"
        assert not os.path.lexists(link), "the pty's link was left behind"
        with open(trace) as written:
            assert "m0 rx uart 6f6b\n" in written.read()
        assert sorted(os.listdir(directory)) == ["bridged.toml", "trace"]


console(lambda master, process: os.write(master, b"\x1d"))  # Ctrl-]
console(lambda master, process: process.send_signal(signal.SIGTERM))
output_reader_gone()
output_blocked()
terminal_not_reopenable()
other_controlling_terminal()
pty_bridge()
print("ok")
