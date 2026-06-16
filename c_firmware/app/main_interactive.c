/* Interactive-mode demo: host socket I/O via TCP loopback.
 *
 * Two FreeRTOS tasks demonstrate the host I/O blocking flow:
 *   1. Task Receiver (high priority 2): tries recv() on a TCP socket.
 *      If no data available (EAGAIN/EWOULDBLOCK), blocks via
 *      sim_host_block_on_fd().
 *   2. Task Sender (low priority 1): sends data to the paired socket,
 *      then calls vTaskDelay to yield.  Data arrives in kernel buffer,
 *      waking the Receiver via the host poller.
 *   3. Receiver resumes, reads the data, and both tasks exit.
 *
 * Uses TCP loopback (127.0.0.1) instead of POSIX socketpair for
 * cross-platform support (Linux, macOS, Windows).
 */

#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"
#include "sim_abi.h"

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#pragma comment(lib, "ws2_32.lib")
#define close_socket(s) closesocket(s)
#define sock_errno WSAGetLastError()
#define E_AGAIN WSAEWOULDBLOCK
typedef SOCKET socket_t;
#else
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#define close_socket(s) close(s)
#define sock_errno errno
#define E_AGAIN EAGAIN
typedef int socket_t;
#endif

#include <string.h>
#include <stdio.h>

/* ── Socket file descriptors ──────────────────────────────────────── */
static socket_t fd_send;   /* Task Sender sends here       */
static socket_t fd_recv;   /* Task Receiver receives here  */

/* ── Set a socket to non-blocking mode ─────────────────────────────── */
static int set_nonblock( socket_t s )
{
#ifdef _WIN32
    unsigned long mode = 1;
    return ioctlsocket( s, (long)FIONBIO, &mode );
#else
    int flags = fcntl( (int)s, F_GETFL, 0 );
    if( flags < 0 ) return -1;
    return fcntl( (int)s, F_SETFL, flags | O_NONBLOCK );
#endif
}

/* ── Create a TCP loopback socket pair ────────────────────────────────
 *
 * Returns 0 on success, -1 on failure.  On success, *out_a and *out_b
 * are two connected non-blocking TCP sockets.
 */
static int tcp_loopback_pair( socket_t *out_a, socket_t *out_b )
{
    socket_t listener;
    struct sockaddr_in addr;
    socklen_t addr_len = sizeof( addr );
    socket_t client, server;
    int ret;

    /* Create listener socket. */
    listener = socket( AF_INET, SOCK_STREAM, 0 );
#ifdef _WIN32
    if( listener == INVALID_SOCKET ) return -1;
#else
    if( listener < 0 ) return -1;
#endif

    /* Bind to 127.0.0.1:0 (OS picks a free port). */
    memset( &addr, 0, sizeof( addr ) );
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = inet_addr( "127.0.0.1" );
    addr.sin_port = 0; /* auto-assign */
    if( bind( listener, (struct sockaddr *)&addr, sizeof( addr ) ) < 0 )
    {
        close_socket( listener );
        return -1;
    }

    /* Get the assigned port. */
    if( getsockname( listener, (struct sockaddr *)&addr, &addr_len ) < 0 )
    {
        close_socket( listener );
        return -1;
    }

    /* Start listening. */
    if( listen( listener, 1 ) < 0 )
    {
        close_socket( listener );
        return -1;
    }

    /* Create client socket and connect to the listener. */
    client = socket( AF_INET, SOCK_STREAM, 0 );
#ifdef _WIN32
    if( client == INVALID_SOCKET )
#else
    if( client < 0 )
#endif
    {
        close_socket( listener );
        return -1;
    }

    ret = connect( client, (struct sockaddr *)&addr, sizeof( addr ) );
    if( ret < 0 )
    {
        close_socket( client );
        close_socket( listener );
        return -1;
    }

    /* Accept the connection on the server side. */
    server = accept( listener, NULL, NULL );
#ifdef _WIN32
    if( server == INVALID_SOCKET )
#else
    if( server < 0 )
#endif
    {
        close_socket( client );
        close_socket( listener );
        return -1;
    }

    /* Close the listener — we only need the two connected sockets. */
    close_socket( listener );

    /* Set both sockets non-blocking. */
    if( set_nonblock( server ) < 0 || set_nonblock( client ) < 0 )
    {
        close_socket( server );
        close_socket( client );
        return -1;
    }

    *out_a = server;
    *out_b = client;
    return 0;
}

/* ── Task Receiver (high priority — runs first, blocks on I/O) ──── */
static void vTaskReceiver( void *pvParameters )
{
    char buf[256];
    int n;
    (void) pvParameters;

    sim_trace_u32( "receiver_start", 1 );

    /* Register the receive fd with the host poller so the scheduler
     * can wake us when data arrives. */
    sim_host_register_fd( (int)fd_recv );

    n = (int)recv( fd_recv, buf, sizeof( buf ) - 1, 0 );
    if( n < 0 && ( sock_errno == E_AGAIN ||
#ifdef EWOULDBLOCK
                   sock_errno == EWOULDBLOCK ||
#endif
                   0 ) )
    {
        sim_trace_u32( "receiver_blocking", (uint32_t)(intptr_t)fd_recv );
        /* No data yet — block on this fd.  The fiber yields with
         * IoWait; the scheduler will resume us when the poller
         * signals that fd_recv is readable. */
        sim_host_block_on_fd( (int)fd_recv );

        /* Resumed — data should be available now. */
        n = (int)recv( fd_recv, buf, sizeof( buf ) - 1, 0 );
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

    sim_host_deregister_fd( (int)fd_recv );
    sim_trace_u32( "receiver_done", 1 );
}

/* ── Task Sender (low priority — runs after Receiver blocks) ─────── */
static void vTaskSender( void *pvParameters )
{
    const char *msg = "Hello from interactive mode!";
    int n;
    (void) pvParameters;

    sim_trace_u32( "sender_start", 1 );

    /* Delay briefly to ensure Receiver tries to read first and
     * blocks on the fd. */
    vTaskDelay( 1 );

    n = (int)send( fd_send, msg, (int)strlen( msg ), 0 );
    sim_trace_u32( "sender_wrote", (uint32_t)n );

    vTaskDelay( 1 );
    sim_trace_u32( "sender_done", 1 );
}

/* ── Idle / timer task memory is provided by main.c ────────────── */

/* ── Entry point called from Rust when --mode interactive is set ─── */
int c_sim_interactive_main( void )
{
    socket_t sv[2];
    TaskHandle_t thR, thS;
    sim_task_handle_t hR, hS;

#ifdef _WIN32
    /* Initialize Winsock on Windows. */
    WSADATA wsa_data;
    if( WSAStartup( MAKEWORD( 2, 2 ), &wsa_data ) != 0 )
    {
        sim_trace_u32( "wsa_startup_fail", 1 );
        return 1;
    }
#endif

    /* Create a connected TCP loopback pair (cross-platform). */
    if( tcp_loopback_pair( &sv[0], &sv[1] ) < 0 )
    {
        sim_trace_u32( "tcp_pair_fail", 1 );
#ifdef _WIN32
        WSACleanup();
#endif
        return 1;
    }
    fd_send = sv[0];
    fd_recv = sv[1];

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

    close_socket( fd_send );
    close_socket( fd_recv );

#ifdef _WIN32
    WSACleanup();
#endif
    return 0;
}
