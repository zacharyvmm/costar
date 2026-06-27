// Virtual flash driver for Zephyr. Implements Zephyr's flash driver API
// using costar's FlatMemoryStore backend.

#include <stdint.h>
#include <stddef.h>

// Forward-declare the C ABI functions (will be resolved at link time by sim-ffi).
extern uint32_t sim_block_create(uint32_t id, uint32_t page_size,
                                  uint32_t page_count, uint8_t erase_value);
extern uint32_t sim_block_read(uint32_t id, uint32_t offset,
                                uint8_t *buf, uint32_t len);
extern uint32_t sim_block_write(uint32_t id, uint32_t offset,
                                 const uint8_t *data, uint32_t len);
extern void     sim_block_erase_page(uint32_t id, uint32_t offset);
extern void     sim_block_get_geometry(uint32_t id, uint32_t *page_size,
                                         uint32_t *page_count);

// Driver state
static uint32_t sim_flash_id = 0;
static int      sim_flash_initialized = 0;

// Called by firmware/app init to register the flash device.
void sim_flash_init(uint32_t id, uint32_t page_size, uint32_t page_count)
{
    sim_block_create(id, page_size, page_count, 0xFF);
    sim_flash_id = id;
    sim_flash_initialized = 1;
}

// Zephyr flash driver: flash_read(dev, offset, data, len)
int flash_sim_read(void *dev, uint32_t offset, void *data, uint32_t len)
{
    (void)dev;
    if (!sim_flash_initialized) return -1;
    sim_block_read(sim_flash_id, offset, (uint8_t *)data, len);
    return 0;
}

// Zephyr flash driver: flash_write(dev, offset, data, len)
int flash_sim_write(void *dev, uint32_t offset, const void *data, uint32_t len)
{
    (void)dev;
    if (!sim_flash_initialized) return -1;
    uint32_t written = sim_block_write(sim_flash_id, offset,
                                        (const uint8_t *)data, len);
    return (written == len) ? 0 : -1;
}

// Zephyr flash driver: flash_erase(dev, offset, size)
int flash_sim_erase(void *dev, uint32_t offset, uint32_t size)
{
    (void)dev;
    if (!sim_flash_initialized) return -1;
    uint32_t page_size, page_count;
    sim_block_get_geometry(sim_flash_id, &page_size, &page_count);
    uint32_t end = offset + size;
    while (offset < end) {
        sim_block_erase_page(sim_flash_id, offset);
        offset += page_size;
    }
    return 0;
}

// Zephyr flash driver: get page info for offset
void flash_sim_get_page_info(void *dev, uint32_t offset,
                              uint32_t *page_size_out, uint32_t *page_count_out)
{
    (void)dev; (void)offset;
    sim_block_get_geometry(sim_flash_id, page_size_out, page_count_out);
}
