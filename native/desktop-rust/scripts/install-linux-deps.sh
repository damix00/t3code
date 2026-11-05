#!/usr/bin/env bash
# System libraries GPUI links against on Debian/Ubuntu (X11 + Wayland, fonts, Vulkan).
set -euo pipefail

packages=(
  build-essential
  pkg-config
  libfontconfig-dev
  libfreetype-dev
  libwayland-dev
  libx11-xcb-dev
  libxcb1-dev
  libxkbcommon-dev
  libxkbcommon-x11-dev
  libvulkan-dev
  libzstd-dev
)

sudo=""
if [ "$(id -u)" -ne 0 ]; then
  sudo="sudo"
fi

$sudo apt-get update
$sudo apt-get install -y --no-install-recommends "${packages[@]}"
