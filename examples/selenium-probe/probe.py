"""Open a page in headless Chrome, take a screenshot, exit 0 if it worked.

Environment, all optional:
  PROBE_URL         the page. Default: this project's GitHub page.
  PROBE_EXPECT      text the page title must contain. Default: the project's
                    name when PROBE_URL is the default, nothing otherwise.
  PROBE_UPLOAD_URL  POST the PNG there, with this job's peak memory and CPU
                    time in headers. Without it the file only stays in /tmp.
  PROBE_HANG=1      never finish, to test the worker's timeout.
  PROBE_FAIL=1      exit 3 at once, to test a failing job.
"""
import json
import os
import sys
import time
import urllib.request

DEFAULT_URL = "https://github.com/nailtongomes/shutup-and-run"
SHOT = "/tmp/screenshot.png"


def cgroup_number(names, field=None):
    """A number from this container's cgroup, v2 first, or None."""
    for name in names:
        try:
            with open(f"/sys/fs/cgroup/{name}", encoding="ascii") as f:
                text = f.read()
        except OSError:
            continue
        if field is None:
            return int(text.strip())
        for line in text.splitlines():
            if line.startswith(field + " "):
                return int(line.split()[1])
    return None


def main():
    if os.environ.get("PROBE_FAIL") == "1":
        print("PROBE_FAIL=1: failing on purpose")
        return 3
    if os.environ.get("PROBE_HANG") == "1":
        print("PROBE_HANG=1: hanging on purpose", flush=True)
        while True:
            time.sleep(60)

    url = os.environ.get("PROBE_URL", DEFAULT_URL)
    expect = os.environ.get("PROBE_EXPECT", "shutup-and-run" if url == DEFAULT_URL else "")
    started = time.time()

    from seleniumbase import Driver

    driver = Driver(browser="chrome", headless=True, chromium_arg="--no-sandbox,--disable-dev-shm-usage")
    try:
        driver.set_window_size(1280, 900)
        driver.get(url)
        title = driver.title
        driver.save_screenshot(SHOT)
    finally:
        driver.quit()

    size = os.path.getsize(SHOT)
    peak = cgroup_number(["memory.peak", "memory/memory.max_usage_in_bytes"])
    usec = cgroup_number(["cpu.stat"], "usage_usec")
    result = {
        "url": url, "title": title, "screenshot_bytes": size,
        "elapsed_seconds": round(time.time() - started, 1),
        "peak_memory_bytes": peak,
        "cpu_seconds": round(usec / 1e6, 1) if usec is not None else None,
    }
    ok = size > 5000 and expect.lower() in title.lower()

    upload = os.environ.get("PROBE_UPLOAD_URL")
    if upload:
        headers = {"Content-Type": "image/png", "X-Elapsed-Seconds": str(result["elapsed_seconds"])}
        if peak is not None:
            headers["X-Peak-Memory-Bytes"] = str(peak)
        if result["cpu_seconds"] is not None:
            headers["X-Cpu-Seconds"] = str(result["cpu_seconds"])
        with open(SHOT, "rb") as f:
            request = urllib.request.Request(upload, data=f.read(), headers=headers, method="POST")
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                result["upload"] = response.status
        except OSError as err:
            result["upload"] = str(err)
            ok = False

    print(json.dumps(result))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
