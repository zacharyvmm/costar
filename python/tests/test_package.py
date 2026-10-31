"""The package, built as a wheel: its stubs load with the runtimes it requires."""
from pathlib import Path
import re
import shutil
import subprocess
import sys
import zipfile

PYTHON = Path(__file__).resolve().parents[1]


def test_the_wheel_requires_runtimes_as_new_as_its_stubs(tmp_path):
    # A copy, laid out as an sdist is (with the proto inside), so the
    # build leaves the source tree alone.
    source = tmp_path / "source"
    shutil.copytree(PYTHON, source, ignore=shutil.ignore_patterns("build", "*.egg-info", "__pycache__", "v1", "tests"))
    shutil.copytree(PYTHON.parent / "proto", source / "proto")
    subprocess.run([sys.executable, "-m", "pip", "wheel", "--no-build-isolation", "--no-deps", "-q",
                    "-w", str(tmp_path / "dist"), str(source)], check=True)
    (wheel,) = (tmp_path / "dist").glob("costar-*.whl")
    with zipfile.ZipFile(wheel) as z:
        (metadata,) = [n for n in z.namelist() if n.endswith(".dist-info/METADATA")]
        requires = re.findall(r"^Requires-Dist: (\S+)$", z.read(metadata).decode(), re.M)
        pb2 = z.read("costar/v1/costar_pb2.py").decode()
        pb2_grpc = z.read("costar/v1/costar_pb2_grpc.py").decode()
    gencode = re.search(r"^# Protobuf Python Version: (\S+)$", pb2, re.M)[1]
    generated = re.search(r"^GRPC_GENERATED_VERSION = '(\S+)'$", pb2_grpc, re.M)[1]
    assert f"protobuf>={gencode}" in requires
    assert f"grpcio>={generated}" in requires
