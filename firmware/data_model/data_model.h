/*
 * Reports the data model firmware is compiled with: type sizes and a
 * struct's layout, one value at a time.  Shared by
 * freertos_data_model and zephyr_data_model.
 */
#ifndef DATA_MODEL_H
#define DATA_MODEL_H

#include <stdint.h>

void data_model_report( void ( * emit )( const char * label, uint32_t value ) );

#endif /* DATA_MODEL_H */
