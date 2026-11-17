# StickC Plus SE user interface

The M5Stack StickC Plus SE has a 135×240 color screen, two buttons, a
power button and a red LED (see the roadmap's
[hardware notes](roadmap.md#m5stickc-plus-se), tried on a device). This
page is what the StickC's own UI does: three pages to watch the bridge,
the screen settings the Core2 has, and the ATOM Lite's button for what
you do on the device. Everything else is done from a management app over
the [control API](control-api.md), as on the ATOM Lite and the Core2.

It's drawn with LVGL, as the Core2's is ([core2-ui.md](core2-ui.md)).
Where this page says "as on the Core2", the Core2's page has the detail,
and the StickC does the same.

Status: being built in
[#28](https://github.com/kotfu/kiss-tnc-bridge/issues/28), page by page
on the device, 16 November 2026. Firmware 1.3 and 1.4 ran the ATOM
Lite's UI on the StickC, the whole screen standing in for the ATOM's
LED. The [mockups](stickc-mockups/index.html) are where the layout
started; where they and this page differ, this page wins. A polish
round comes before it's done.

## The device

Held in landscape, with the USB port at the right:

| Control | Where | What it does |
|---|---|---|
| **A** | The large button right of the screen | The ATOM Lite's button (see [The buttons](#the-buttons)) |
| **B** | The top edge, at the left end | The next page |
| **Power** | The bottom edge | A short press turns the screen off, or wakes it; held, it turns the device on, and off |
| **Red LED** | The left end | Breathes while the screen is off, in `breathe` (see [The screen](#the-screen)) |

A and B are GPIOs the firmware reads (A is GPIO37, B GPIO39). The power
button is the AXP192's, as on the Core2: the chip turns the device on,
reports short presses, and cuts the power when it's held, whatever the
firmware is doing.

| Power button | While | What happens | Whose |
|---|---|---|---|
| Hold about half a second | Off | Turns the StickC on | The AXP192's |
| Short press | On | Turns the screen off, or wakes it | The firmware's |
| Hold 6 s | On | Cuts the power | The AXP192's. The factory firmware sets 4 s; 6 s, as on the Core2, is harder to do by accident in a pocket |

## The buttons

**A is the ATOM Lite's button,** with its gestures and timings (see
[atom-lite-ui.md](atom-lite-ui.md#what-you-can-do-on-the-device)):

| Action | Gesture |
|---|---|
| Pair a Kenwood or select a BLE TNC | Double press |
| Cancel pairing | A press, while pairing |
| Start authorization mode | Hold 3 s |
| End authorization mode | A press, in authorization mode |
| Reset | Hold A while turning the StickC on (see [Reset](#reset)) |

A press of A also wakes the screen, and a double press or a hold does
what it does whatever the screen shows.

**B moves to the next page:** Bridge, TNC, System, and round to Bridge.
While the screen is off, a press of B only wakes it, on the page it was
on (see [The screen](#the-screen)). B is read every 20 ms, and a press
counts once it's been down for two reads: GPIO39 has a known erratum
that can glitch it.

## Layout

Every page has a **top bar**, 20 px, over a rule:

- At the left, beside B, the page marker, `1/3`, then the page's title,
  so B is the way to the others.
- At the right, the battery's percentage, then the
  [battery](#the-battery).

Pages are stacked top and bottom, never in left and right halves: a
radio's name or the device's name in half the width is cut off at six or
seven characters.

Text is Montserrat, LVGL's built-in font, at 12, 14 and 20 px.
Bluetooth addresses are in capitals. The background is black. Colors
mean what they mean on the Core2: green connected, amber waiting, blue
pairing, grey quiet.

## Bridge page

Where the device starts. The radio above a rule, KISS TNC apps below:

```
1/3 Bridge                    100% [■■]
---------------------------------------
TH-D75                           RX ■
Connected for 2h 14m             TX ■
---------------------------------------
KTB StickC
5C:8D:01:79:49:1A  12m            ■ ■
A4:C3:F0:2B:77:10  3m             ■ ■
```

- **The radio:** its name at 20 px, or "No radio". Under it, its state
  in color and how long, in the Core2's words: "Connected for 2h 14m",
  "Connecting for 5s", "Waiting for 5s" (a BLE TNC), "Retrying in 8s
  for 1m", "Pairing", "Stopped", or with no radio "Press A twice to pair
  a radio".
- **RX and TX,** at the right at 20 px while the radio is connected,
  each with a light as tall as the capitals: green for a frame from the
  radio, amber for a frame to it. A light shows for 300 ms. There are no
  counts: `ktbctl status` and a management app have them. The radio's
  name gives way to them.
- **The name KISS TNC apps see,** at 20 px.
- **Each app connected,** up to two: its address, how long it's been
  connected, and two lights at the right, green for a frame from the app
  and amber for a frame to it. The times start at one column, past the
  widest an address can be, so they line up whatever the addresses are.
  With none, "No apps connected (of 2)".

## TNC page

The radio the device has, and how to change it:

```
2/3 TNC                       100% [■■]
---------------------------------------
TH-D75
Kenwood, paired
1C:8F:57:BC:EF:26
Connected for 2h 14m
---------------------------------------
  Press A twice to choose another TNC.
```

- **Its name** at 20 px, or "No radio".
- **Its make and how the device has it:** the make (or "BLE KISS TNC"),
  then ", paired" or ", not paired" for a Kenwood, ", selected" for a
  BLE TNC. With no radio, "Nothing paired or selected".
- **Its Bluetooth address.**
- **Its state and how long,** as on the Bridge page.
- **Under the rule,** one line: "Press A twice to choose another TNC.",
  or with no radio "Press A twice to choose a TNC."

## System page

Only what's worth seeing on the device, one row each at 14 px, the name
in grey at the left and the value at the right. A management app reads
the rest.

| Row | What |
|---|---|
| Uptime | How long it's been running |
| Last run | How long the run before this one lasted, or "None yet" |
| Firmware | The version, `1.4.0` |
| Bluetooth | The device's own Bluetooth address |
| Screen | The [screen setting](#the-screen): "Dim after 30 s", "Dark after 1 min", "Breathe after 15 s", "Bright" |

## The battery

At the right of the top bar: the percentage, then the battery with its
level, as on the Core2:

| Power | The level |
|---|---|
| On battery | Grey |
| On battery, 20% or less | Red |
| External power, charging | Green, with a black stripe moving through it |
| External power, full | Solid green, 100% |

External power is USB, which comes in on the AXP192's VBUS input (on
the Core2 it's ACIN). The percentage is the Core2's estimate from the
voltage: the AXP192 has no fuel gauge. Once the charger stops on
external power, it shows 100%: a full StickC battery rests at about
4.135 V once the charge current stops, which the voltage curve calls
98%. The battery is 120 mAh, so it lasts an hour or two (provisional: to
measure).

## The screen

The Core2's modes and timeout, with the same settings, `screen_normal`
(`bright`, `dim`, `dark`, `breathe`) and `screen_timeout_seconds`, 30 s
by default (see [The screen](core2-ui.md#the-screen) on the Core2).
They're
[device settings](control-api.md#devicesettingsget-and-devicesettingsset),
in place of the ATOM's `led_*` settings the StickC had through firmware
1.4.0.

**The settings are changed from a management app** (or `ktbctl --device
PORT device settings set screen_normal=dark`), not on the device. The
System page's last row, **Screen**, shows them. The power button is the
way to turn the screen off and on by hand.

- **Brightness** is the AXP192's LDO2 voltage: full is about 3.1 V and
  dim 2.5 V, the lowest step short of off. The factory firmware runs at
  2.8 V. Neither is a setting.
- **The timeout** runs from the last wake, and from the end of the
  start-up screen.
- **The wake rule:** while the screen is off, a press of B only wakes
  it, and doesn't change the page. Dimmed, B wakes it and goes to the
  next page, since the screen can be read. A press of A wakes it too, and
  A's double press and hold do what they do whatever the screen shows.
- **The power button:** a short press turns the screen off at once, in
  any mode, and the next short press, or A or B, wakes it.
- **`breathe`** is `dark`, with the red LED breathing while the screen
  is off, whatever turned it off, so the StickC can be told from one
  that's off: the ATOM's breath, 8 s, starting bright, down to 15%, at a
  peak of 8% of the LED's full (the Core2 breathes its green LED at
  full). Otherwise the red LED is off.
- **What wakes it on its own,** as on the Core2: a new error, the radio
  connecting or disconnecting, an app connecting or disconnecting, and
  `device.identify`.
- **Installing over USB** lights it, even if the power button had
  turned it off (see [Installing over USB](#installing-over-usb)).

## Start-up

As on the Core2: the screen is set up first, before the settings load,
and the start-up screen is drawn before the backlight comes on:

```
         kiss-tnc-bridge
       firmware 1.4.0-dev
               (◠)
           Starting...
```

A spinner turns while it starts, and a step taking longer than 2 s says
which, "Starting Bluetooth...". The Bridge page replaces it once
everything has started, already filled in. B does nothing until then.

## Installing over USB

When `ktbctl device flash` or the web installer sends
[`device.install_notice`](control-api.md#deviceinstall_notice), the
screen changes at once, over any page, to the Core2's installing screen,
fitted to 135 px:

```
         kiss-tnc-bridge
  Installing firmware v1.4.0...

The device will restart when complete.
```

The backlight comes on, even if the power button had turned the screen
off. In download mode no firmware runs, and the ROM leaves the screen
and the AXP192 alone, so it stays through the whole install, until the
new firmware's start-up screen replaces it. While it's up, B and the
power button do nothing, and the pages stop updating.

## Reset

The ATOM Lite's reset (see [atom-lite-ui.md](atom-lite-ui.md#reset)):
hold A while turning the StickC on with the power button, and keep
holding it through the 3 s warning. Every setting goes back to its
default, the radio is forgotten, and every authorized management app is
removed; the firmware, the device's key and the authorization code stay.

A is read before any settings are loaded or Bluetooth starts. It's a
GPIO, so unlike the Core2's touch circles (issue #20) nothing has to be
calibrated first.

## Not on the screen yet

Some of what A does shows on the ATOM's LED, which the StickC doesn't
have, and nothing on the screen stands in for it yet:

| What | On the screen now |
|---|---|
| Pairing | The Bridge page's state says "Pairing"; there's no list of radios, pairing number or result |
| Authorization mode | Nothing: the authorization code is over USB (`ktbctl auth code`) or from an authorized management app |
| Reset's warning, and reset done | Nothing: the start-up screen stays up |
| Errors | Only the screen waking: a management app or `ktbctl device info` shows them |

## Not on the device

| Action | Instead |
|---|---|
| Change the name, or any setting | A management app, or `ktbctl --device` over USB |
| Brightness | Fixed |
| Power off | Hold the power button 6 s |
| Sound | The buzzer is too quiet to use |
| Disconnect an app | Not offered |
| Restart | Power off and on |

## To decide

- What the screen shows for pairing, authorization mode, reset and
  errors.

## To judge on the device

- Battery life with the screen dim, dark and in `breathe`.
- Whether LVGL should stop drawing while the screen is off, to save
  power.
