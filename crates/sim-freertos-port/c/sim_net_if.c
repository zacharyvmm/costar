/*
 * FreeRTOS+TCP V4.x NetworkInterface driver for costar.
 *
 * This driver bridges FreeRTOS+TCP's NetworkInterface_t API to costar's
 * VirtualEthDevice (via sim_eth_* C ABI).  It replaces the hardware-specific
 * NetworkInterface driver with our deterministic virtual Ethernet device.
 *
 * The V4 API uses function pointers in a NetworkInterface_t struct:
 *   pfInitialise       → sim_eth_register()
 *   pfOutput           → sim_eth_send()
 *   pfGetPhyLinkStatus → always returns pdTRUE (virtual link always up)
 *
 * Incoming frames are delivered by polling sim_eth_recv() and calling
 * xNetworkInterfaceInput() to feed them into the FreeRTOS+TCP stack.
 *
 * See: source/include/FreeRTOS_Routing.h for NetworkInterface_t definition.
 */

#include <stdint.h>
#include <stddef.h>
#include <string.h>

#include "FreeRTOS.h"
#include "task.h"
#include "FreeRTOS_IP.h"
#include "FreeRTOS_Sockets.h"
#include "NetworkInterface.h"
#include "FreeRTOS_Routing.h"
#include "FreeRTOS_IP_Private.h"
#include "FreeRTOSIPConfig.h"
#include "sim_abi.h"

/* Forward declarations. */
extern uint32_t sim_eth_register(uint32_t id, const uint8_t *mac, uint32_t mtu);
extern uint32_t sim_eth_send(uint32_t id, const uint8_t *data, uint32_t len);
extern uint32_t sim_eth_recv(uint32_t id, uint8_t *buf, uint32_t buf_size);
extern uint32_t sim_eth_poll(uint32_t id);
extern void     sim_eth_on_recv(uint32_t id, void (*callback)(void));

/* ── Driver state ──────────────────────────────────────────────── */

/* Virtual Ethernet device ID. */
static uint32_t sim_nic_eth_id = 0;

/* The single NetworkInterface descriptor (registered with FreeRTOS+TCP). */
static NetworkInterface_t *sim_nic_interface = NULL;

/* Our MAC address. */
static uint8_t sim_nic_mac[6] = { 0x02, 0x00, 0x00, 0x00, 0x00, 0x01 };

/* Whether the receive task should keep polling. */
static BaseType_t sim_nic_running = pdFALSE;

/* ── Initialise ────────────────────────────────────────────────── */

static BaseType_t prvInterfaceInitialise( NetworkInterface_t *pxInterface )
{
    (void) pxInterface;

    /* Register the virtual Ethernet device. */
    sim_eth_register(sim_nic_eth_id, sim_nic_mac, ipconfigNETWORK_MTU);
    sim_nic_interface = pxInterface;

    sim_trace_u32("tcp:nic_init", sim_nic_eth_id);

    return pdPASS;
}

/* ── Output (transmit) ─────────────────────────────────────────── */

static BaseType_t prvInterfaceOutput( NetworkInterface_t *pxInterface,
                                       NetworkBufferDescriptor_t *const pxNetworkBuffer,
                                       BaseType_t xReleaseAfterSend )
{
    (void) pxInterface;

    /* Get the Ethernet frame from the network buffer. */
    const uint8_t *pucEthernetBuffer = pxNetworkBuffer->pucEthernetBuffer;
    size_t uxLength = pxNetworkBuffer->xDataLength;

    if (uxLength > 0 && pucEthernetBuffer != NULL) {
        sim_eth_send(sim_nic_eth_id, pucEthernetBuffer, (uint32_t) uxLength);
    }

    if (xReleaseAfterSend != pdFALSE) {
        vReleaseNetworkBufferAndDescriptor( pxNetworkBuffer );
    }

    return pdPASS;
}

/* ── Link status ───────────────────────────────────────────────── */

static BaseType_t prvGetPhyLinkStatus( NetworkInterface_t *pxInterface )
{
    (void) pxInterface;
    /* Virtual link is always up. */
    return pdTRUE;
}

/* ── Receive polling task ──────────────────────────────────────── */

/*
 * Poll the virtual Ethernet device for incoming frames and feed them
 * into the FreeRTOS+TCP stack via xNetworkInterfaceInput().
 *
 * This function should be called periodically (e.g., from the IP-task
 * or a dedicated polling task) to check for new frames.
 */
void sim_nic_poll_receive( void )
{
    uint8_t buf[2048];
    uint32_t n;

    while ( ( n = sim_eth_recv( sim_nic_eth_id, buf, sizeof( buf ) ) ) > 0 )
    {
        /* Feed the frame into FreeRTOS+TCP. */
        eFrameProcessingResult_t eResult = eConsiderFrameForProcessing( buf );

        if ( eResult != eProcessBuffer )
        {
            sim_trace_u32("tcp:input_dropped", (uint32_t) n);
        }
    }
}

/* ── Public initialization ─────────────────────────────────────── */

/*
 * Initialise the costar network interface and register it with
 * FreeRTOS+TCP.  Called once at startup before FreeRTOS_IPInit().
 *
 * Returns the NetworkInterface_t descriptor, or NULL on failure.
 */
NetworkInterface_t *sim_nic_init( uint32_t eth_id, const uint8_t *mac )
{
    sim_nic_eth_id = eth_id;

    if ( mac != NULL )
    {
        memcpy( sim_nic_mac, mac, 6 );
    }

    /* Fill the descriptor. */
    static NetworkInterface_t xInterface;

    memset( &xInterface, 0, sizeof( xInterface ) );
    xInterface.pcName             = "costar-eth0";
    xInterface.pvArgument         = NULL;
    xInterface.pfInitialise       = prvInterfaceInitialise;
    xInterface.pfOutput           = prvInterfaceOutput;
    xInterface.pfGetPhyLinkStatus = prvGetPhyLinkStatus;
    /* pfAddAllowedMAC / pfRemoveAllowedMAC left NULL → promiscuous mode. */

    /* Register with FreeRTOS+TCP. */
    NetworkInterface_t *pxResult = FreeRTOS_AddNetworkInterface( &xInterface );

    if ( pxResult != NULL )
    {
        sim_nic_interface = pxResult;
    }

    return pxResult;
}

/*
 * Backward-compatible interface descriptor filler.
 * Required by FreeRTOS_IPInit() when ipconfigIPv4_BACKWARD_COMPATIBLE is set.
 * We use a static descriptor (registered in sim_nic_init above).
 */
#if (ipconfigIPv4_BACKWARD_COMPATIBLE == 1)
    NetworkInterface_t *pxFillInterfaceDescriptor(BaseType_t xEMACIndex,
                                                    NetworkInterface_t *pxInterface)
    {
        (void) xEMACIndex;
        /* Our NetworkInterface is already registered via FreeRTOS_AddNetworkInterface.
         * Just return the existing descriptor. */
        if (sim_nic_interface != NULL && pxInterface != NULL) {
            memcpy(pxInterface, sim_nic_interface, sizeof(NetworkInterface_t));
        }
        return pxInterface;
    }
#endif

/*
 * Start the receive polling loop (runs as a FreeRTOS task).
 * In a real implementation this would use a timer or callback,
 * but for MVP we use simple polling at each tick.
 */
void sim_nic_start_poll( void )
{
    sim_nic_running = pdTRUE;
}

void sim_nic_stop_poll( void )
{
    sim_nic_running = pdFALSE;
}

BaseType_t sim_nic_is_running( void )
{
    return sim_nic_running;
}

/* ── Stub: vApplicationIPNetworkEventHook ──────────────────────── */
#if ( ipconfigUSE_NETWORK_EVENT_HOOK == 1 )
    void vApplicationIPNetworkEventHook( eIPCallbackEvent_t eNetworkEvent )
    {
        (void) eNetworkEvent;
        sim_trace_u32("tcp:net_event", (uint32_t) eNetworkEvent);
    }
#endif
