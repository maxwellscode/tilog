# A Linux build environment for tilog: Rust, and the tools its tests and demos use.
FROM mcr.microsoft.com/devcontainers/rust:1-bookworm

# tmux: to try the screen in a terminal that can be scripted. openssh-client: for ssh sources.
# gzip is there already; procps gives `ps` and `pgrep`.
RUN apt-get update \
    && apt-get install -y --no-install-recommends tmux openssh-client procps \
    && rm -rf /var/lib/apt/lists/*
