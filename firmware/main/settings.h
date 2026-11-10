// Per-device settings, kept in NVS (docs/control-api.md,
// device.settings.get and device.settings.set). Reset erases them, and
// every one goes back to its default.
#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "cJSON.h"

namespace ktb::settings {

// The values are stored in NVS: add new ones at the end.
enum class LedNormal : uint8_t { solid, breathe, dark, dim };

// Reads the settings from NVS, with defaults for any that aren't there.
void load();

// The current values. Safe from any task.
std::string public_name();
LedNormal led_normal();
uint16_t led_wake_seconds();
uint8_t led_brightness();

// Every setting, as device.settings.get returns them.
cJSON *to_json();

// Applies the changes in obj (a JSON object of key: value). Either every
// change is valid and saved, or nothing changes and error says why.
// reboot_required lists the keys that take effect only after a reboot.
bool apply(const cJSON *obj, std::string &error, std::vector<std::string> &reboot_required);

// The NVS namespace, for reset.
constexpr const char *nvs_namespace = "settings";

} // namespace ktb::settings
