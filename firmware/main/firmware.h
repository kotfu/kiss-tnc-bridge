// The running image: its version, its partition, and rollback
// (docs/roadmap.md, Firmware updates).
#pragma once

#include <cstdint>

#include "cJSON.h"

namespace ktb::firmware {

// How long a new image has to mark itself valid on its first boot. A hang
// wouldn't roll back on its own, so it restarts, and the bootloader goes
// back to the previous image.
constexpr uint32_t self_test_deadline_s = 60; // provisional

// Logs what's running and, on a new image's first boot, starts the
// deadline. Call early.
void check_boot();

// The self-test passed: marks a new image valid. Harmless on any other boot.
void self_test_passed();

// device.info's "firmware" object.
cJSON *info_json();

} // namespace ktb::firmware
