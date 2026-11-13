# Spike: bring up the StickC Plus SE

Phase G (roadmap, StickC Plus SE firmware; issue #21). What the hardware
really is, before designing the UI, and what LVGL costs on a chip with no
PSRAM. Run 12 November 2026 on the project's StickC Plus SE (USB serial
number `C5529161EE`), after backing up its factory firmware
(`firmware/backups/`).

## Answer

The hardware is as M5Stack documents it, plus a real-time clock it
doesn't mention, and minus a usable buzzer. LVGL fits: about 361 KB of
flash and 37 KB of internal RAM for a status screen, which would leave the
StickC with about as much heap as the Core2 has in the same situation.

## The tool

```
. ~/src/esp/esp-idf/export.sh
idf.py build                                   # the bring-up tests
idf.py -p PORT -b 1500000 flash
idf.py -B build-lvgl -DSDKCONFIG=build-lvgl/sdkconfig \
  -DSDKCONFIG_DEFAULTS="sdkconfig.defaults;sdkconfig.lvgl" build   # the LVGL measurement
```

The bring-up build scans the I2C bus, dumps the AXP192 as the factory
firmware left it (writing only to switch the screen's rails on if they're
off), fills the screen with colours and then a border with coloured
corners, blinks the LED, plays tones, and then logs buttons A and B (B
plays tones), the power button and the battery. The LVGL build draws a
mock status screen instead of the colour tests, and logs the heap at each
step. Flash at 1500000; the CH552 takes writes at that rate, not reads.

## Findings

### I2C and power

- I2C on SDA G21, SCL G22: the AXP192 at `0x34`, and a **BM8563
  real-time clock at `0x51`**, which M5Stack's page doesn't list.
- The AXP192 as the factory firmware leaves it (it keeps its settings
  through an ESP32 reset):

  | Register | Value | Means |
  |---|---|---|
  | `0x00` | `0x3d` | USB on **VBUS** (bit 5), not ACIN as on the Core2 |
  | `0x01` | `0x70` | Charging, battery present |
  | `0x12` | `0x5f` | DCDC1, DCDC2, DCDC3, LDO2, LDO3 and EXTEN on |
  | `0x26` | `0x68` | DCDC1 (the ESP32) at 3.3 V |
  | `0x28` | `0xac` | LDO2 at 2.8 V (the backlight), LDO3 at 3.0 V (the screen) |
  | `0x33` | `0xc0` | Charging to 4.2 V at 100 mA |
  | `0x36` | `0x0c` | Power button: off after 4 s held |
  | `0x82` | `0xff` | Every ADC on |

- Battery: 3.95 to 4.05 V while charging from USB at about 86 mA.

### The screen

ST7789v2 on SPI2 (MOSI G15, CLK G13, DC G23, RST G18, CS G5) at 40 MHz,
135×240, offset 52 by 40, colours inverted, RGB order. With the USB port
at the bottom: red, green and blue came out as named, the corners where
they were drawn, and a 1-pixel border visible on every edge. The
backlight is LDO2, lit as the factory firmware left it.

### LED, buttons, buzzer

- The red LED on G10, active low: three blinks seen.
- Buttons, with the USB end towards you:

  | Button | Where | Wired to |
  |---|---|---|
  | A | The large button on the face, near the USB end | G37, low when pressed |
  | B | The right side, at the end away from USB | G39, low when pressed |
  | Power | The left side, near the USB end | The AXP192: short and long presses as interrupts (long at about 1 s) |

- The passive buzzer on G2 is **too quiet to use**: at 50% duty (as loud
  as a GPIO can drive it), 2.0 and 2.7 kHz were heard only a few inches
  away and 4 kHz not at all; 880 to 3520 Hz tones, 200 ms each, weren't
  noticed. The UI uses no sound.

### LVGL

LVGL 9 through `esp_lvgl_port`, as on the Core2, with the Core2's fonts
(Montserrat 12, 14, 16, 20) and dark theme, but its memory from the
ordinary heap (`CONFIG_LV_USE_CLIB_MALLOC`): the Core2 puts it in PSRAM,
which the StickC hasn't got.

- **Flash:** the image grows from 217 KB to 587 KB: **about 361 KB**, at
  the firmware's own optimisation (ESP-IDF's default). The ATOM Lite's
  firmware 1.2.0 is 1.09 MB in a 1.88 MB app partition, so the StickC's
  would leave about 430 KB.
- **Internal RAM:**

  | Step | Free heap | Cost |
  |---|---|---|
  | Start (LVGL's static data is in the image) | 295,016 | 7.4 KB against the build without LVGL |
  | `lvgl_port_init`: its task and timers | 281,936 | 9.6 KB |
  | The display, two 20-row strips (10.8 KB) | 266,172 | 15.8 KB |
  | A status screen: a bar, five labels, two buttons | 263,136 | 3.0 KB (its lowest 262,300) |
  | Five redraws later | 263,136 | nothing more |

  **About 37 KB in all.** The ATOM Lite's firmware has about 112 KB free
  with a TD-H9 and a phone, so the StickC would have about 75 KB: what the
  Core2 has (74.8 KB) with the same load. One strip instead of two would
  save 5.4 KB.

## Not tried

- The bridge itself with the screen running: the heap with Bluetooth up
  is an estimate from the ATOM Lite's.
- The backlight's brightness levels (LDO2's voltage), and the screen off.
- The BM8563 clock.
- Running on battery alone, and how long it lasts.
- Powering off from the firmware.

## The board now

It runs this spike (the LVGL build). The factory firmware is in
`firmware/backups/`, with how to restore it.
