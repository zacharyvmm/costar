"""Talking to firmware over a link, as a host would: send a frame, wait for
the answer.  firmware/freertos_link_node answers "P<n>" with "R<n>"."""
import pytest

import costar
from costar import ms

# A responder node, and a passive machine standing for the test's host.
LINK = """
name = "link"

[[machine]]
name = "responder"
image = "freertos_link_node.so"
devices = [{ name = "link", kind = "channel", irq = 1 }, { name = "node", kind = "registers" }]

[[machine]]
name = "host"
devices = [{ name = "link", kind = "channel" }]

[[wire]]
endpoints = ["responder.link", "host.link"]
latency_ms = 2

[[input]]                  # register 0 of "node" picks the role: responder
target = "responder.node"
register = 0
value = 1
"""


def test_ping_and_wait_for_the_reply(costar_session):
    session = costar_session(text=LINK)
    session.run_until(ms(5))

    session.send("host.link", b"P\x07")
    ping = session.wait_for("got_ping", machine="responder", value=7, timeout=ms(10))
    assert ping.at == ms(7)  # sent at 5 ms, on a 2 ms link

    reply = session.wait_for("rx", machine="host", timeout=ms(10))
    assert reply.text == "link 5207"
    assert reply.at == ms(9)
    assert session.now == ms(9)  # the session stopped at the reply


def test_waiting_in_vain_times_out_in_virtual_time(costar_session):
    session = costar_session(text=LINK)
    with pytest.raises(costar.WaitTimeout, match="got_ping"):
        session.wait_for("got_ping", timeout=ms(50))
    assert session.now == ms(50)


def test_inspect_shows_each_threads_stack_use(costar_session):
    session = costar_session(text=LINK)
    session.run_until(ms(5))
    snapshot = session.inspect()
    handler = next(t for t in snapshot.machines["responder"].threads if t.name == "handler")
    assert handler.declared == 1024  # configMINIMAL_STACK_SIZE words of 4 bytes
    assert 0 < handler.used <= handler.limit == 10 * 1024
    assert snapshot.machines["host"].threads == []  # a passive machine


def test_bad_requests_are_errors(costar_session):
    session = costar_session(text=LINK)
    with pytest.raises(costar.CostarError) as error:
        session.send("nobody.link", b"x")
    assert error.value.code.name == "INVALID_ARGUMENT"
    with pytest.raises(ValueError):
        session.wait_for("rx", machine="nobody")
