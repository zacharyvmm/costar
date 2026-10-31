/*
 * Reports the data model this firmware is compiled with.  An x86_64 build is
 * LP64 (8-byte pointers, long and size_t); an i686 build is ILP32, like
 * Cortex-M.  Bugs that depend on these (truncated pointers, a long assumed to
 * be 32 or 64 bits, struct layouts shared with other code, size_t arithmetic
 * that wraps) only show up in a build with the target's sizes.
 *
 * Struct layout is where i686 and Cortex-M still differ: i686's ABI aligns
 * 8-byte members to 4 inside structs, ARM's to 8.  For `struct record`
 * below, Cortex-M has stamp at 16 and size 24; i686 has 12 and 20.
 */
#include <stddef.h>

#include "data_model.h"

struct record
{
    char tag;
    long count;
    void * next;
    uint64_t stamp;
};

void data_model_report( void ( * emit )( const char * label, uint32_t value ) )
{
    emit( "sizeof_pointer", sizeof( void * ) );
    emit( "sizeof_long", sizeof( long ) );
    emit( "sizeof_size_t", sizeof( size_t ) );
    emit( "sizeof_long_long", sizeof( long long ) );
    emit( "record_count_at", offsetof( struct record, count ) );
    emit( "record_next_at", offsetof( struct record, next ) );
    emit( "record_stamp_at", offsetof( struct record, stamp ) );
    emit( "sizeof_record", sizeof( struct record ) );

    /* Sizes add up at the target's width: 3 GiB + 1 GiB wraps to 0 in 32
     * bits. */
    volatile size_t xLength = ( size_t ) 0xC0000000u;
    emit( "size_t_4gib_wraps", xLength + ( size_t ) 0x40000000u == 0 );
}
