"""Turn a Zephyr native_sim build's runner link inputs (zephyr/NSI/nsi_config:
NSI_LINK_OPTIONS and NSI_EXTRA_LIBS, e.g. -lstdc++ for C++ applications) into
two response files for the costar image: compiler-driver flags (OPTIONS) and
libraries, which must follow the objects (LIBRARIES, read by the linker).

The values are expanded by make and split by the shell, as the stock runner's
Makefile does (so $$ORIGIN reaches the linker as $ORIGIN).  The linker script (-T ...) is the stock runner's, and -static / -pie
options are for its executable; they are dropped.  Each argument is quoted
for response-file syntax (spaces, quotes and backslashes survive).  Files are
written on change only, so unchanged builds don't relink (but always created:
the image's link depends on them even when they are empty).

    python3 costar_nsi_link_options.py <make> <nsi_config> <options file> <libraries file>
"""
import os
import subprocess
import sys
from pathlib import Path

EXECUTABLE_ONLY = {"-static", "-static-pie", "-pie", "-no-pie"}


def link_arguments(make, nsi_config):
    rule = "costar_link_arguments:\n\t@printf '%s\\0' $(NSI_LINK_OPTIONS) $(NSI_EXTRA_LIBS)\n"
    env = {k: v for k, v in os.environ.items() if k not in ("MAKEFLAGS", "MFLAGS", "MAKELEVEL")}
    output = subprocess.run(
        [make, "-s", "-r", "-R", "--no-print-directory", "-f", nsi_config, "-f", "-", "costar_link_arguments"],
        input=rule.encode(), stdout=subprocess.PIPE, env=env, check=True).stdout
    for argument in output.split(b"\0"):
        if argument:
            yield argument.decode("utf-8", errors="surrogateescape")


def response_file(arguments):
    quoted = (a.replace("\\", "\\\\").replace('"', '\\"') for a in arguments)
    return "".join(f'"{a}"\n' for a in quoted)


def write_if_changed(path, content):
    path = Path(path)
    if not path.exists() or path.read_text(encoding="utf-8", errors="surrogateescape") != content:
        path.write_text(content, encoding="utf-8", errors="surrogateescape")


def main(make, nsi_config, options_file, libraries_file):
    options, libraries = [], []
    arguments = link_arguments(make, nsi_config)
    for argument in arguments:
        if argument == "-T":
            next(arguments, None)
        elif argument.startswith("-T") or argument in EXECUTABLE_ONLY:
            pass
        elif argument.startswith("-l") or argument.endswith((".a", ".so")):
            libraries.append(argument)
        else:
            options.append(argument)
    write_if_changed(options_file, response_file(options))
    write_if_changed(libraries_file, response_file(libraries))


if __name__ == "__main__":
    main(*sys.argv[1:])
