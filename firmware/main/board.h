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

// The UI is the LED and the button (docs/atom-lite-ui.md).
constexpr bool screen = false;

#elif CONFIG_KTB_BOARD_STICKC_PLUS_SE

constexpr const char *name = "stickc-plus-se";

// Until its own UI is designed it runs the ATOM's:
// button A (the large button on the face) is the ATOM's button, and the
// whole screen is its LED (stickc_screen.cpp). A is active low with a
// pull-up on the board; GPIO37 is input only and isn't a strapping pin.
constexpr gpio_num_t button = GPIO_NUM_37;

// No RGB LED: the screen stands in for it, and the red LED (GPIO10) is
// kept off.
constexpr gpio_num_t rgb_led = GPIO_NUM_NC;

// No screen UI yet: the ATOM's settings (led_*) apply, not the Core2's.
constexpr bool screen = false;

#elif CONFIG_KTB_BOARD_CORE2

constexpr const char *name = "core2-v1.3";

// No button: the touch screen and the circles below it.
constexpr gpio_num_t button = GPIO_NUM_NC;

// No RGB LED: the screen shows state.
constexpr gpio_num_t rgb_led = GPIO_NUM_NC;

// The UI is the touch screen (docs/core2-ui.md, core2_ui.cpp).
constexpr bool screen = true;

#else
#error "A board in Kconfig.projbuild has no section in board.h"
#endif

} // namespace ktb::board
