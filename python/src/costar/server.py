"""Running a costar-grpc server for a script or a test run."""
from __future__ import annotations

import os
import queue
import shutil
import subprocess
import sys
import threading
from typing import Optional, Sequence, Union

from costar.client import Client, CostarError


class Server:
    """A costar-grpc process listening on a free local port.  A context
    manager: on exit it is shut down cleanly (SIGTERM), which closes its
    sessions.

        with Server("build/costar-grpc") as server, server.client() as client:
            ...

    The server's stderr is passed on to this process's sys.stderr.
    """

    def __init__(self, executable: Union[str, os.PathLike] = "costar-grpc", *, host: str = "127.0.0.1",
                 args: Sequence[str] = (), startup_timeout: float = 10.0):
        path = shutil.which(os.fspath(executable))
        if path is None:
            raise FileNotFoundError(f"cannot find the costar-grpc server {os.fspath(executable)!r}")
        self.executable = path
        self._process = subprocess.Popen([path, "--listen", f"{host}:0", *args], stdin=subprocess.DEVNULL,
                                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        # The server says which port it got: "costar-grpc: listening on HOST:PORT".
        lines: queue.Queue = queue.Queue()
        self._reader = threading.Thread(target=self._read_stderr, args=(lines,), daemon=True)
        self._reader.start()
        prefix = "costar-grpc: listening on "
        try:
            while True:
                line = lines.get(timeout=startup_timeout)
                if line is None:
                    raise CostarError(f"{path} exited with status {self._process.wait()} before listening")
                if line.startswith(prefix):
                    self.address = line[len(prefix):].strip()
                    break
        except queue.Empty:
            self.stop()
            raise CostarError(f"{path} did not start listening within {startup_timeout} s") from None
        except BaseException:
            self.stop()
            raise

    def _read_stderr(self, lines: queue.Queue) -> None:
        listening = False
        for line in self._process.stderr:
            if listening:
                sys.stderr.write(line)
            else:
                lines.put(line)
                listening = line.startswith("costar-grpc: listening on ")
        lines.put(None)

    def client(self, **kwargs) -> Client:
        """A new connection to this server."""
        return Client(self.address, **kwargs)

    @property
    def returncode(self) -> Optional[int]:
        """The exit status, once the server has stopped."""
        return self._process.poll()

    def stop(self, timeout: float = 10.0) -> Optional[int]:
        """Shut the server down (SIGTERM, then SIGKILL after ``timeout``
        seconds) and return its exit status."""
        if self._process.poll() is None:
            self._process.terminate()
            try:
                self._process.wait(timeout)
            except subprocess.TimeoutExpired:
                self._process.kill()
                self._process.wait()
        self._reader.join(timeout)
        return self._process.returncode

    def __enter__(self) -> "Server":
        return self

    def __exit__(self, *exc) -> None:
        self.stop()
