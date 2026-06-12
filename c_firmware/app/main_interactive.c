/* Interactive-mode demo: host socket I/O via socketpair.
 *
 * Two FreeRTOS tasks demonstrate the host I/O blocking flow:
 *   1. Task Receiver (high priority 2): tries read() on a socketpair fd.
 *      If no data available (EAGAIN), blocks via sim_host_block_on_fd().
 *   2. Task Sender (low priority 1): writes data to the paired fd, then
 *      calls vTaskDelay to yield.  Data arrives in kernel buffer, waking
 *      the Receiver via the host poller.
 *   3. Receiver resumes, reads the data, and both tasks exit.
 */

#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"
#include "sim_abi.h"

#include <sys/socket.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <string.h>
#include <stdio.h>

/* ── socketpair file descriptors ──────────────────────────────────── */
static int fd_send;   /* Task Sender writes here       */
static int fd_recv;   /* Task Receiver reads from here  */

/* ── Task Receiver (high priority — runs first, blocks on I/O) ──── */
static void vTaskReceiver( void *pvParameters )
{
    char buf[256];
    ssize_t n;
    (void) pvParameters;

    sim_trace_u32( "receiver_start", 1 );

    /* Register the receive fd with the host poller so the scheduler
     * can wake us when data arrives. */
    sim_host_register_fd( fd_recv );

    n = read( fd_recv, buf, sizeof( buf ) - 1 );
    if( n < 0 && ( errno == EAGAIN || errno == EWOULDBLOCK ) )
    {
        sim_trace_u32( "receiver_blocking", (uint32_t)fd_recv );
        /* No data yet — block on this fd.  The fiber yields with
         * IoWait; the scheduler will resume us when the poller
         * signals that fd_recv is readable. */
        sim_host_block_on_fd( fd_recv );

        /* Resumed — data should be available now. */
        n = read( fd_recv, buf, sizeof( buf ) - 1 );
    }

    if( n > 0 )
    {
        buf[n] = '\0';
        sim_trace_u32( "receiver_got", (uint32_t)n );
    }
    else
    {
        sim_trace_u32( "receiver_read_fail", (uint32_t)n );
    }

    sim_host_deregister_fd( fd_recv );
    sim_trace_u32( "receiver_done", 1 );
}

/* ── Task Sender (low priority — runs after Receiver blocks) ─────── */
static void vTaskSender( void *pvParameters )
{
    const char *msg = "Hello from interactive mode!";
    ssize_t n;
    (void) pvParameters;

    sim_trace_u32( "sender_start", 1 );

    /* Delay briefly to ensure Receiver tries to read first and
     * blocks on the fd. */
    vTaskDelay( 1 );

    n = write( fd_send, msg, strlen( msg ) );
    sim_trace_u32( "sender_wrote", (uint32_t)n );

    vTaskDelay( 1 );
    sim_trace_u32( "sender_done", 1 );
}

/* ── Idle / timer task memory is provided by main.c ────────────── */

/* ── Entry point called from Rust when --mode interactive is set ─── */
int c_sim_interactive_main( void )
{
    int sv[2];
    TaskHandle_t thR, thS;
    sim_task_handle_t hR, hS;

    /* Create a connected socket pair. */
    if( socketpair( AF_UNIX, SOCK_STREAM, 0, sv ) < 0 )
    {
        sim_trace_u32( "socketpair_fail", (uint32_t)errno );
        return 1;
    }
    fd_send = sv[0];
    fd_recv = sv[1];

    /* Set both fds non-blocking so read() / write() don't stall the
     * simulator if data isn't immediately available. */
    int flags = fcntl( fd_send, F_GETFL, 0 );
    fcntl( fd_send, F_SETFL, flags | O_NONBLOCK );
    flags = fcntl( fd_recv, F_GETFL, 0 );
    fcntl( fd_recv, F_SETFL, flags | O_NONBLOCK );

    /* Create FreeRTOS tasks.
     * Receiver at priority 2 (higher) so it runs first and blocks.
     * Sender at priority 1 (lower) so it runs after Receiver yields. */
    xTaskCreate( vTaskReceiver, "Receiver", 512, NULL, 2, &thR );
    xTaskCreate( vTaskSender,   "Sender",   512, NULL, 1, &thS );

    /* Create Rust fibers (must happen from main, not trace hook). */
    hR = sim_create_task( "Receiver", (sim_task_entry_fn)vTaskReceiver, NULL, 512, 2 );
    hS = sim_create_task( "Sender",   (sim_task_entry_fn)vTaskSender,   NULL, 512, 1 );

    /* Register TCB mappings for sim_set_current_task_by_id. */
    sim_bridge_register( hR, thR );
    sim_bridge_register( hS, thS );

    vTaskStartScheduler();

    close( fd_send );
    close( fd_recv );
    return 0;
}
