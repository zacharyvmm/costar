# costar's image: costar installed, with Zephyr, twister, GDB, gRPC and the
# Python client ready.  Releases publish it as ghcr.io/zacharyvmm/costar.
#
#   docker build -f docker/Dockerfile -t costar .
#   docker run --rm -v "$PWD:/src" -w /src costar \
#     sh -c 'cmake -S . -B build -G Ninja && cmake --build build && ctest --test-dir build -j"$(nproc)"'
#
# In it, find_package(costar) works (find_package(costar COMPONENTS grpc)
# too), and ZEPHYR_BASE and COSTAR_ZEPHYR_PYTHON in the environment point
# costar's CMake modules at Zephyr.  One venv, first on the PATH, has Zephyr's
# and twister's requirements, pytest and the costar Python package.
#
# The `base` stage is everything but costar itself: what building costar
# needs.  The devcontainer (.devcontainer/devcontainer.json) uses it.

FROM ubuntu:24.04 AS base

ARG ZEPHYR_VERSION=v4.5.0
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential cmake ninja-build git ca-certificates python3 python3-venv \
      libboost-context-dev gcovr gdb \
      libgrpc++-dev libprotobuf-dev protobuf-compiler protobuf-compiler-grpc \
    && rm -rf /var/lib/apt/lists/*

# Zephyr, for costar_add_zephyr_image(): the tree and a Python with its
# requirements.  No SDK: native_sim uses the host compiler.  The same venv
# has twister's requirements, and the Python client's (grpcio-tools builds
# its stubs) and pytest.
RUN git clone --depth 1 --branch ${ZEPHYR_VERSION} https://github.com/zephyrproject-rtos/zephyr.git /opt/zephyr \
    && python3 -m venv /opt/zephyr-venv \
    && /opt/zephyr-venv/bin/pip install --no-cache-dir -r /opt/zephyr/scripts/requirements-base.txt \
         natsort ply junitparser grpcio grpcio-tools pytest
# Zephyr caches compiler checks in ~/.cache/zephyr, or in $ZEPHYR_BASE/.cache
# if there is no writable home (docker run --user): any user may write there.
RUN mkdir -m 1777 /opt/zephyr/.cache
ENV ZEPHYR_BASE=/opt/zephyr \
    ZEPHYR_TOOLCHAIN_VARIANT=host \
    COSTAR_ZEPHYR_PYTHON=/opt/zephyr-venv/bin/python \
    PATH=/opt/zephyr-venv/bin:$PATH

FROM base

# costar itself, with gRPC, tested (the Python client and twister too), then
# installed to /usr/local.  Zephyr is found from the environment, as it is
# for projects built in the image.
COPY . /tmp/costar
RUN cmake -S /tmp/costar -B /tmp/costar-build -G Ninja -DCMAKE_BUILD_TYPE=Release \
      -DCOSTAR_BUILD_GRPC=ON -DCOSTAR_PYTHON=/opt/zephyr-venv/bin/python \
    && cmake --build /tmp/costar-build \
    && ctest --test-dir /tmp/costar-build --output-on-failure -j"$(nproc)" \
    && cmake --install /tmp/costar-build \
    && pip install --no-cache-dir /tmp/costar/python \
    && rm -rf /tmp/costar /tmp/costar-build
