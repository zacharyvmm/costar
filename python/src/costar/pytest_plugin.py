"""pytest fixtures for tests that drive costar sessions.

Installed with the costar package (a ``pytest11`` entry point); without
installing it, load it with ``-p costar.pytest_plugin``.

Options:
  --costar-grpc PATH         the server to start (default: $COSTAR_GRPC, or costar-grpc on PATH)
  --costar-address HOST:PORT use a running server instead
  --costar-images DIR        where firmware images are (repeatable; also $COSTAR_IMAGES)

Fixtures:
  costar_server   a Client of the server, one per test run
  costar_images   the image directories, as Paths
  costar_session  a factory: costar_session(scenario_file) or
                  costar_session(text=...); its sessions are destroyed after
                  the test
"""
from __future__ import annotations

import os
from pathlib import Path

import pytest

from costar.client import Client, CostarError
from costar.server import Server


def pytest_addoption(parser):
    group = parser.getgroup("costar")
    group.addoption("--costar-grpc", metavar="PATH",
                    help="the costar-grpc server to start (default: $COSTAR_GRPC, or costar-grpc on PATH)")
    group.addoption("--costar-address", metavar="HOST:PORT",
                    help="use a running costar-grpc server instead of starting one")
    group.addoption("--costar-images", metavar="DIR", action="append", default=[],
                    help="a directory of firmware images (repeatable; also $COSTAR_IMAGES, ':'-separated)")


@pytest.fixture(scope="session")
def costar_server(pytestconfig):
    """A Client of the costar-grpc server for this test run."""
    address = pytestconfig.getoption("costar_address")
    if address:
        with Client(address) as client:
            yield client
        return
    executable = pytestconfig.getoption("costar_grpc") or os.environ.get("COSTAR_GRPC") or "costar-grpc"
    try:
        server = Server(executable)
    except (FileNotFoundError, CostarError) as error:
        pytest.fail(f"{error} (pass --costar-grpc PATH or --costar-address HOST:PORT)", pytrace=False)
    with server, server.client() as client:
        yield client


@pytest.fixture(scope="session")
def costar_images(pytestconfig):
    """The directories firmware images are looked up in."""
    dirs = list(pytestconfig.getoption("costar_images"))
    dirs += [d for d in os.environ.get("COSTAR_IMAGES", "").split(os.pathsep) if d]
    return [Path(d).resolve() for d in dirs]


@pytest.fixture
def costar_session(costar_server, costar_images):
    """Make sessions: costar_session(scenario_file, images=()) or
    costar_session(text=..., images=()).  They are destroyed after the test."""
    sessions = []

    def create(scenario=None, *, text=None, images=()):
        session = costar_server.create_session(scenario, text=text, images=[*images, *costar_images])
        sessions.append(session)
        return session

    yield create
    for session in sessions:
        try:
            session.destroy()
        except CostarError:
            pass  # destroyed by the test
