# The relay of internet sharing: the release's static musl binary on scratch.
# The download is checked against the release's SHA256SUMS.
FROM alpine:3.20 AS get
ARG RELEASE=https://github.com/nailtongomes/shutup-and-run/releases/latest/download
WORKDIR /get
RUN wget -q "$RELEASE/shutup-and-run-x86_64-unknown-linux-musl" "$RELEASE/SHA256SUMS" \
 && grep ' shutup-and-run-x86_64-unknown-linux-musl$' SHA256SUMS | sha256sum -c - \
 && chmod 755 shutup-and-run-x86_64-unknown-linux-musl

FROM scratch
COPY --from=get /get/shutup-and-run-x86_64-unknown-linux-musl /shutup-and-run
ENTRYPOINT ["/shutup-and-run", "relay"]
