// A status LED from its three settings, led_when, led_pattern and
// led_activity_blink (docs/stickc-ui.md, The red LED): the StickC's red
// and the Core2's green. Each board's UI calls level() from one LVGL
// timer and drives its own LED with the answer.
#pragma once

#include <cstdint>
#include <string>

namespace ktb::status_led {

// How bright the LED is, of its full, for led_pattern's bright and dim;
// breathe peaks at bright.
struct Levels {
    float bright;
    float dim;
};

// The LED's level now, 0 to 1 of its full. can_light is false while it
// must be off whatever the settings say (starting, installing, powering
// off); screen_dark is the backlight at 0, whatever turned it off. Call
// it every 40 ms or so: it watches the radio's frames for blinks.
float level(int64_t now_ms, bool can_light, bool screen_dark, const Levels &levels);

// "Breathe, screen dark, blink", "Dim, always", "Off": the settings on
// one line, for a System page.
std::string line();

} // namespace ktb::status_led
