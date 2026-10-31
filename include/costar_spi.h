/*
 * costar_spi.h — SPI transfers over a costar channel device.
 *
 * A SPI controller is a channel.  Each request frame clocks bytes through
 * one chip select's device, full duplex: the controller's bytes out (MOSI)
 * and the device's bytes back (MISO), returned in one reply frame.  A
 * transaction (chip select active, then released) is one frame or several:
 * flags say where it starts and ends, so a long transfer can be split, and
 * chip select can be held between transfers.  On the host, device models
 * answer (scenario [[model]]s: spi-nor); any other endpoint can too.
 * Zephyr's costar,spi driver speaks this; FreeRTOS firmware can use
 * costar_chan_send/recv directly.
 *
 * Request:
 *   byte 0     the chip select
 *   byte 1     a tag, echoed by the reply (match replies to requests)
 *   byte 2     flags: COSTAR_SPI_SELECT, COSTAR_SPI_DESELECT
 *   bytes 3-4  length: the bytes to clock, big-endian
 *   bytes 5-6  how many of the bytes received to return (at most length),
 *              big-endian
 *   bytes 7-   the bytes to send (at most length; the rest are 0x00)
 *
 * Reply:
 *   byte 0     the chip select, as in the request
 *   byte 1     the request's tag
 *   byte 2     status: COSTAR_SPI_OK, or COSTAR_SPI_BAD_REQUEST
 *   bytes 3-   the first bytes received, as many as asked for
 *
 * Both start with the chip select, so a wire fault's match_hex can pick one
 * device's traffic.  Nothing on a chip select reads as 0xFF (a pulled-up
 * MISO line).  A frame without SELECT continues the transaction open on its
 * chip select (if another one is open, it is released first, and if none is,
 * the frame starts one).  A frame with SELECT releases any open transaction
 * first, its own chip select's too: chip select goes inactive and active
 * again, and the device acts on the command it had (a page program
 * writes).  So a controller that gives up on a transaction should release
 * it (DESELECT, no bytes) rather than leave that to the next one.
 */
#ifndef COSTAR_SPI_H
#define COSTAR_SPI_H

/* Request flags. */
#define COSTAR_SPI_SELECT 0x01u   /* chip select goes active first: a new transaction */
#define COSTAR_SPI_DESELECT 0x02u /* chip select is released after the bytes */

/* Reply status. */
#define COSTAR_SPI_OK 0u
#define COSTAR_SPI_BAD_REQUEST 1u

#define COSTAR_SPI_REQUEST_HEADER 7u
#define COSTAR_SPI_REPLY_HEADER 3u
#define COSTAR_SPI_MAX_LENGTH 0xffffu

#endif /* COSTAR_SPI_H */
