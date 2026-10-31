# costar Zephyr module

Zephyr drivers that put costar devices behind Zephyr's own driver APIs, plus
the `costar` shield. `costar_add_zephyr_image()` adds both to every Zephyr
image automatically.

| Compatible | Zephyr API | costar device |
|---|---|---|
| `costar,uart` | UART: polling and interrupt-driven | a channel. Received frames (up to 64 KiB) are read byte by byte; each write is one frame. The interrupt is level-triggered: it stays asserted while enabled RX data or TX readiness remains. |
| `costar,entropy` | entropy (and so `sys_rand_get()`) | a registers device, optional. A pseudo-random generator, so runs are reproducible. It's seeded at boot from its `seed` property XORed with registers 0 (low 32 bits) and 1 (high 32 bits) of the device named `costar-device` (default `entropy`). The shield declares it and makes it `zephyr,entropy`. Give machines that run the same image different seeds. |
| `costar,flash` | flash, with page layout | a memory. Its size is the flash size; erased bytes read 0xFF. Like NOR, only erased bytes can be written (`-EIO` otherwise). |

Declare devices in the application's overlay. The names and IRQ lines must
match the board in the scenario. On native_sim, lines 0–2 are reserved, so
use 3–31.

```dts
/ {
	costar_uart: costar-uart {
		compatible = "costar,uart";
		costar-device = "uart";   /* the scenario's device name */
		irq = <5>;                /* the scenario's irq */
	};
	costar_flash: costar-flash {
		compatible = "costar,flash";
		costar-device = "flash";
		erase-block-size = <256>;
	};
};
```

## The `costar` shield

The shield disables native_sim devices whose host side lives in Zephyr's own
runner, which costar replaces:
- PTY UARTs;
- file-backed flash and EEPROM;
- the host RNG (replaced by `costar,entropy`) and counter;
- audio, SDL, USB, Bluetooth user channel, TAP.

It also clears the chosen nodes that pointed at them. printk goes to costar's
log.

To put the console on a costar UART, pass a fragment as `EXTRA_CONF_FILE`
(the shield's `costar.conf` is merged after `prj.conf`, so `prj.conf` can't
override it). For example, add `ZEPHYR_ARGS -DEXTRA_CONF_FILE=console.conf` to
`costar_add_zephyr_image()`:

```
CONFIG_UART_CONSOLE=y
# native_sim always enables its own console, and it initializes at 99,
# after the UART console (60), taking over printk.  Initialize it first.
CONFIG_POSIX_ARCH_CONSOLE_INIT_PRIORITY=59
```

Then choose the UART:

```dts
/ { chosen { zephyr,console = &costar_uart; }; };
```

Pure software emulators (I2C, SPI, GPIO, ADC and RTC emulators, CAN loopback)
remain available.
