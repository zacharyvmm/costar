# costar Zephyr module

Zephyr drivers that put costar devices behind Zephyr's own driver APIs, plus
the `costar` shield. `costar_add_zephyr_image()` adds both to every Zephyr
image automatically.

| Compatible | Zephyr API | costar device |
|---|---|---|
| `costar,uart` | UART: polling and interrupt-driven | a channel. Received frames (up to 64 KiB) are read byte by byte; each write is one frame. The interrupt is level-triggered: it stays asserted while enabled RX data or TX readiness remains. |
| `costar,entropy` | entropy (and so `sys_rand_get()`) | a registers device, optional. A pseudo-random generator, so runs are reproducible. It's seeded at boot from its `seed` property XORed with registers 0 (low 32 bits) and 1 (high 32 bits) of the device named `costar-device` (default `entropy`). The shield declares it and makes it `zephyr,entropy`. Give machines that run the same image different seeds. |
| `costar,gpio` | GPIO, with edge-triggered interrupts | a registers device. Register 0 holds the input levels, one bit per pin, set by the host or a plant. The driver writes the output pins' levels to register 1 (traced as `reg`). Output pins read their own level and disconnected pins read 0. When the host changes a register, matching edges on input pins run their callbacks; `GPIO_INT_MODE_ENABLE_ONLY`/`DISABLE_ONLY` are supported. Edges are found by comparing levels when the ISR runs, so a pulse that starts and ends while interrupts are locked across virtual time is missed. Level-triggered interrupts, pulls and open drain return `-ENOTSUP`. The `irq` property is optional. |
| `costar,can` | CAN, classic and FD (with `CONFIG_CAN_FD_MODE`), normal and loopback modes | a channel. Each CAN frame is one channel frame: a 4-byte big-endian ID with flags in the top bits (`0x80000000` extended, `0x40000000` remote, `0x20000000` FD), the DLC (plus `0x80` for bit rate switch), then the data. A wire fault's `match_hex` can therefore select frames by ID. Use a `shared_medium` wire with the bus's `bits_per_second`. A send fills one of eight TX mailboxes; the device's ISR transmits it (and in loopback mode receives it) and runs its TX callback, at once unless interrupts are locked. There are no acknowledgements, arbitration or bus errors, and a started controller is always error-active. Callbacks run in the ISR and don't nest, except that `can_stop()` runs the TX callbacks of the frames it aborts (with `-ENETDOWN`) itself. FD frames are received only in FD mode. Up to `CONFIG_COSTAR_CAN_MAX_FILTERS` (16) receive filters. |
| `costar,wdt` | watchdog (`wdt_install_timeout`, `wdt_setup`, `wdt_feed`, `wdt_disable`) | a watchdog device (`kind = "watchdog"`). One timeout, in whole milliseconds; no window (`-EINVAL`), no callback and no `WDT_FLAG_RESET_NONE` (`-ENOTSUP`), and no `WDT_OPT_PAUSE_IN_SLEEP` (`-ENOTSUP`). When it expires, costar reboots the machine. The shield declares it, as alias `watchdog0`, on the device named `costar-device` (default `wdt`); without that device the calls return `-ENODEV`. |
| `costar,i2c` | I2C controller: `i2c_transfer()`, `i2c_configure()`, `i2c_get_config()` | a channel. Each transfer is one request frame, answered by one reply frame ([`include/costar_i2c.h`](../../../include/costar_i2c.h)); on the host, scenario device models (`i2c-registers`, `i2c-eeprom`) answer for the bus's parts. The calling thread blocks until the reply's IRQ, or until the transfer timeout (`zephyr,transfer-timeout-ms`, else `CONFIG_I2C_TRANSFER_TIMEOUT_MS`; 0 waits for ever). A NACK, a timeout or a malformed reply is `-EIO`; a late reply is dropped. The bus speed is only reported: the wire's `bits_per_second` sets the timing. Controller mode only; no target mode, RTIO or callbacks. A transfer must fit in `CONFIG_COSTAR_I2C_FRAME_SIZE` (4096 bytes; else `-EINVAL`). The shield makes native_sim's `i2c0` one, on the channel `i2c0` with IRQ 12. |
| `costar,flash` | flash, with page layout | a memory. Its size is the flash size; erased bytes read 0xFF. Like NOR, only erased bytes can be written (`-EIO` otherwise). |

Declare devices in the application's overlay (the shield already declares
`i2c0`; put parts on it, or change its `costar-device` and `irq`). The names and IRQ lines must
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
	costar_can: costar-can {
		compatible = "costar,can";
		costar-device = "can";
		irq = <7>;
	};
	costar_gpio: costar-gpio {
		compatible = "costar,gpio";
		costar-device = "gpio";
		irq = <6>;
		gpio-controller;
		#gpio-cells = <2>;
	};
};
```

## Thread stack sizes

`costar_thread.c` (`CONFIG_COSTAR_THREAD_INFO`, on by default) isn't a driver.
costar's native_sim runner calls it when each thread starts, to learn the
thread's declared stack size, so that costar can check the thread stays
within it (see ARCHITECTURE.md §2). It turns on `CONFIG_THREAD_STACK_INFO`.

## The `costar` shield

The shield disables native_sim devices whose host side lives in Zephyr's own
runner, which costar replaces:
- PTY UARTs;
- file-backed flash and EEPROM;
- the host RNG (replaced by `costar,entropy`) and counter;
- audio, SDL, USB, Bluetooth user channel, TAP.

It also clears the chosen nodes that pointed at them. printk goes to costar's
log. It declares `costar,entropy` (as `zephyr,entropy`) and `costar,wdt` (as
alias `watchdog0`).

It turns native_sim's I2C emulator controller, `i2c0`, into a `costar,i2c`
controller on the board's channel `i2c0` (IRQ 12), so an application puts
its parts on `&i2c0`, with Zephyr's own drivers, as it would on a real
board:

```dts
&i2c0 {
	tmp112@48 {
		compatible = "ti,tmp112";
		reg = <0x48>;
	};
};
```

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

Pure software emulators (SPI, GPIO, ADC and RTC emulators, CAN loopback)
remain available. For Zephyr's I2C emulators, make `i2c0` one again in the
application's overlay (`compatible = "zephyr,i2c-emul-controller";`).
