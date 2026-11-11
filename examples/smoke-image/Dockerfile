# A harmless image for scripts/smoke.sh. It has sh, sleep and Python.
# With no command it runs check.py, which prints one line and exits 0.
FROM python:3.12-alpine
COPY check.py /check.py
LABEL org.opencontainers.image.source="https://github.com/nailtongomes/shutup-and-run"
LABEL org.opencontainers.image.description="Smoke test job for shutup-and-run"
CMD ["python", "/check.py"]
