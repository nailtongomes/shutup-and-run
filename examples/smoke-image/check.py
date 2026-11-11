"""The whole job: prove Python runs, show the env reached it, stay up briefly."""
import os
import platform
import time

print("python ok", platform.python_version(), os.environ.get("SMOKE_MARK", ""), flush=True)
time.sleep(float(os.environ.get("SMOKE_SLEEP", "2")))
