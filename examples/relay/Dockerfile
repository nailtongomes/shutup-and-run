# The relay of internet sharing: ghcr.io/nailtongomes/shutup-and-run-relay.
# The release workflow copies the release's static musl binary here, checked
# against its SHA256SUMS, and pushes the image as <version> and latest.
# To build one yourself, put a musl binary here as ./shutup-and-run (AGENTS.md)
# and run `docker build -t shutup-and-run-relay .`.
FROM scratch
COPY shutup-and-run /shutup-and-run
EXPOSE 7000 8080
LABEL org.opencontainers.image.source="https://github.com/nailtongomes/shutup-and-run"
LABEL org.opencontainers.image.description="Relay of shutup-and-run internet sharing: PCs dial in on 7000, containers use the HTTP proxy on 8080"
LABEL org.opencontainers.image.licenses="MIT"
ENTRYPOINT ["/shutup-and-run", "relay"]
# The token comes from SHUTUP_AND_RUN_RELAY_TOKEN (or --token-file).
CMD ["--agents=0.0.0.0:7000", "--proxy=0.0.0.0:8080"]
