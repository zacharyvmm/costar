// Virtual block device driver for FreeRTOS+FAT.
// Implements the FF_Disk_t interface using costar's FlatMemoryStore backend.

#include <stdint.h>
#include <stddef.h>
#include <string.h>

// Forward-declare the C ABI functions.
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
static uint32_t sim_block_disk_id = 0;
static int      sim_block_initialized = 0;

void sim_block_init(uint32_t id, uint32_t page_size, uint32_t page_count)
{
    sim_block_create(id, page_size, page_count, 0xFF);
    sim_block_disk_id = id;
    sim_block_initialized = 1;
}

// FreeRTOS+FAT: FF_Read(pxDisk, ulSector, pvBuffer, ulCount)
int sim_block_read_sectors(void *disk, uint32_t sector, void *buf, uint32_t count)
{
    (void)disk;
    if (!sim_block_initialized) return -1;
    uint32_t page_size, page_count;
    sim_block_get_geometry(sim_block_disk_id, &page_size, &page_count);
    uint32_t sector_size = page_size;  // 1 sector = 1 page for simplicity
    uint32_t offset = sector * sector_size;
    uint32_t len = count * sector_size;
    uint32_t read = sim_block_read(sim_block_disk_id, offset, (uint8_t *)buf, len);
    return (read == len) ? 0 : -1;
}

// FreeRTOS+FAT: FF_Write(pxDisk, ulSector, pvBuffer, ulCount)
int sim_block_write_sectors(void *disk, uint32_t sector, const void *buf, uint32_t count)
{
    (void)disk;
    if (!sim_block_initialized) return -1;
    uint32_t page_size, page_count;
    sim_block_get_geometry(sim_block_disk_id, &page_size, &page_count);
    uint32_t sector_size = page_size;
    uint32_t offset = sector * sector_size;
    uint32_t len = count * sector_size;
    uint32_t written = sim_block_write(sim_block_disk_id, offset,
                                        (const uint8_t *)buf, len);
    return (written == len) ? 0 : -1;
}

// FreeRTOS+FAT: FF_GetCapacity(pxDisk) -> sector count
uint32_t sim_block_get_capacity(void *disk)
{
    (void)disk;
    if (!sim_block_initialized) return 0;
    uint32_t page_size, page_count;
    sim_block_get_geometry(sim_block_disk_id, &page_size, &page_count);
    return page_count;  // 1 sector = 1 page
}

// FreeRTOS+FAT: FF_GetStatus(pxDisk) -> always present
int sim_block_get_status(void *disk)
{
    (void)disk;
    return sim_block_initialized ? 1 : 0;
}

// FreeRTOS+FAT: FF_Init(pxDisk) -> initialise
int sim_block_ff_init(void *disk)
{
    (void)disk;
    return sim_block_initialized ? 0 : -1;
}
