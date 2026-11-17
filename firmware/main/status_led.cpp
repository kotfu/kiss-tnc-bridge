#include "status_led.h"

#include <cmath>
#include "radio.h"
#include "settings.h"

namespace ktb::status_led {

namespace {

// The ATOM's breath: 8 s, starting bright, down to 15% (ui.cpp).
constexpr uint32_t breathe_ms = 8000;
constexpr float breathe_low = 0.15f;

// A blink: dark this long for a frame to or from the radio, then lit at
// least as long before the next, so steady traffic flickers rather than
// holding it dark. A frame in the lit gap blinks once the gap ends.
constexpr int64_t blink_dark_ms = 100;
constexpr int64_t blink_lit_ms = 100;

int64_t since = -1;     // when it was lit, for the breath
uint32_t frames = 0;    // radio::frames() last seen
bool pending = false;   // a frame seen, not yet blinked for
int64_t dark_until = 0; // a blink, until then
int64_t next_blink = 0; // the earliest the next blink can start

} // namespace

float level(int64_t now, bool can_light, bool screen_dark, const Levels &levels)
{
    uint32_t f = radio::frames();
    if (f != frames) {
        frames = f;
        pending = true;
    }
    bool lit = false;
    if (can_light) {
        switch (settings::led_when()) {
        case settings::LedWhen::always: lit = true; break;
        case settings::LedWhen::screen_dark: lit = screen_dark; break;
        case settings::LedWhen::never: break;
        }
    }
    if (!lit) {
        since = -1;
        pending = false;
        dark_until = 0;
        return 0;
    }
    if (since < 0) {
        since = now;
    }
    if (!settings::led_activity_blink()) {
        pending = false;
    } else if (pending && now >= next_blink) {
        pending = false;
        dark_until = now + blink_dark_ms;
        next_blink = dark_until + blink_lit_ms;
    }
    if (now < dark_until) {
        return 0;
    }
    switch (settings::led_pattern()) {
    case settings::LedPattern::bright: return levels.bright;
    case settings::LedPattern::dim: return levels.dim;
    case settings::LedPattern::breathe: break;
    }
    float phase = static_cast<float>((now - since) % breathe_ms) / breathe_ms;
    return levels.bright *
           (breathe_low + (1 - breathe_low) * (0.5f + 0.5f * cosf(2 * static_cast<float>(M_PI) * phase)));
}

std::string line()
{
    if (settings::led_when() == settings::LedWhen::never) {
        return "Off";
    }
    std::string shown;
    switch (settings::led_pattern()) {
    case settings::LedPattern::bright: shown = "Bright"; break;
    case settings::LedPattern::dim: shown = "Dim"; break;
    default: shown = "Breathe"; break;
    }
    shown += settings::led_when() == settings::LedWhen::always ? ", always" : ", screen dark";
    if (settings::led_activity_blink()) {
        shown += ", blink";
    }
    return shown;
}

} // namespace ktb::status_led
