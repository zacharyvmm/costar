# costar run --break-on board:received=2: GDB stops inside the firmware's
# costar_trace() call, in the receiving task.
run
bt
costar machines
costar threads
costar bt 1
continue
