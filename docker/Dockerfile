# A CI image with costar installed, and Zephyr ready for Zephyr images.
#
#   docker build -f docker/Dockerfile -t costar .
#   docker run --rm -v "$PWD:/src" -w /src costar \
#     sh -c 'cmake -S . -B build -G Ninja && cmake --build build && ctest --test-dir build -j"$(nproc)"'
#
# In it, find_package(costar) works, and COSTAR_ZEPHYR_BASE and
# COSTAR_ZEPHYR_PYTHON are set in the environment.
FROM ubuntu:24.04

ARG ZEPHYR_VERSION=v4.5.0
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential cmake ninja-build git ca-certificates python3 python3-venv \
      libboost-context-dev gcovr \
    && rm -rf /var/lib/apt/lists/*

# Zephyr, for costar_add_zephyr_image(): the tree and a Python with its
# requirements.  No SDK: native_sim uses the host compiler.
RUN git clone --depth 1 --branch ${ZEPHYR_VERSION} https://github.com/zephyrproject-rtos/zephyr.git /opt/zephyr \
    && python3 -m venv /opt/zephyr-venv \
    && /opt/zephyr-venv/bin/pip install --no-cache-dir -r /opt/zephyr/scripts/requirements-base.txt
ENV ZEPHYR_BASE=/opt/zephyr \
    COSTAR_ZEPHYR_PYTHON=/opt/zephyr-venv/bin/python

# costar itself, tested, then installed to /usr/local.
COPY . /tmp/costar
RUN cmake -S /tmp/costar -B /tmp/costar-build -G Ninja -DCMAKE_BUILD_TYPE=Release \
      -DCOSTAR_ZEPHYR_BASE=/opt/zephyr -DCOSTAR_ZEPHYR_PYTHON=/opt/zephyr-venv/bin/python \
    && cmake --build /tmp/costar-build \
    && ctest --test-dir /tmp/costar-build --output-on-failure -j"$(nproc)" \
    && cmake --install /tmp/costar-build \
    && rm -rf /tmp/costar /tmp/costar-build
