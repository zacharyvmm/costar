# A Zephyr machine and a FreeRTOS one: break on the Zephyr one's first
# console line, before the FreeRTOS one has booted.
run
bt
costar machines
costar threads zephyr
costar threads freertos
# Every console line breaks: let the rest of the run go by.
handle SIGTRAP nostop noprint nopass
continue
