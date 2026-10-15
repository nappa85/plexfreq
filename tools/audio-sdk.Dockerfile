# Pinned by repo digest (resolved from coderus/sailfishos-platform-sdk-aarch64:latest
# on 2026-10-15); update deliberately and re-check Qt/glibc/target versions.
FROM coderus/sailfishos-platform-sdk-aarch64@sha256:e5f7596d14502746b308c3dde36f65fc25d7f76c45a01acaed56a1f55b76f592
RUN sb2 -t SailfishOS-5.2.0.15-aarch64 -m sdk-install -R zypper --non-interactive install --no-recommends gstreamer1.0-devel gstreamer1.0-plugins-base-devel pulseaudio-devel
