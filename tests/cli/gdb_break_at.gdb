# costar run --break-at-ms 3: GDB stops before anything due at 3 ms runs,
# with every task suspended.
run
costar machines
costar threads board
costar bt board:2
continue
