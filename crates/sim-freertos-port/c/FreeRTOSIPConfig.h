/*
 * FreeRTOS+TCP V4.x configuration for costar simulator.
 *
 * This provides the ipconfig* macros required by FreeRTOS+TCP.
 * The simulator uses VirtualEthDevice as the network interface;
 * the actual hardware driver (sim_net_if.c) bridges to costar's
 * deterministic virtual Ethernet device.
 *
 * See: https://www.freertos.org/FreeRTOS-Plus/FreeRTOS_Plus_TCP/TCP_IP_Configuration.html
 */

#ifndef FREERTOS_IP_CONFIG_H
#define FREERTOS_IP_CONFIG_H

/* Protocol versions — only IPv4 for MVP. */
#define ipconfigUSE_IPv4                    1
#define ipconfigUSE_IPv6                    0

/* Enable backward-compatible FreeRTOS_IPInit() API (V4+). */
#define ipconfigIPv4_BACKWARD_COMPATIBLE    1

/* Debug printing — map to FreeRTOS printf facility. */
#define ipconfigHAS_DEBUG_PRINTF            0
#define ipconfigHAS_PRINTF                  0

/* Byte order — host is little-endian on all costar targets. */
#define ipconfigBYTE_ORDER                  pdFREERTOS_LITTLE_ENDIAN

/* No checksum offloading — software computes checksums. */
#define ipconfigDRIVER_INCLUDED_RX_IP_CHECKSUM  0

/* Default socket timeouts (in ms). */
#define ipconfigSOCK_DEFAULT_RECEIVE_BLOCK_TIME    ( 5000 )
#define ipconfigSOCK_DEFAULT_SEND_BLOCK_TIME       ( 5000 )

/* DNS — enabled for hostname resolution. */
#define ipconfigUSE_DNS                     1
#define ipconfigDNS_CACHE_ENTRIES           2
#define ipconfigDNS_REQUEST_ATTEMPTS        2

/* IP task priority and stack. */
#define ipconfigIP_TASK_PRIORITY            ( configMAX_PRIORITIES - 2 )
#define ipconfigIP_TASK_STACK_SIZE_WORDS    ( configMINIMAL_STACK_SIZE * 5 )

/* Network event hook — call vApplicationIPNetworkEventHook. */
#define ipconfigUSE_NETWORK_EVENT_HOOK      0

/* DHCP — disabled for deterministic tests (static IP). */
#define ipconfigUSE_DHCP                    0
#define ipconfigUSE_DHCP_HOOK               0

/* ARP cache. */
#define ipconfigARP_CACHE_ENTRIES           6
#define ipconfigMAX_ARP_RETRANSMISSIONS     5
#define ipconfigMAX_ARP_AGE                 150

/* Network buffers — keep small for deterministic tests. */
#define ipconfigNUM_NETWORK_BUFFER_DESCRIPTORS  30
#define ipconfigEVENT_QUEUE_LENGTH \
    ( ipconfigNUM_NETWORK_BUFFER_DESCRIPTORS + 5 )

/* TCP support. */
#define ipconfigUSE_TCP                     1
#define ipconfigUSE_TCP_WIN                 1
#define ipconfigTCP_WIN_SEG_COUNT           2
#define ipconfigTCP_RX_BUFFER_LENGTH        ( 4096 )
#define ipconfigTCP_TX_BUFFER_LENGTH        ( 4096 )
#define ipconfigTCP_HANG_PROTECTION         1
#define ipconfigTCP_HANG_PROTECTION_TIME    30

/* Time-to-live. */
#define ipconfigUDP_TIME_TO_LIVE            128
#define ipconfigTCP_TIME_TO_LIVE            128

/* MTU — standard Ethernet. */
#define ipconfigNETWORK_MTU                 1500U

/* Packet filler for alignment. */
#define ipconfigPACKET_FILLER_SIZE          2U

/* Socket features. */
#define ipconfigALLOW_SOCKET_SEND_WITHOUT_BIND   1
#define ipconfigSUPPORT_SELECT_FUNCTION          1
#define ipconfigSUPPORT_SIGNALS                  0
#define ipconfigSOCKET_HAS_USER_SEMAPHORE        0
#define ipconfigSOCKET_HAS_USER_WAKE_CALLBACK    0
#define ipconfigUSE_CALLBACKS                    0

/* ICMP ping support. */
#define ipconfigREPLY_TO_INCOMING_PINGS          1
#define ipconfigSUPPORT_OUTGOING_PINGS           1

/* Ethernet filtering — driver does basic filtering. */
#define ipconfigFILTER_OUT_NON_ETHERNET_II_FRAMES  0
#define ipconfigETHERNET_DRIVER_FILTERS_FRAME_TYPES  0

/* INET address support. */
#define ipconfigINCLUDE_FULL_INET_ADDR           1

/* Address validation. */
#define ipconfigIS_VALID_PROG_ADDRESS( x )    ( ( x ) != NULL )

/* Keep-alive. */
#define ipconfigTCP_KEEP_ALIVE                   1
#define ipconfigTCP_KEEP_ALIVE_INTERVAL          20

/* mDNS / LLMNR / NBNS — disabled for MVP. */
#define ipconfigUSE_NBNS                         0
#define ipconfigUSE_LLMNR                        0
#define ipconfigUSE_MDNS                         0

/* ARP features. */
#define ipconfigUSE_ARP_REMOVE_ENTRY             1
#define ipconfigARP_STORES_REMOTE_ADDRESSES      1

/* Buffer padding. */
#define ipconfigBUFFER_PADDING                   14

#endif /* FREERTOS_IP_CONFIG_H */
