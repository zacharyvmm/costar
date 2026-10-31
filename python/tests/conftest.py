"""Shared helpers for the example tests.  They drive costar's own test
firmware (firmware/) and scenarios (tests/scenarios/)."""
from pathlib import Path

import pytest


@pytest.fixture
def scenarios():
    """costar's scenario directory."""
    return Path(__file__).resolve().parents[2] / "tests" / "scenarios"


@pytest.fixture
def require_image(costar_images):
    """Skip the test unless every image named is in an image directory
    (Zephyr images are only built when costar is configured with Zephyr)."""

    def require(*names):
        for name in names:
            if not any((d / name).is_file() for d in costar_images):
                pytest.skip(f"{name} isn't built")

    return require
