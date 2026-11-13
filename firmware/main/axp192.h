// The AXP192 power chip on the Core2 and the StickC Plus SE, on the
// internal I2C bus at 0x34: the power rails, the backlight, the battery,
// the power button and power off (docs/core2-ui.md; the roadmap's Hardware
// notes). The boards wire it differently: the Core2's backlight is DCDC3
// and its USB comes in on ACIN, the StickC's backlight is LDO2 and its USB
// comes in on VBUS.
#pragma once

#include <cstdint>

#include "driver/i2c_master.h"
#include "sdkconfig.h"

namespace ktb::axp192 {

// Sets up the rails the screen needs (and the Core2's touch), with the
// backlight off; on the Core2 it also resets the screen through the
// chip's GPIO4. False if the chip didn't answer.
bool init(i2c_master_bus_handle_t bus);

#if CONFIG_KTB_BOARD_CORE2
// The backlight is the chip's DCDC3: its voltage sets the brightness, and
// off switches the rail off.
enum class Backlight : uint8_t { off, dim, full };
void backlight(Backlight b);
#else
// The backlight is the chip's LDO2: 0 switches it off, 1 to 100 sets its
// voltage (led_brightness). Writes the chip only on a change.
void backlight(uint8_t brightness);
#endif

struct Power {
    bool external = false; // 5 V on USB: charging or full
    bool charging = false;
    bool battery = false;  // a battery is connected
    float battery_v = 0;
    float charge_ma = 0;
    float discharge_ma = 0;
    int percent = 0; // an estimate from the voltage: the AXP192 has no fuel gauge
};
Power read();

// The power button's short presses since the last call. A long hold
// (6 s) cuts the power in the chip, whatever the firmware does.
int short_presses();

// Has the chip cut the power. Doesn't return if it works.
void power_off();

} // namespace ktb::axp192
