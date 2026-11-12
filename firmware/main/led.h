// The ATOM Lite's one SK6812 RGB LED. On boards without one (board.h),
// init() and show() do nothing.
#pragma once

#include <cstdint>

namespace ktb::led {

struct Rgb {
    uint8_t r, g, b;
    bool operator==(const Rgb &o) const { return r == o.r && g == o.g && b == o.b; }
};

// The palette (docs/atom-lite-ui.md). Values are at full brightness, before
// led_brightness scales them; tuned by eye on the device.
constexpr Rgb off{0, 0, 0};
constexpr Rgb red{255, 0, 0};
constexpr Rgb yellow{255, 150, 0};
constexpr Rgb green{0, 255, 0};
constexpr Rgb cyan{0, 220, 255};
constexpr Rgb blue{0, 0, 255};
constexpr Rgb magenta{255, 0, 255};
constexpr Rgb white{255, 255, 255};
// The frame flick to the radio. Pure white flicked on top of green looked
// magenta: only red and blue change, and an eye used to green sees white
// short of green. Extra green makes up for it.
constexpr Rgb flick{170, 255, 170};

void init();

// Shows a color at a brightness (0-100, led_brightness), times a level
// (0-1) for breathing and the frame dip. Writes the LED only on a change.
void show(Rgb color, uint8_t brightness, float level = 1.0f);

} // namespace ktb::led
