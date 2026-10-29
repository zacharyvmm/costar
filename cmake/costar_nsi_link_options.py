"""Turn a Zephyr native_sim build's runner link inputs (zephyr/NSI/nsi_config:
NSI_LINK_OPTIONS and NSI_EXTRA_LIBS, e.g. -lstdc++ for C++ applications) into
two response files for the costar image: compiler-driver flags (OPTIONS) and
libraries, which must follow the objects (LIBRARIES, read by the linker).

The values are split the way the stock runner's Makefile hands them to the
shell.  The linker script (-T ...) is the stock runner's, and -static / -pie
options are for its executable; they are dropped.  Each argument is quoted
for response-file syntax (spaces, quotes and backslashes survive).  Files are
written on change only, so unchanged builds don't relink (but always created:
the image's link depends on them even when they are empty).

    python3 costar_nsi_link_options.py <nsi_config> <options file> <libraries file>
"""
import re
import shlex
import sys
from pathlib import Path

EXECUTABLE_ONLY = {"-static", "-static-pie", "-pie", "-no-pie"}


def link_arguments(config):
    for line in config.splitlines():
        match = re.match(r"NSI_(LINK_OPTIONS|EXTRA_LIBS):=(.*)", line)
        if match:
            yield from shlex.split(match.group(2))


def response_file(arguments):
    quoted = (a.replace("\\", "\\\\").replace('"', '\\"') for a in arguments)
    return "".join(f'"{a}"\n' for a in quoted)


def write_if_changed(path, content):
    path = Path(path)
    if not path.exists() or path.read_text(encoding="utf-8", errors="surrogateescape") != content:
        path.write_text(content, encoding="utf-8", errors="surrogateescape")


def main(nsi_config, options_file, libraries_file):
    config = Path(nsi_config).read_text(encoding="utf-8", errors="surrogateescape")
    options, libraries = [], []
    arguments = link_arguments(config)
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
