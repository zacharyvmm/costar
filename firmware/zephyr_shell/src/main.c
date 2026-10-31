/*
 * The Zephyr shell on a costar UART: an ordinary Zephyr application whose
 * shell (see prj.conf and app.overlay) runs on its own thread.  It adds one
 * command of its own, `costar hello`.
 */
#include <zephyr/kernel.h>
#include <zephyr/shell/shell.h>

static int cmd_hello(const struct shell *sh, size_t argc, char **argv)
{
	shell_print(sh, "hello from %s at %u ms", argc > 1 ? argv[1] : "costar", k_uptime_get_32());
	return 0;
}

SHELL_STATIC_SUBCMD_SET_CREATE(costar_commands,
			       SHELL_CMD_ARG(hello, NULL, "Say hello: hello [name]", cmd_hello, 1, 1),
			       SHELL_SUBCMD_SET_END);
SHELL_CMD_REGISTER(costar, &costar_commands, "costar test commands", NULL);

int main(void)
{
	return 0;
}
