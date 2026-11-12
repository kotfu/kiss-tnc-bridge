// The board this firmware is built for, picked by CONFIG_KTB_BOARD_* in the
// board's sdkconfig.<board> file. Everything else is shared.
#pragma once

#include "driver/gpio.h"
#include "sdkconfig.h"

namespace ktb::board {

#if CONFIG_KTB_BOARD_ATOM_LITE

// device.info's "board"
constexpr const char *name = "atom-lite";

// The button: active low, with a pull-up on the board. GPIO39 is input
// only and isn't a strapping pin, so holding it at power-on is safe.
constexpr gpio_num_t button = GPIO_NUM_39;

// One SK6812 RGB LED, under the button.
constexpr gpio_num_t rgb_led = GPIO_NUM_27;

#elif CONFIG_KTB_BOARD_STICKC_PLUS_SE

constexpr const char *name = "stickc-plus-se";

// Until its own UI is designed: button A (the M5 button on the front)
// works as the ATOM's button. Active low with a pull-up on the board;
// GPIO37 is input only and isn't a strapping pin. Not tried on a device.
constexpr gpio_num_t button = GPIO_NUM_37;

// No RGB LED: one red LED (GPIO10) and a screen, both still to design.
constexpr gpio_num_t rgb_led = GPIO_NUM_NC;

#elif CONFIG_KTB_BOARD_CORE2

constexpr const char *name = "core2";

// No physical button: the touch buttons under the screen come with its
// UI. Until then a radio is paired over USB.
constexpr gpio_num_t button = GPIO_NUM_NC;

// No RGB LED: the screen shows state, once it's designed.
constexpr gpio_num_t rgb_led = GPIO_NUM_NC;

#else
#error "A board in Kconfig.projbuild has no section in board.h"
#endif

} // namespace ktb::board
