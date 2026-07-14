#ifndef SIM_ABI_H
#define SIM_ABI_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Task entry point ──────────────────────────────────────────────── */

/** Signature of a simulated task entry function. */
typedef void (*sim_task_entry_fn)(void *arg);

/* ── Yield / resume reasons ────────────────────────────────────────── */

typedef enum sim_yield_reason {
    SIM_YIELD_COOPERATIVE = 0,
    SIM_YIELD_RTOS_PORT   = 1,
    SIM_YIELD_BLOCKED     = 2,
    SIM_YIELD_SLEEP       = 3,
    SIM_YIELD_IO          = 4,
    SIM_YIELD_TASK_EXIT   = 5,
} sim_yield_reason_t;

/* ── Opaque handles ────────────────────────────────────────────────── */

/** Opaque task handle returned by sim_create_task. */
typedef uintptr_t sim_task_handle_t;

/* ── Virtual time ──────────────────────────────────────────────────── */

/** Return the current virtual time in ticks. */
uint64_t sim_now_ticks(void);

/* ── Per-machine instance state (Stage B1) ─────────────────────────── */

/**
 * Get or create per-machine instance-state storage for `key`.
 *
 * The first call for a key allocates zeroed storage of `size` bytes with the
 * given `alignment`, owned by the active machine and stable for its lifetime.
 * Later calls with the same key must pass an identical size/alignment; a
 * mismatch, a zero size, or no active machine returns NULL. Restart drops all
 * regions.
 */
void *sim_instance_state(uint32_t key, uint32_t size, uint32_t alignment);

/* ── Task lifecycle ────────────────────────────────────────────────── */

/**
 * Register a new simulated task with the simulator.
 *
 * The task does NOT start running until sim_start_scheduler() is called.
 *
 * @param name                  Human-readable task name (must be a string
 *                              literal or permanently allocated).
 * @param entry                 C function to execute as the task body.
 * @param arg                   Argument passed to the task entry.
 * @param requested_stack_words Stack depth requested by the RTOS (in
 *                              words).  The simulator may allocate a
 *                              larger host stack internally.
 * @param priority              RTOS task priority (0 = lowest).
 * @return An opaque task handle, or 0 on failure.
 */
sim_task_handle_t sim_create_task(
    const char *name,
    sim_task_entry_fn entry,
    void *arg,
    uint32_t requested_stack_words,
    uint32_t priority
);

/**
 * Start the simulator scheduler.
 *
 * This function never returns in a running simulation; control stays
 * inside the Rust event loop until the simulation terminates.
 */
void sim_start_scheduler(void);

/**
 * Yield the currently executing task.
 *
 * Must only be called from within a running task.  If no task is
 * active the call is recorded as a fatal error.
 */
void sim_port_yield(void);

/**
 * Mark the current task as exited.
 *
 * The task will not be rescheduled after this call.
 */
void sim_task_exit(void);

/**
 * Record that a task has been deleted by the RTOS kernel.
 *
 * Called from the traceTASK_DELETE hook during vTaskDelete.  Pushes
 * the task ID onto a deferred-deletion list.  After the current fiber
 * yields, the Rust scheduler marks the task's fiber as Exited.
 *
 * Safe to call from any context (inside or outside a fiber).
 */
void sim_task_deleted(uint64_t task_id);

/**
 * Suspend the current task until the given absolute virtual time.
 *
 * Must only be called from within a running task.
 * The scheduler will not resume this task before `until_ticks`.
 */
void sim_task_delay_until(uint64_t until_ticks);

/* ── Scheduler control (called by Rust) ─────────────────────────────── */

/**
 * Port hook: called by traceTASK_CREATE after FreeRTOS initialises a
 * new TCB.  Creates the corresponding Rust fiber and stores the handle
 * in the TCB.  The parameter is actually a `TCB_t *` (tskTaskControlBlock)
 * but we use void* here to avoid requiring the full struct definition.
 */
void sim_port_task_created(void *pxNewTCB);

/** Register a TCB mapping for sim_set_current_task_by_id. */
void sim_bridge_register(uint64_t task_id, void *tcb);

/** Record a TCB for deferred fiber creation. */
void sim_bridge_add_pending_tcb(void *tcb);

/** Look up the Rust task_id for a given TCB pointer.  Returns 0 if not found. */
uint64_t sim_bridge_find_task_id(void *tcb);

/** Create Rust fibers for all pending TCBs.  Returns count created. */
uint32_t sim_bridge_create_pending_fibers(void);

/**
 * Set the currently-executing TCB by Rust task id.
 *
 * Called by the Rust scheduler before resuming a fiber so that
 * the C kernel's pxCurrentTCB is correct when vTaskDelay / taskYIELD
 * are called.
 */
void sim_set_current_task_by_id(uint64_t task_id);

/**
 * Advance the RTOS tick count by one and move any expired delayed
 * tasks back onto the ready list.
 *
 * Called by the Rust scheduler when virtual time crosses a tick
 * boundary.  Returns the number of tasks woken.
 */
uint32_t sim_tick_advance(void);

/**
 * Batch-advance the tick count by `count` ticks.
 *
 * Semantically equivalent to calling sim_tick_advance() `count` times,
 * but with a single C↔Rust crossing.  Returns the total number of
 * context-switch requests signalled during the batch.
 */
uint32_t sim_advance_ticks(uint32_t count);

/* ── Critical sections ─────────────────────────────────────────────── */

/** Enter a virtual critical section (nesting counter). */
void sim_enter_critical(void);

/** Exit a virtual critical section.  Deferred interrupts are delivered
 *  when nesting reaches zero. */
void sim_exit_critical(void);

/* ── Trace helpers ──────────────────────────────────────────────────── */

/** Record a u32 data point in the simulator trace. */
void sim_trace_u32(const char *label, uint32_t value);

/** Register a human-readable symbol name for a task by its opaque handle.
 *  The name is recorded as a TaskCreated trace event for post-mortem analysis. */
void sim_register_symbol(uint64_t task_id, const char *name);

/* ── Interrupt controller ──────────────────────────────────────────── */

/** Raise a virtual interrupt (adds to pending set). */
void sim_irq_raise(uint32_t irq);

/** Clear a pending virtual interrupt (acknowledge). */
void sim_irq_clear(uint32_t irq);

/** Return the lowest pending IRQ number, or UINT32_MAX if none. */
uint32_t sim_irq_pending(void);

/** Deliver all pending interrupts. Returns count delivered. */
uint32_t sim_irq_deliver_pending(uint64_t now);

/* ── Virtual UART ──────────────────────────────────────────────────── */

/** Write bytes to a virtual UART. Returns bytes written. */
uint32_t sim_uart_write(uint32_t id, const uint8_t *data, uint32_t len);

/* ── Virtual timer ──────────────────────────────────────────────────── */

/** Arm a virtual timer to fire after `delay_ticks` from now. */
void sim_timer_arm(uint32_t id, uint64_t delay_ticks);

/** Disarm a virtual timer. */
void sim_timer_disarm(uint32_t id);

/* ── GPIO ───────────────────────────────────────────────────────────── */

/**
 * Set a GPIO pin state.
 * Returns the IRQ number if change triggered an interrupt, or UINT32_MAX.
 */
uint32_t sim_gpio_set(uint32_t id, uint32_t pin, uint32_t state);

/* ── Virtual I2C ─────────────────────────────────────────────────────── */

/** Write bytes to an I2C target.  Returns bytes written. */
uint32_t sim_i2c_write(uint32_t id, const uint8_t *data, uint32_t len);

/** Read bytes from an I2C target into a caller-provided buffer.
 *  Returns bytes read.  RX buffer must be pre-populated via sim_i2c_inject_rx. */
uint32_t sim_i2c_read(uint32_t id, uint8_t *buf, uint32_t len);

/** Combined I2C write-then-read (repeated start). Returns bytes read. */
uint32_t sim_i2c_write_read(uint32_t id, const uint8_t *tx_data, uint32_t tx_len,
                            uint8_t *rx_buf, uint32_t rx_len);

/** Set the I2C target address.  ten_bit=0 for 7-bit, 1 for 10-bit. */
void sim_i2c_set_address(uint32_t id, uint16_t address, uint32_t ten_bit);

/** Check whether the last I2C operation received a NACK.
 *  Returns 1 if NACK was received, 0 otherwise. */
uint32_t sim_i2c_get_nack(uint32_t id);

/** Inject bytes into the I2C RX buffer (simulates target device response). */
void sim_i2c_inject_rx(uint32_t id, const uint8_t *data, uint32_t len);

/* ── Virtual SPI ─────────────────────────────────────────────────────── */

/** Full-duplex SPI transfer.  Returns bytes received. */
uint32_t sim_spi_transfer(uint32_t id, const uint8_t *tx_data, uint32_t tx_len,
                          uint8_t *rx_buf, uint32_t rx_len);

/** Set SPI configuration: mode (0-3), speed (Hz), word size (8 or 16).
 *  Returns 0 on success, 1 for invalid mode, 2 for invalid word size. */
uint32_t sim_spi_set_config(uint32_t id, uint32_t mode, uint32_t speed_hz,
                            uint32_t word_size);

/** Set SPI chip select.  Returns 0 on success, 1 if controller not found. */
uint32_t sim_spi_set_cs(uint32_t id, uint32_t active);

/** Inject bytes into the SPI RX buffer (simulates peripheral response). */
void sim_spi_inject_rx(uint32_t id, const uint8_t *data, uint32_t len);

/* ── Virtual CAN ─────────────────────────────────────────────────────── */

/** Send a CAN frame.  Returns 0 on success, 1 on failure. */
uint32_t sim_can_send(uint32_t ctrl_id, uint32_t can_id,
                      const uint8_t *data, uint32_t len,
                      uint32_t is_ext, uint32_t is_remote);

/** Receive the oldest CAN frame from the RX queue.
 *  Returns the DLC of the received frame, or 0 if none available.
 *  Writes CAN ID into *can_id_out, extended flag into *is_ext_out,
 *  remote flag into *is_remote_out, and payload into buf. */
uint32_t sim_can_recv(uint32_t ctrl_id,
                      uint8_t *buf, uint32_t buf_len,
                      uint32_t *can_id_out,
                      uint32_t *is_ext_out,
                      uint32_t *is_remote_out);

/** Inject a CAN frame into the RX queue (simulates external node). */
void sim_can_inject_rx(uint32_t ctrl_id, uint32_t can_id,
                       const uint8_t *data, uint32_t len, uint32_t is_ext);

/** Enable (1) or disable (0) loopback mode.  Returns 0 on success. */
uint32_t sim_can_set_loopback(uint32_t ctrl_id, uint32_t enable);

/** Get the CAN error state: 0=Active, 1=Warning, 2=Passive, 3=BusOff. */
uint32_t sim_can_get_error(uint32_t ctrl_id);

/* ── Virtual ADC ────────────────────────────────────────────────────── */

/**
 * Read the ADC value for a specific channel.
 *
 * Returns the pre-injected reading for the given channel of the ADC
 * identified by `id`.  If the ADC is not registered, returns 0.
 */
uint16_t sim_adc_read(uint32_t id, uint32_t channel);

/**
 * Inject a reading for a specific ADC channel.
 *
 * Sets the ADC reading for the given channel so that subsequent
 * `sim_adc_read` calls for that channel return `value`.
 * If the ADC is not registered, this is a no-op.
 */
void sim_adc_inject_reading(uint32_t id, uint32_t channel, uint16_t value);

/**
 * Set the ADC resolution in bits.
 *
 * Valid values: 8, 10, 12, 16.  Invalid values are silently ignored.
 * If the ADC is not registered, this is a no-op.
 */
void sim_adc_set_resolution(uint32_t id, uint32_t bits);

/* ── Virtual Temperature Sensor ─────────────────────────────────────── */

/**
 * Read the current temperature from a virtual temperature sensor.
 *
 * Returns the temperature in millidegrees Celsius (m°C).
 * Default is 25000 (= 25.0 °C).  Returns 0 if the sensor is not
 * registered.
 */
int32_t sim_temp_read(uint32_t id);

/**
 * Set the temperature of a virtual temperature sensor.
 *
 * The value is in millidegrees Celsius (m°C):
 *   - 25000 → 25.000 °C
 *   - -10000 → -10.000 °C
 *
 * If the sensor is not registered, this is a no-op.
 */
void sim_temp_set_value(uint32_t id, int32_t milli_c);

/* ── Virtual EEPROM ──────────────────────────────────────────────────── */

/** Read a byte from a virtual EEPROM.  Returns the byte (0–255), or
 *  UINT32_MAX if the device is not found or addr is out of bounds. */
uint32_t sim_eeprom_read(uint32_t id, uint32_t addr);

/** Write a byte to a virtual EEPROM.  Returns 0 on success, 1 if the
 *  device is not found or addr is out of bounds. */
uint32_t sim_eeprom_write(uint32_t id, uint32_t addr, uint32_t byte);

/** Return the size of a virtual EEPROM in bytes, or 0 if not found. */
uint32_t sim_eeprom_size(uint32_t id);

/* ── Virtual Flash ────────────────────────────────────────────────────── */

/** Read a byte from a virtual Flash device.  Returns the byte (0–255),
 *  or UINT32_MAX if the device is not found or addr is out of bounds. */
uint32_t sim_flash_read(uint32_t id, uint32_t addr);

/** Write data to a virtual Flash page at the given offset.
 *  Writes only succeed to erased (0xFF) locations.
 *  Returns the number of bytes written, or 0 on failure. */
uint32_t sim_flash_write(uint32_t id, uint32_t page, uint32_t offset,
                         const uint8_t *data, uint32_t len);

/** Erase a virtual Flash page (fills with 0xFF).
 *  Returns 0 on success, 1 on failure. */
uint32_t sim_flash_erase(uint32_t id, uint32_t page);

/* ── Fault injection ─────────────────────────────────────────────── */

/** Inject an I2C NACK on the next read. */
void sim_fault_inject_i2c_nack(void);

/** Inject an SPI data/CRC error on the next transfer. */
void sim_fault_inject_spi_error(void);

/** Inject a CAN bus error on the next send. */
void sim_fault_inject_can_error(void);

/** Clear all injected faults. */
void sim_fault_clear(void);

/* ── Virtual entropy (Phase 30) ──────────────────────────────────── */

/**
 * Fill a buffer with deterministic pseudo-random bytes.
 *
 * Writes up to `len` bytes into `buf`.  Returns the number of bytes
 * actually written (always `len` on success), or 0 if the entropy
 * source with the given `id` is not registered.
 *
 * The output is deterministic for a given seed — same simulator run
 * with same seed produces identical bytes.
 */
uint32_t sim_entropy_request(uint32_t id, uint8_t *buf, uint32_t len);

/**
 * Reseed the virtual entropy source identified by `id`.
 *
 * Subsequent sim_entropy_request calls produce a different byte
 * sequence for the same device.
 */
void sim_entropy_seed(uint32_t id, uint64_t seed);

/* ── Virtual networking (deterministic) ─────────────────────────────── */

 /** Inject a packet into the network device rx queue. Returns bytes injected. */
 uint32_t sim_net_inject_rx(const uint8_t *data, uint32_t len);

 /** Drain oldest tx packet into buf. Returns bytes written (0 if empty). */
 uint32_t sim_net_drain_tx(uint8_t *buf, uint32_t buf_size);

 /** Check if any rx packets are pending. Returns 1 if yes, 0 if no. */
 uint32_t sim_net_poll(void);

 /* ── Host-connected I/O (interactive mode) ──────────────────────────── */

 /** Register a host file descriptor with the poller. Returns 0 on success. */
 int32_t sim_host_register_fd(int32_t fd);

 /** Deregister a host file descriptor from the poller. Returns 0 on success. */
 int32_t sim_host_deregister_fd(int32_t fd);

 /** Block the current task on a host file descriptor (yields with IoWait). */
 void sim_host_block_on_fd(int32_t fd);

/* ── CPU-bound stall mitigation (budget polling) ──────────────────── */

/**
 * Poll the function-entry budget for the current task.
 *
 * Called from __cyg_profile_func_enter when -finstrument-functions is
 * enabled, and from the SIM_LOOP_POLL() macro for manual loop hooks.
 *
 * Increments an entry counter; if the budget is exceeded, the fiber
 * yields with BudgetExceeded and resets on resume.  file and line
 * identify the call site (may be NULL/0 from the automatic hook).
 *
 * Safe to call from any context (uses thread-local state only).
 */
void sim_budget_poll(const char *file, uint32_t line);

/**
 * Reset the function-entry budget counter for the current task.
 *
 * Call at task startup to clear any residual budget state from
 * a previous task that ran on the same host thread.
 */
void sim_budget_reset(void);

/**
 * Set the budget limit (max function/edge checks before forced yield).
 *
 * Default is 1,000,000.  For Tier 3 edge instrumentation, a much
 * lower value (e.g., 10-100) is recommended because sim_budget_poll
 * is called after every EDGE_CHECK_INTERVAL edges.
 *
 * Safe to call from any context (uses thread-local state only).
 */
void sim_budget_set_limit(uint64_t max_entries);

/* ── Tier 2 loop hook macro ───────────────────────────────────────── */

/**
 * Manual loop poll point for cooperative-fiber stall mitigation.
 *
 * Insert SIM_LOOP_POLL() inside tight loops that do not call any
 * other function.  This gives the budget poller a chance to yield
 * the fiber, preventing infinite-loop hangs in cooperative mode.
 *
 * Equivalent to calling sim_budget_poll(__FILE__, __LINE__) — safe to
 * use in any context (thread-local only, re-entrant safe).
 *
 * Usage:
 *
 *   while (1) {
 *       SIM_LOOP_POLL();
 *       // tight work loop
 *   }
 */
#define SIM_LOOP_POLL() sim_budget_poll(__FILE__, __LINE__)

/* ── Virtual Ethernet ─────────────────────────────────────────────── */

/** Register a virtual Ethernet device with the simulator. */
uint32_t sim_eth_register(uint32_t id, const uint8_t *mac, uint32_t mtu);

/** Send an Ethernet frame from the guest. Returns bytes queued. */
uint32_t sim_eth_send(uint32_t id, const uint8_t *data, uint32_t len);

/** Receive the next Ethernet frame into buf. Returns bytes written. */
uint32_t sim_eth_recv(uint32_t id, uint8_t *buf, uint32_t buf_size);

/** Check if any rx frames are pending for this Ethernet device. */
uint32_t sim_eth_poll(uint32_t id);

/** Register a receive callback (called when frames arrive). */
void sim_eth_on_recv(uint32_t id, void (*callback)(void));

/* ── Virtual Display ───────────────────────────────────────────────── */

/** Initialize a virtual display.
 *  color_mode: 0=RGB565, 1=RGB888, 2=ARGB8888.
 *  Returns 0 on success, 1 on error. */
uint32_t sim_display_init(uint32_t id, uint16_t width, uint16_t height, uint32_t color_mode);

/** Set a single pixel.  Returns 0 on success, 1 if out of bounds. */
uint32_t sim_display_set_pixel(uint32_t id, uint16_t x, uint16_t y, uint32_t color);

/** Fill a rectangle with a solid color.  Returns 0 on success. */
uint32_t sim_display_fill_rect(uint32_t id, uint16_t x, uint16_t y,
                               uint16_t w, uint16_t h, uint32_t color);

/** Draw a bitmap onto the display.  Returns bytes copied. */
uint32_t sim_display_draw_bitmap(uint32_t id, uint16_t x, uint16_t y,
                                 uint16_t w, uint16_t h,
                                 const uint8_t *data, uint32_t data_len);

/** Enable (1) or disable (0) the display. */
void sim_display_enable(uint32_t id, uint32_t enable);

/** Set backlight level (0-100). */
void sim_display_set_backlight(uint32_t id, uint32_t level);

/** Get display width, or 0 if not found. */
uint16_t sim_display_get_width(uint32_t id);

/** Get display height, or 0 if not found. */
uint16_t sim_display_get_height(uint32_t id);

/* ── Virtual Touch Screen ──────────────────────────────────────────── */

/** Initialize a touch screen associated with a display.
 *  Returns 0 on success. */
uint32_t sim_touch_init(uint32_t id, uint32_t display_id);

/** Read the next touch event from the queue.
 *  Returns 1 if an event was read and written to the out params,
 *  0 if the queue is empty.
 *  out_type: 0=Press, 1=Release, 2=Move. */
uint32_t sim_touch_get_event(uint32_t id,
                             uint32_t *out_point_id,
                             uint16_t *out_x,
                             uint16_t *out_y,
                             uint8_t *out_pressure,
                             uint32_t *out_type);

/** Get the number of pending touch events. */
uint32_t sim_touch_pending_count(uint32_t id);

/* ── Virtual block device (filesystem) ───────────────────────────── */

/** Create a new virtual block device. */
uint32_t sim_block_create(uint32_t id, uint32_t page_size,
                          uint32_t page_count, uint8_t erase_value);

/** Read from the block device at an absolute offset.
 *  Writes up to `len` bytes into `buf`. Returns bytes actually read. */
uint32_t sim_block_read(uint32_t id, uint32_t offset,
                        uint8_t *buf, uint32_t len);

/** Write to the block device at an absolute offset.
 *  Target locations must be erased (contain erase_value) before writing.
 *  Returns the number of bytes actually written. */
uint32_t sim_block_write(uint32_t id, uint32_t offset,
                         const uint8_t *data, uint32_t len);

/** Erase the page containing the given absolute offset.
 *  Sets all bytes in that page to the erase_value. */
void sim_block_erase_page(uint32_t id, uint32_t offset);

/** Get geometry of the block device.
 *  Writes page_size and page_count to the output pointers. */
void sim_block_get_geometry(uint32_t id, uint32_t *page_size,
                            uint32_t *page_count);

/** Snapshot the block device to a host file. Returns 0 on success. */
int32_t sim_block_snapshot(uint32_t id, const char *path);

/** Restore a block device from a host file. Returns 0 on success. */
int32_t sim_block_restore(uint32_t id, const char *path);

/* ── Virtual Bluetooth HCI ────────────────────────────────────────── */

/** Register a virtual HCI controller.  Returns the controller ID. */
uint32_t sim_bt_register(uint32_t id);

/** Send an HCI command or ACL data packet from the host to the controller.
 *  `packet_type`: 1=Command, 2=ACL Data, 4=Event. */
void sim_bt_send(uint32_t id, uint8_t packet_type,
                 const uint8_t *data, uint32_t len);

/** Receive the next HCI event or ACL data packet for the host.
 *  Writes packet_type into *packet_type_out and payload into buf.
 *  Returns bytes written (payload only), or 0 if empty. */
uint32_t sim_bt_recv(uint32_t id, uint8_t *packet_type_out,
                     uint8_t *buf, uint32_t buf_size);

/** Inject a scripted HCI event into the controller.
 *  Used for deterministic test scripting. */
void sim_bt_inject_event(uint32_t id, const uint8_t *data, uint32_t len);

/** Register a receive callback (called when events/data arrive for host). */
void sim_bt_on_recv(uint32_t id, void (*callback)(void));

/* ── Peripheral event queue (RTOS-agnostic) ──────────────────────── */

/**
 * Schedule a C callback at the given absolute cycle time.
 *
 * This is the primary mechanism for virtual devices (timers, UART,
 * GPIO) to schedule events on the simulator's event queue.  The
 * callback runs at the specified virtual time and typically calls
 * sim_irq_raise() or sim_trace_u32().
 *
 * Owned by the costar engine, not by any RTOS — works identically
 * for FreeRTOS, Zephyr, and future RTOS ports.
 */
void sim_schedule_event(uint64_t at_cycles, void (*callback)(void));

#ifdef __cplusplus
}
#endif

#endif /* SIM_ABI_H */
