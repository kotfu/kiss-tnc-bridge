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
enum class LedNormal : uint8_t { bright, breathe, dark, dim };

// A board with a screen (board::screen_settings) has these in place of
// the LED's (docs/core2-ui.md, The screen). Stored in NVS: add new ones
// at the end.
enum class ScreenNormal : uint8_t { bright, dim, dark };

// A board with a status LED (board::status_led: the StickC's red, the
// Core2's green) has
// these three, each on its own: when it's lit, how, and whether it goes
// dark for a moment on each frame to or from the radio. Stored in NVS:
// add new ones at the end.
enum class LedWhen : uint8_t { always, screen_dark, never };
enum class LedPattern : uint8_t { bright, dim, breathe };

// Reads the settings from NVS, with defaults for any that aren't there.
void load();

// The current values. Safe from any task.
std::string public_name();
LedNormal led_normal();
uint16_t led_wake_seconds();
uint8_t led_brightness();
ScreenNormal screen_normal();
uint16_t screen_timeout_seconds();
LedWhen led_when();
LedPattern led_pattern();
bool led_activity_blink();

// Every setting, as device.settings.get returns them.
cJSON *to_json();

// Applies the changes in obj (a JSON object of key: value). Either every
// change is valid and saved, or nothing changes and error says why.
// reboot_required lists the keys that take effect only after a reboot.
bool apply(const cJSON *obj, std::string &error, std::vector<std::string> &reboot_required);

// The NVS namespace, for reset.
constexpr const char *nvs_namespace = "settings";

} // namespace ktb::settings
