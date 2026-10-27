"""The code under test is this checkout's broker, never the machine's staged one: without this,
a test that runs scripts/heavy.py without NUCLEOS_HEAVY_DIR would forward to master's copy."""

import os

os.environ["NUCLEOS_HEAVY_LOCAL"] = "1"
