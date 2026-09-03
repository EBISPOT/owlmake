# syntax=docker/dockerfile:1

# owlmake in a minimal Alpine image, with PATH shims so a repository's existing
# CI, which invokes `robot`, `jq`, `sssom` and `make` by name, resolves them to om.
#
# Two images are produced from this file:
#   * default target       — om only, no language runtime (~38 MB)
#   * target `with-python`  — adds a Python 3 runtime for build steps that
#                             shell out to Python, e.g. uPheno, plus `git` for
#                             steps that diff against a release
# Most ontologies (EFO, CL, UBERON, …) build with the default image; only the
# minority that run Python scripts, or whose non-release targets shell out to
# git, need `with-python`.
#
#   docker build -t owlmake .                        # default (slim)
#   docker build -t owlmake:python --target with-python .
#
# The builder always runs on the native build arch and cross-compiles to the
# requested target arch with cargo-zigbuild, so multi-arch images build WITHOUT
# emulating the (heavy) Rust compile under QEMU — only the tiny final stage runs
# under the target architecture.

# ---- build: cross-compile a static musl binary for the target arch ----
FROM --platform=$BUILDPLATFORM ghcr.io/rust-cross/cargo-zigbuild:0.22.3 AS build
ARG TARGETARCH
RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-musl ;; \
      arm64) target=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    echo "$target" > /tmp/target; \
    rustup target add "$target"
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target,sharing=locked \
    set -eux; \
    target="$(cat /tmp/target)"; \
    cargo zigbuild --release --locked --target "$target"; \
    install -Dm755 "target/$target/release/om" /out/om

# ---- base: minimal Alpine + om + tool shims (shared by both images) ----
FROM alpine:3.24 AS base
# ca-certificates: owlmake fetches imports over HTTPS and trusts the system CA
# store (ureq native-certs).
#
# bash: a GitHub Actions container job runs every step inside this image, and a
# step that shells out finds no interpreter without it — `actions-js/push` opens
# with `bash start.sh` and dies on `spawn bash ENOENT`, which is how EFO's
# ID-allocation workflow failed after minting its IDs. Repository scripts assume
# it just as readily. It costs ~4 MB with readline and ncurses-libs, so unlike
# git (~13 MB, see the python stage) it is cheap enough for both images.
RUN apk add --no-cache ca-certificates bash

COPY --from=build /out/om /usr/local/bin/om

# PATH shims on the regular PATH: an ontology repository's scripts and CI call
# these tool names directly, so each is routed to the single om binary. om's own
# top-level commands (chaining included) cover the surface those names are called
# with, and its jq and SSSOM engines are subcommands. The `make` shim routes to
# `om make`, which resolves the repo's targets and runs the one asked for
# natively (`VAR=value` arguments and all) — so CI that calls `make test …` /
# `make cl-base.owl` runs on this image with no workflow changes.
RUN set -eux; \
    printf '#!/bin/sh\nexec om "$@"\n'       > /usr/local/bin/robot; \
    printf '#!/bin/sh\nexec om jq "$@"\n'    > /usr/local/bin/jq; \
    printf '#!/bin/sh\nexec om sssom "$@"\n' > /usr/local/bin/sssom; \
    printf '#!/bin/sh\nexec om make "$@"\n'  > /usr/local/bin/make; \
    chmod 0755 /usr/local/bin/robot /usr/local/bin/jq /usr/local/bin/sssom /usr/local/bin/make; \
    # the binary and every shim must be present and executable on the PATH
    for t in om robot jq sssom make; do command -v "$t" >/dev/null; test -x "$(command -v "$t")"; done

# An empty plugin directory, at the path repositories expect. om implements the
# namespaced commands it supports natively (`kgcl:mint`, `odk:*`, `uberon:*`) and
# loads nothing from here, but repository scripts and CI steps reference the
# location — EFO's ID-allocation step lists it before invoking `kgcl:mint`. With
# the variable unset that expands to `ls ""`, which fails the step under `set -e`;
# an existing empty directory and an exported path make such references no-ops.
RUN mkdir -p /tools/robot-plugins
ENV ROBOT_PLUGINS_DIRECTORY=/tools/robot-plugins

# Run as a non-root user: a runtime stage with no USER defaults to root
# regardless of any user set in earlier build stages. Own /work so recipe
# steps can write build artifacts there.
RUN addgroup -S owlmake \
    && adduser -S -G owlmake -h /work owlmake \
    && mkdir -p /work \
    && chown owlmake:owlmake /work
USER owlmake

WORKDIR /work
# No forced ENTRYPOINT: om (and the compatibility shims) are on PATH, so the
# image is a normal environment — `docker run IMG om prepare-release`,
# `docker run IMG om diff …`, `docker run IMG sh`. A bare run prints om help.
CMD ["om", "--help"]

# ---- with-python: base + the interpreters a repository's own scripts need ----
# py3-pandas covers the common case (uPheno and friends build tables with it);
# it pulls in NumPy and its OpenBLAS runtime, which is most of this layer's size.
#
# git is here and not in the slim image: build steps use it to diff an edit file
# against a release or to fetch a branch's version of one (EFO has five such
# steps, which is why a plan records `requires: [git]` for them), but it costs
# ~13 MB — 7 MB of git and the rest its HTTPS stack, a third of the slim image
# for something no release build needs. Against this layer it is noise.
FROM base AS with-python
USER root
RUN apk add --no-cache python3 py3-pandas git
USER owlmake

# ---- default target: the slim image (om only) ----
# Kept last so `docker build` with no --target produces the smaller image.
FROM base AS slim
