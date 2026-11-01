"""A Zephyr application's console and UART, scripted the way a Renode
Robot test would ("wait for line", "write line").
firmware/zephyr_uart_flash echoes what it receives in upper case."""
from costar import ms

UART_ECHO = """
name = "uart-echo"

[[machine]]
name = "zephyr"
image = "zephyr_uart_flash.so"
devices = [{ name = "uart", kind = "channel", irq = 5 },
           { name = "flash", kind = "memory", size = 1024 }]
"""


def test_console_and_uart(costar_session, require_image):
    require_image("zephyr_uart_flash.so")
    session = costar_session(text=UART_ECHO)

    boot = session.wait_for("log", r"^boot (\d+)$", machine="zephyr", timeout=ms(10))
    assert boot.text == "boot 1"  # erased flash: the first boot

    session.run_until(ms(3))
    session.write("zephyr.uart", "hi")  # arrives now, as if typed
    line = session.wait_for("log", r"^got i at \d+ us$", timeout=ms(10))
    assert line.text == "got i at 3000 us"
    echo = session.wait_for("tx", r"^uart 49$", machine="zephyr", timeout=ms(10))  # "I"
    assert echo.at == ms(3)
