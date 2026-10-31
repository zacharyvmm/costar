"""A plant written in Python closes the thermostat's control loop.

firmware/freertos_thermostat reads a temperature from register 0 every
100 ms and switches the heater (register 1) on below 20.0 °C.  The plant
below is the built-in C++ "thermal" plant (apps/costar/thermal_plant.cpp)
rewritten in Python, stepped in lockstep with the session; the firmware sees
the same world, so the trace is the same as with the compiled plant.
"""
from costar import ms

THERMOSTAT = """
name = "thermostat"

[[machine]]
name = "thermostat"
image = "freertos_thermostat.so"
devices = [{ name = "thermo", kind = "registers" }, { name = "log", kind = "memory", size = 64 }]
"""


class ThermalPlant:
    """A room: warms by `heat` per step while the heater is on, cools by
    `cool` otherwise.  Temperatures are in 0.1 °C."""

    def __init__(self, target, initial=180, heat=5, cool=3):
        self.target = target
        self.temperature = initial
        self.heat = heat
        self.cool = cool

    def __call__(self, step):
        heating = step.register(self.target, 1) != 0
        step.set_register(self.target, 0, self.temperature)
        self.temperature = max(0, self.temperature + (self.heat if heating else -self.cool))


def test_a_python_plant_matches_the_compiled_one(costar_session, scenarios):
    compiled = costar_session(scenarios / "thermostat.toml")  # the "thermal" plant, every 50 ms
    compiled.run_until(ms(1000))

    scripted = costar_session(text=THERMOSTAT)
    scripted.lockstep(ms(50), ms(1000), ThermalPlant("thermostat.thermo"))

    trace = [str(r) for r in scripted.trace()]
    assert trace == [str(r) for r in compiled.trace()]
    assert trace == (scenarios / "thermostat.trace").read_text().splitlines()
    # The loop closes: the heater went on and off.
    heater = [r.text for r in scripted.trace() if r.label == "reg"]
    assert "thermo[1]=1" in heater and "thermo[1]=0" in heater


def test_a_python_plant_s_inputs_replay_on_seek_and_clone(costar_session):
    session = costar_session(text=THERMOSTAT)
    session.lockstep(ms(50), ms(1000), ThermalPlant("thermostat.thermo"))
    trace = session.trace()

    branch = session.clone()
    assert branch.trace() == trace
    assert branch.inspect() == session.inspect()

    session.seek(ms(1000))  # rebuilt, and replayed
    assert session.trace() == trace
    session.seek(ms(500))  # drops the inputs after 500 ms
    assert session.trace() == [r for r in trace if r.at <= ms(500)]
