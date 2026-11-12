# kiss-tnc-bridge firmware for the M5Stack ATOM Lite, StickC Plus SE and Core2.
# One project for every board; a board's sdkconfig.<board> picks it.
# See README.md, and docs/roadmap.md for the plan.
cmake_minimum_required(VERSION 3.22)

# The version comes from version.txt, which ESP-IDF reads when PROJECT_VER
# isn't given. Release builds pass -DPROJECT_VER from the firmware-vX.Y.Z tag.
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(ktb-firmware)
