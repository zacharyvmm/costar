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
| `costar,spi` | SPI controller: `spi_transceive()`, `spi_release()` | a channel. A transceive is a transaction on the chip select of the device's `reg`: frames, each answered by a reply frame with the bytes received ([`include/costar_spi.h`](../../../include/costar_spi.h)); on the host, scenario device models (`spi-nor`) answer for the devices. It clocks max(tx, rx) bytes, sending `0x00` past the tx buffers and for NULL ones. Transfers longer than `CONFIG_COSTAR_SPI_FRAME_SIZE` (4096 bytes, with a 7-byte header) are split into several frames within the transaction. The calling thread blocks until each reply's IRQ, or `CONFIG_SPI_COMPLETION_TIMEOUT_TOLERANCE` ms (`-ETIMEDOUT`); a late reply is dropped. `SPI_LOCK_ON` keeps the bus for one `spi_config` until `spi_release()`, and `SPI_HOLD_ON_CS` keeps its chip select active until the next transceive without it or `spi_release()`. A failed transceive releases its chip select at once (the far side may have it active, if only a reply was lost), with a frame of its own; if that fails too, `spi_release()` tries again and returns its error. Controller mode, 8-bit words, MSB first, single line; chip selects are native (`cs-gpios` returns `-ENOTSUP`); no async or RTIO. The frequency is only reported: the wire's `bits_per_second` sets the timing (give it one: a driver that polls a busy flash without sleeping otherwise polls without time passing, and stalls). The shield makes native_sim's `spi0` one, on the channel `spi0` with IRQ 13. |
| `costar,flash` | flash, with page layout | a memory. Its size is the flash size; erased bytes read 0xFF. Like NOR, only erased bytes can be written (`-EIO` otherwise). |
| `costar,adc` | ADC, with `adc_read_async()` (`CONFIG_ADC_ASYNC`) and sequence options | a registers device. Register n holds channel n's raw sample, set by the host or a plant. A conversion takes no virtual time. Samples are 16 bits wide (resolutions 1–16); a register above full scale reads as full scale. The register is the converter's output, so gain, reference and acquisition time are accepted and don't change it; oversampling averages equal samples and calibration does nothing. Differential channels return `-ENOTSUP`. Extra samplings, callbacks and intervals work as on any Zephyr ADC (Zephyr's `adc_context.h`): with an interval, a kernel timer starts each sampling, which reads the registers at its own virtual time. Properties: `nchannels` (default 8, at most 32), `ref-internal-mv` (default 3300). |
| `costar,pwm` | PWM (`pwm_set_cycles`, `pwm_get_cycles_per_sec`) | a registers device. Channel c's period and pulse, in cycles, are registers 3c and 3c + 1, and register 3c + 2 holds its flags (1: `PWM_POLARITY_INVERTED`). The driver writes them when they change (traced as `reg`), so plants read the duty cycle as pulse / period. The clock is `clock-frequency` (default 1 GHz, so cycles are nanoseconds); `channels` defaults to 8. At boot every channel is set back to 0, as a reset peripheral stops its outputs. Other flags return `-ENOTSUP`; there's no capture. |
| `costar,eeprom` | EEPROM | a memory. Its size is the EEPROM's. Unlike flash, any byte can be written at any time, with no erase; bytes never written read 0xFF. `read-only` makes writes fail with `-EACCES`. |
| `costar,eth` | Ethernet (`ethernet_api`), for Zephyr's own network stack | a channel. Each channel frame is one Ethernet frame, without preamble, FCS or padding (as on a TAP interface, so a `tap` bridge carries it unchanged). Sends go out at once; the wire models latency and speed. The ISR queues arriving frames for the driver's RX thread, which hands them to the stack (`net_recv_data()`, which may process them in that thread, as with `CONFIG_NET_TC_RX_COUNT=0`; `CONFIG_COSTAR_ETH_RX_THREAD_STACK_SIZE`, default `CONFIG_NET_RX_STACK_SIZE`, and `CONFIG_COSTAR_ETH_RX_THREAD_PRIORITY`, cooperative, default 2); frames that arrive while the interface is down, or that the stack has no buffer for, are dropped. The link is up from the start. The MAC address is `local-mac-address`, `zephyr,random-mac-address` or an NVMEM cell; with none, it's random as with `zephyr,random-mac-address`, which `costar,entropy` makes reproducible, so give machines that run the same image different entropy seeds. |

Declare devices in the application's overlay (the shield already declares
`i2c0` and `spi0`; put parts on them, or change their `costar-device` and
`irq`). The names and IRQ lines must
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
	costar_eth: costar-eth {
		compatible = "costar,eth";
		costar-device = "eth";
		irq = <8>;
		local-mac-address = [02 00 5e 00 53 01];   /* optional */
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

The shield already declares an ADC, a PWM controller and an EEPROM on
devices named `adc`, `pwm` and `eeprom`, disabled. Enable the ones the
application uses, and add ADC channels as for any Zephyr ADC:

```dts
&costar_adc {
	status = "okay";
	channel@0 {
		reg = <0>;
		zephyr,gain = "ADC_GAIN_1";
		zephyr,reference = "ADC_REF_INTERNAL";
		zephyr,acquisition-time = <ADC_ACQ_TIME_DEFAULT>;
		zephyr,resolution = <12>;
	};
};
&costar_pwm { status = "okay"; };      /* #pwm-cells = <3>: channel, period, flags */
&costar_eeprom { status = "okay"; };   /* also the eeprom-0 alias */

/ {
	zephyr,user {
		io-channels = <&costar_adc 0>;               /* ADC_DT_SPEC_GET() */
		pwms = <&costar_pwm 0 PWM_MSEC(1) PWM_POLARITY_NORMAL>;   /* PWM_DT_SPEC_GET() */
	};
};
```

## Networking

With a `costar,eth` node, Zephyr's own network stack runs on costar
unchanged: `CONFIG_NETWORKING`, `CONFIG_NET_L2_ETHERNET` and the protocols
and sockets the application uses. Its threads and timers run on virtual
time, and with `costar,entropy` its random numbers (MAC addresses, ports,
IDs) are the same every run. Don't enable `CONFIG_NET_NATIVE_OFFLOADED_SOCKETS`: those
are the host's sockets, through Zephyr's own runner. See
[`firmware/zephyr_net`](../../../firmware/zephyr_net) and
[`examples/zephyr/tap_bridge.toml`](../../../examples/zephyr/tap_bridge.toml),
which bridges a machine to a Linux TAP interface.

## Thread stack sizes

`costar_thread.c` (`CONFIG_COSTAR_THREAD_INFO`, on by default) isn't a driver.
costar's native_sim runner calls it when each thread starts, to learn the
thread's declared stack size, so that costar can check the thread stays
within it (see ARCHITECTURE.md §2). It turns on `CONFIG_THREAD_STACK_INFO`.

## The `costar` shield

The shield disables native_sim devices whose host side lives in Zephyr's own
runner, which costar replaces:
- PTY UARTs;
- file-backed flash and EEPROM (the `eeprom-0` alias is `costar_eeprom`'s);
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

Likewise, its SPI emulator controller, `spi0`, becomes a `costar,spi`
controller on the board's channel `spi0` (IRQ 13). A device's `reg` is its
chip select:

```dts
&spi0 {
	flash@0 {
		compatible = "jedec,spi-nor";
		reg = <0>;
		spi-max-frequency = <8000000>;
		size = <0x800000>; /* bits */
		jedec-id = [ef 40 14];
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

The Zephyr shell needs no more than `CONFIG_SHELL=y` and a chosen UART, as
in [`firmware/zephyr_shell`](../../../firmware/zephyr_shell); type into it with
`costar run --console`:

```dts
/ { chosen { zephyr,shell-uart = &costar_uart; }; };
```

Pure software emulators (GPIO, ADC and RTC emulators, CAN loopback) remain
available. For Zephyr's I2C or SPI emulators, make `i2c0` or `spi0` one
again in the application's overlay (`compatible =
"zephyr,i2c-emul-controller";` or `"zephyr,spi-emul-controller"`).
