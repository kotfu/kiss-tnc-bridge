// The ATOM Lite's one SK6812 RGB LED (led.cpp). On the StickC Plus SE the
// screen stands in for it (stickc_page.cpp). On boards with
// neither (board.h), init() and show() do nothing.
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

// The StickC Plus SE's start-up screen, as the Core2's: the step under
// way, shown once it's slow, and the end of starting, when the pages
// replace it. ui::starting() and ui::started() pass them on.
void starting(const char *step);
void started();

// The StickC's installing screen, as the Core2's (ui::show_installing()).
bool show_installing(const char *version, uint32_t timeout_ms);

// Wakes the StickC's screen, as a press does: for device.identify, as on
// the Core2. Safe from any task.
void wake();

// The StickC's pairing screen: while it's up (pairing, or its result
// until A says OK) A's presses are its, not the ATOM's, and press_a()
// passes each one on when it's let go. Safe from any task.
bool takes_a();
void press_a();

// A's double press started a pairing (before radio::start_pairing()), and
// how a pairing ended (ui::post's pairing events): the screen shows only
// the pairings A started, and their results. Safe from any task.
void pairing_started();
void pairing_result(bool ok);

// The StickC's reset at power-on (ui::reset_requested_at_boot()), as the
// Core2's words: the warning with its bar, progress 0 to 1, while A is
// held; the start-up screen again if A is let go; and the reset done,
// before the restart.
void show_reset_warning(float progress);
void show_reset_cancelled();
void show_reset_done();

} // namespace ktb::led
