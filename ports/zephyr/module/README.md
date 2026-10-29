# costar Zephyr module

Zephyr drivers that put costar devices behind Zephyr's own driver APIs, plus
the `costar` shield. `costar_add_zephyr_image()` adds both to every Zephyr
image automatically.

| Compatible | Zephyr API | costar device |
|---|---|---|
| `costar,uart` | UART: polling and interrupt-driven | a channel. Received frames are read byte by byte; each write is one frame. |
| `costar,flash` | flash, with page layout | a memory. Its size is the flash size; erased bytes read 0xFF. |

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
- the host RNG and counter;
- audio, SDL, USB, Bluetooth user channel, TAP.

It also clears the chosen nodes that pointed at them. printk goes to costar's
log.

To put the console on a costar UART, set `CONFIG_UART_CONSOLE=y` and
`CONFIG_POSIX_ARCH_CONSOLE=n`, and choose the UART:

```dts
/ { chosen { zephyr,console = &costar_uart; }; };
```

Pure software emulators (I2C, SPI, GPIO, ADC and RTC emulators, CAN loopback)
remain available.
