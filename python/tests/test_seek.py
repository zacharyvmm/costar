"""History is deterministic: seeking back and running again, or cloning,
gives the same trace, inputs included."""
from costar import ms


def test_seek_back_and_run_again(costar_session, scenarios):
    session = costar_session(scenarios / "link_nodes.toml")
    session.power("responder", "reset", at=ms(15))
    session.run_until(ms(40))
    first = session.trace()
    assert any(r.label == "power" for r in first)

    # Back to before the reset: the replay keeps it, since it was
    # submitted before 20 ms (only inputs after the seek's time are dropped).
    session.seek(ms(20))
    assert session.now == ms(20)
    assert session.trace() == [r for r in first if r.at <= ms(20)]
    session.run_until(ms(40))
    assert session.trace() == first

    session.seek(ms(12))  # drops the reset
    session.run_until(ms(40))
    assert not any(r.label == "power" for r in session.trace())


def test_wait_for_after_a_seek_finds_the_same_record(costar_session, scenarios):
    session = costar_session(scenarios / "link_nodes.toml")
    pong = session.wait_for("got_pong", machine="initiator", value=3, timeout=ms(30))
    session.seek(ms(5))
    assert session.wait_for("got_pong", machine="initiator", value=3, timeout=ms(30)) == pong


def test_a_clone_goes_its_own_way(costar_session, scenarios):
    session = costar_session(scenarios / "link_nodes.toml")
    session.run_until(ms(15))
    branch = session.clone()
    branch.power("responder", "off")
    branch.run_until(ms(40))
    session.run_until(ms(40))
    assert not any(r.label == "got_ping" and r.at > ms(15) for r in branch.trace())
    assert any(r.label == "got_ping" and r.at > ms(15) for r in session.trace())
    assert branch.inspect().machines["responder"].cpu_state == "off"
