# A heavy, real job for testing a worker: SeleniumBase drives headless Chrome
# to a page, takes a screenshot, and exits 0 if that worked. No secret, no
# token, no client data. Published as
# ghcr.io/nailtongomes/shutup-and-run-selenium-probe. linux/amd64 only.
FROM python:3.12-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends wget ca-certificates fonts-liberation \
 && wget -q -O /tmp/chrome.deb https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb \
 && apt-get install -y --no-install-recommends /tmp/chrome.deb \
 && rm -rf /tmp/chrome.deb /var/lib/apt/lists/*

# The driver is fetched now, for the Chrome just installed, so a job does not
# download anything.
RUN pip install --no-cache-dir seleniumbase==4.57.1 \
 && seleniumbase get chromedriver

COPY probe.py /probe.py
# CMD, not ENTRYPOINT: a task's `command` replaces it.
CMD ["python", "/probe.py"]
