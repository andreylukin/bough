# bough orb base: the CLIs a project session needs to act as the user.
# Every project without its own `base` builds its setup.sh on top of this.
# Versions follow the user's host where the major version matters.
FROM docker.io/library/debian:bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git gnupg jq less make openssh-client procps \
      python3 python3-pip python3-venv unzip xz-utils \
 && rm -rf /var/lib/apt/lists/*

# gh
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
 && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
 && apt-get update && apt-get install -y --no-install-recommends gh && rm -rf /var/lib/apt/lists/*

# aws v2
RUN curl -fsSL "https://awscli.amazonaws.com/awscli-exe-linux-$(uname -m).zip" -o /tmp/aws.zip \
 && unzip -q /tmp/aws.zip -d /tmp && /tmp/aws/install && rm -rf /tmp/aws /tmp/aws.zip

ARG HELM=4.2.0
ARG HELMFILE=1.5.2
ARG SOPS=3.13.1
ARG JUST=1.51.0
# gcx shares the host's mounted config; a newer gcx migrates it to a
# format the host's older gcx cannot read, so it must match the host.
ARG GCX=0.2.14
RUN set -e; A=$(dpkg --print-architecture); \
    curl -fsSL "https://dl.k8s.io/release/$(curl -fsSL https://dl.k8s.io/release/stable.txt)/bin/linux/$A/kubectl" -o /usr/local/bin/kubectl; \
    curl -fsSL "https://get.helm.sh/helm-v$HELM-linux-$A.tar.gz" | tar -xz -C /tmp && mv /tmp/linux-$A/helm /usr/local/bin/; \
    curl -fsSL "https://github.com/helmfile/helmfile/releases/download/v$HELMFILE/helmfile_${HELMFILE}_linux_$A.tar.gz" | tar -xz -C /usr/local/bin helmfile; \
    curl -fsSL "https://github.com/getsops/sops/releases/download/v$SOPS/sops-v$SOPS.linux.$A" -o /usr/local/bin/sops; \
    curl -fsSL "https://github.com/casey/just/releases/download/$JUST/just-$JUST-$(uname -m)-unknown-linux-musl.tar.gz" | tar -xz -C /usr/local/bin just; \
    curl -fsSL "https://github.com/argoproj/argo-cd/releases/latest/download/argocd-linux-$A" -o /usr/local/bin/argocd; \
    chmod +x /usr/local/bin/kubectl /usr/local/bin/sops /usr/local/bin/argocd; \
    curl -fsSL https://raw.githubusercontent.com/grafana/gcx/main/scripts/install.sh | GCX_VERSION=$GCX GCX_INSTALL_DIR=/usr/local/bin sh; \
    curl -fsSL https://astral.sh/uv/install.sh | UV_INSTALL_DIR=/usr/local/bin UV_NO_MODIFY_PATH=1 sh; \
    rm -rf /tmp/linux-$A

# go: official tarball, checksum per arch.
ARG GO=1.27.0
ARG GO_SHA_ARM64=51798d2c42d0e1c6ed7fd9f48728b4193abac9e8aad6dbac2fe96a81f5909bda
ARG GO_SHA_AMD64=675c26c449cbb18fc24b74650de1eabbae6e16f64326fd85a283fb3b58280685
RUN set -e; A=$(dpkg --print-architecture); \
    case "$A" in arm64) S=$GO_SHA_ARM64 ;; amd64) S=$GO_SHA_AMD64 ;; *) echo "no go for $A" >&2; exit 1 ;; esac; \
    curl -fsSL --retry 3 "https://go.dev/dl/go$GO.linux-$A.tar.gz" -o /tmp/go.tgz; \
    echo "$S  /tmp/go.tgz" | sha256sum -c -; \
    tar -xzf /tmp/go.tgz -C /usr/local; rm /tmp/go.tgz; \
    ln -s /usr/local/go/bin/go /usr/local/go/bin/gofmt /usr/local/bin/

# parallel-cli: tools and their python outside /root, so no mount or cache hides them.
ARG PARALLEL=0.9.3
RUN UV_TOOL_DIR=/opt/uv-tools UV_TOOL_BIN_DIR=/usr/local/bin UV_PYTHON_INSTALL_DIR=/opt/uv-python \
      uv tool install --python 3.13 "parallel-web-tools[cli]==$PARALLEL" \
 && parallel-cli --version
