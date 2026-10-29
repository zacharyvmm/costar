"""The Zephyr link-input helper must hand the linker exactly the arguments the
stock runner's make and shell would: link a shared object through the response
files it writes and compare the RUNPATH and archive names that come out.

    python3 nsi_link_options_test.py <costar_nsi_link_options.py> <make> <scratch dir>
"""
import shlex
import subprocess
import sys
from pathlib import Path

NAMES = ["plain", "with space", "back\\slash", 'double"quote', "single'quote",
         "semi;colon", "trailing\\", "unbalanced[;value", "$ORIGIN"]


def run(*args):
    return subprocess.run([str(a) for a in args], capture_output=True, text=True, check=True).stdout


def make_quote(argument):
    """An nsi_config value: quoted for the shell, then $ escaped for make."""
    return shlex.quote(argument).replace("$", "$$")


def main(script, make, scratch):
    root = Path(scratch)
    root.mkdir(parents=True, exist_ok=True)
    (root / "probe.c").write_text("int probe;\n")
    run("cc", "-c", root / "probe.c", "-o", root / "probe.o")
    failures = []
    for i, name in enumerate(NAMES):
        case = root / str(i)
        case.mkdir(exist_ok=True)
        archive = case / f"lib{name}.a"
        archive.unlink(missing_ok=True)
        run("ar", "rcs", archive, root / "probe.o")
        runpath = f"/probe/{name}"
        config = case / "nsi_config"
        config.write_text(
            f"NSI_LINK_OPTIONS:=-T /stock/runner.ld -static {make_quote('-Wl,-rpath,' + runpath)}\n"
            f"NSI_EXTRA_LIBS:={make_quote(str(archive))}\n")
        options, libraries = case / "options.rsp", case / "libraries.rsp"
        run(sys.executable, script, make, config, options, libraries)
        image = case / "probe.so"
        run("cc", "-shared", f"@{options}", root / "probe.o", f"-Wl,@{libraries}", "-o", image)
        dynamic = run("readelf", "-d", image)
        if f"Library runpath: [{runpath}]" not in dynamic:
            failures.append(f"{name!r}: RUNPATH is not {runpath!r}:\n{dynamic}")
    if failures:
        sys.exit("\n".join(failures))
    print(f"{len(NAMES)} argument spellings survived")


if __name__ == "__main__":
    main(*sys.argv[1:])
