"""Starting servers, and talking to more than one."""
import os
import shutil

import pytest

import costar
from costar import ms


def test_servers_get_free_ports_and_shut_down_cleanly(pytestconfig, costar_images, scenarios):
    executable = pytestconfig.getoption("costar_grpc") or os.environ.get("COSTAR_GRPC") or "costar-grpc"
    if shutil.which(executable) is None:
        pytest.skip("no costar-grpc to start")
    with costar.Server(executable) as one, costar.Server(executable) as two:
        assert one.address != two.address
        with one.client() as client:
            session = client.create_session(scenarios / "thermostat.toml", images=costar_images)
            session.run(None)  # in the background, for ever; the server closes it on shutdown
            assert [s.session for s in client.sessions()] == [session.id]
    assert one.returncode == 0 and two.returncode == 0


def test_sessions_are_context_managers(costar_server, costar_images, scenarios):
    with costar_server.create_session(scenarios / "ping_pong.toml", images=costar_images) as session:
        assert session.run_until(ms(1)).now == ms(1)
        snapshot = session.inspect()
        assert snapshot.now == ms(1)
        assert set(snapshot.machines) == {"sender", "receiver"}
    assert session.id not in [s.session for s in costar_server.sessions()]
