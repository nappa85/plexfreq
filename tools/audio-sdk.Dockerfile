FROM coderus/sailfishos-platform-sdk-aarch64:latest
RUN sb2 -t SailfishOS-5.2.0.15-aarch64 -m sdk-install -R zypper --non-interactive install --no-recommends gstreamer1.0-devel gstreamer1.0-plugins-base-devel pulseaudio-devel
