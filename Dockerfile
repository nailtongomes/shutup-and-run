# Static musl binary. CI copies it here before `docker build`.
# Mounting the host Docker socket into this image is root on the host.
FROM scratch
COPY shutup-and-run /shutup-and-run
EXPOSE 8787
LABEL org.opencontainers.image.source="https://github.com/nailtongomes/shutup-and-run"
LABEL org.opencontainers.image.description="On-demand container slots"
ENTRYPOINT ["/shutup-and-run"]
