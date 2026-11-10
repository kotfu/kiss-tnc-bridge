// The board this firmware is built for. Only the ATOM Lite so far; the
// Core2 comes in Phase G.
#pragma once

#include "driver/gpio.h"

namespace ktb::board {

// device.info's "board"
constexpr const char *name = "atom-lite";

// The button: active low, with a pull-up on the board. GPIO39 is input
// only and isn't a strapping pin, so holding it at power-on is safe.
constexpr gpio_num_t button = GPIO_NUM_39;

// One SK6812 RGB LED, under the button.
constexpr gpio_num_t led = GPIO_NUM_27;

} // namespace ktb::board
