// The Core2's screen hardware: the internal I2C bus with the AXP192 and
// the touch controller, the ILI9342 screen on SPI, and LVGL on top
// through esp_lvgl_port, which runs LVGL in its own task.
#pragma once

#include <cstdint>

#include "lvgl.h"

namespace ktb::core2 {

// The screen and touch area: the touch panel runs past the screen's 240
// rows, over the three circles.
constexpr int width = 320;
constexpr int height = 240;

// Sets up the AXP192, the screen (black, backlight off), touch and LVGL.
// False if something didn't start; the log says what.
bool start();

struct TouchPoint {
    uint16_t x;
    uint16_t y; // 240 and over: the strip with the circles
};
// Reads the touch controller: up to max points, now.
int read_touch(TouchPoint *points, int max);

lv_display_t *display();

// LVGL from a task other than its own.
bool lock(uint32_t timeout_ms = 0);
void unlock();

} // namespace ktb::core2
