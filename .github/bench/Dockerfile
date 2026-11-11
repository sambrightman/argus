# The criterion benchmarks, prebuilt, for Bencher's bare-metal runners
# (linux/amd64, no network). They build their fixtures with git.
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends git \
    && rm -rf /var/lib/apt/lists/*
COPY bin/ /bench/
COPY run.sh /bench/run.sh
# criterion writes its reports under the working directory
WORKDIR /tmp
CMD ["/bench/run.sh"]
