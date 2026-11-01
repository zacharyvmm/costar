/*
 * costar_i2c.h — I2C transfers over a costar channel device.
 *
 * An I2C controller is a channel.  Each transfer (the messages of one
 * i2c_transfer(), to one target address) is one request frame, and the
 * bus's targets answer it with one reply frame.  On the host, device models
 * answer (scenario [[model]]s: i2c-registers, i2c-eeprom); any other
 * endpoint can too.  Zephyr's costar,i2c driver speaks this; FreeRTOS
 * firmware can use costar_chan_send/recv directly.
 *
 * Request:
 *   bytes 0-1  the target address, big-endian; COSTAR_I2C_ADDR_10_BITS marks
 *              a 10-bit address
 *   byte 2     a tag, echoed by the reply (match replies to requests)
 *   then each message in order:
 *     byte 0     flags: COSTAR_I2C_MSG_READ, _STOP, _RESTART
 *     bytes 1-2  length, big-endian
 *     bytes 3-   a write's data (a read has none)
 *
 * Reply:
 *   bytes 0-1  the target address, as in the request
 *   byte 2     the request's tag
 *   byte 3     status: COSTAR_I2C_ACK, or why the transfer stopped
 *   bytes 4-   on ACK, the data of the read messages, in order
 *
 * Both start with the address, so a wire fault's match_hex can pick one
 * target's traffic.  A target that doesn't answer (a dropped frame) leaves
 * the controller waiting: firmware needs a timeout.  Messages are joined as
 * on the bus: a message with RESTART, or one in the other direction,
 * starts again (a repeated start); one with STOP ends the transaction, and
 * the next one starts a new one.  The end of a transfer is a stop.
 */
#ifndef COSTAR_I2C_H
#define COSTAR_I2C_H

#define COSTAR_I2C_ADDR_10_BITS 0x8000u

/* Message flags (the same values as Zephyr's I2C_MSG_*). */
#define COSTAR_I2C_MSG_READ 0x01u
#define COSTAR_I2C_MSG_STOP 0x02u
#define COSTAR_I2C_MSG_RESTART 0x04u

/* Reply status. */
#define COSTAR_I2C_ACK 0u
#define COSTAR_I2C_NACK_ADDRESS 1u /* no target acknowledged its address */
#define COSTAR_I2C_NACK_DATA 2u    /* the target refused a written byte */
#define COSTAR_I2C_BAD_REQUEST 3u  /* a malformed request */

#define COSTAR_I2C_REQUEST_HEADER 3u
#define COSTAR_I2C_MESSAGE_HEADER 3u
#define COSTAR_I2C_REPLY_HEADER 4u
#define COSTAR_I2C_MAX_MESSAGE 0xffffu

#endif /* COSTAR_I2C_H */
