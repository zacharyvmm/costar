"""A Python client for costar's gRPC API, and a pytest plugin.

    with costar.Server("build/costar-grpc") as server, server.client() as client:
        session = client.create_session("tests/scenarios/link_nodes.toml", images=["build/firmware"])
        record = session.wait_for("got_ping", machine="responder", timeout=costar.ms(50))
        session.run_until(costar.ms(30))

Times are integers in nanoseconds of virtual time; ``us()``, ``ms()`` and
``seconds()`` convert.  See the README's "Scripted tests in Python" and
docs/ARCHITECTURE.md §10.
"""
import pkgutil

# The generated costar.v1 stubs may live in another directory on sys.path (a
# CMake build's python/ directory) than the rest of the package.
__path__ = pkgutil.extend_path(__path__, __name__)

from costar.client import (  # noqa: E402
    Client,
    CostarError,
    Device,
    Machine,
    PlantStep,
    Record,
    Session,
    Snapshot,
    Status,
    Thread,
    WaitTimeout,
    ms,
    seconds,
    us,
)
from costar.server import Server  # noqa: E402

__all__ = [
    "Client",
    "CostarError",
    "Device",
    "Machine",
    "PlantStep",
    "Record",
    "Server",
    "Session",
    "Snapshot",
    "Status",
    "Thread",
    "WaitTimeout",
    "ms",
    "seconds",
    "us",
]
