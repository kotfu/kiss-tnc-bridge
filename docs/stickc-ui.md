# StickC Plus SE user interface

The M5Stack StickC Plus SE has a 135×240 color screen, two buttons, a
power button and a red LED (see the roadmap's
[hardware notes](roadmap.md#m5stickc-plus-se), tried on a device). This
is the design of what you can do with them in the StickC's first
firmware, 1.3. It's the Core2's UI ([core2-ui.md](core2-ui.md)) made to
fit a narrow screen and two buttons, built the same way, with LVGL.
Everything else is done from a management app over the
[control API](control-api.md), as on the ATOM Lite and the Core2.

Where this page says "as on the Core2", the Core2's page has the detail,
and the StickC does the same.

Status: draft, 12 November 2026, on hold. Firmware 1.3 shipped the
StickC with the ATOM Lite's UI on its screen instead; the full design is
[#28](https://github.com/kotfu/kiss-tnc-bridge/issues/28), which starts
from this draft. Anything marked provisional is to be judged on the
device. #28 is now building it in landscape, page by page on the device,
so most of this draft no longer matches what's built. [The
screen](#the-screen) is up to date: it's what the firmware does.

## What's in 1.3

The same as the Core2's 1.2:

- **Bridge page:** the radio, the KISS TNC apps connected, and the
  device's name. Where the device starts.
- **TNC page:** the radio the device has; pairing a Kenwood, selecting a
  BLE TNC, and forgetting the radio.
- **System page:** battery, power, uptime, firmware, the name and
  Bluetooth address, errors, the screen settings, and power off.
- **The screen's modes** and its timeout, the same settings as the
  Core2's.
- **Reset** by holding A at power-on.
- **The red LED** for what matters while the screen is dark
  (provisional).

Not in 1.3, as on the Core2: authorization mode on the device, the
authorization code on the screen, brightness as a setting, and anything
the microSD card or Wi-Fi would bring (the StickC has neither). **No
sound:** the buzzer is too quiet to use (see the hardware notes).

## The device

Held in portrait, with the USB port at the bottom: the screen fills the
face, with A below it.

| Control | Where | What it does |
|---|---|---|
| **A** | The large button on the face, below the screen | Acts: the action named at the bottom of the screen, right above it |
| **B** | The right side, at the top end | Moves: the next page, or the next row of a list |
| **Power** | The left side, near the USB end | A short press turns the screen off or wakes it; held, it turns the device on, and off |
| **Red LED** | The top end, beside B | Errors, and frames going to the radio (see [The LED](#the-led)) |

A and B are GPIOs the firmware reads. The power button is the AXP192's,
as on the Core2: the chip turns the device on, reports short and long
presses, and cuts the power when it's held, whatever the firmware is
doing.

| Power button | While | What happens | Whose |
|---|---|---|---|
| Hold about half a second | Off | Turns the StickC on | The AXP192's |
| Short press | On | Turns the screen off, or wakes it | The firmware's |
| Hold 6 s | On | Cuts the power | The AXP192's. The factory firmware sets 4 s; 6 s, as on the Core2, is harder to do by accident in a pocket |

A long press (about 1 s) is reported too; the firmware doesn't use it in
1.3.

## Layout

| Part | Where | What |
|---|---|---|
| Top bar | The top 20 px | The [battery](#the-battery) at the left; at the right, beside B, the page dots: one for each page, the current one lit, so B is the way to the others |
| Page | Between the bars | The page, or a step of a [flow](#flows) |
| A bar | The bottom 22 px, when there's something for A | A's label, centered, right above A: "Pair", "Yes" |

A page with nothing for A gives the bottom 22 px to the page.

Text is Montserrat, LVGL's built-in font, at 12 to 20 px, and 28 px for
the pairing number (provisional: the fonts cost flash). About 19
characters fit across at 12 px, enough for a Bluetooth address. The
background is black. Colors mean what they mean on the Core2: green
connected, amber waiting, red an error, blue a selection or the main
action, grey quiet.

## Navigation

**On a page,** B goes to the next page (Bridge, TNC, System, and round
again), and A does what the A bar says, if anything.

**In a list,** a row is highlighted: B moves to the next row (round to
the first after the last), and A chooses it. Every list ends with a
**Back** row, which goes back where the list came from. There are no
long presses to learn: the way out is always a row you can see.

**In a question** (yes or no), A is the yes: the A bar says what it
does ("Yes", "Forget", "Power off"). B is the no, and the top bar's page
dots give way to its label, "No" or "Cancel", at the right beside B.

**Flows** (pairing, the screen settings, power off) are as on the
Core2: while one runs, B no longer changes pages, and every step has a
way back, a Back row or B's Cancel. A flow keeps the screen lit until it
ends.

A press counts when it's let go (provisional), so a button held while
the device is picked up does nothing until it's released, and holding A
at power-on is [reset](#reset), not a press.

## The battery

At the left of the top bar: the battery with its level, then its
percentage, as on the Core2:

| Power | The level |
|---|---|
| On battery | Grey |
| On battery, 20% or less | Red |
| External power, charging | Green, with stripes moving through it |
| External power, full | Solid green |

External power is USB, which comes in on the AXP192's VBUS input (on
the Core2 it's ACIN). The battery is 120 mAh, so it lasts an hour or two
(provisional: to measure), and the percentage is the same estimate from
the voltage as the Core2's.

## Bridge page

The Core2's Bridge page, stacked:

1. **The radio:** its name (`TH-D75`, `TD-H9`) at 20 px; its state, in
   color, with how long ("Connected for 2h 14m", "Retrying in 8s",
   "Waiting", "Pairing", "No radio"), the states and wording of
   the Core2's; its Bluetooth address.
2. **RX and TX** since it connected, each with its activity light, as
   on the Core2.
3. **Apps on KTB-F5B4**, "2 of 2": the name KISS TNC apps see, and how
   many are connected. Then a row for each app: its address and how long
   it's been connected. The counts for each app are left out: they don't
   fit, and the Core2 and `ktbctl status` have them.

Nothing for A.

## TNC page and pairing

The radio the device has: its name, its make, whether it's paired (a
Kenwood) or selected (a BLE TNC), its address, and its state. Under it,
what to do on the radio before changing it, as on the Core2.

A is **Change**, which opens a list:

- **Pair or select a radio:** the pairing flow.
- **Forget TH-D75,** while there's a radio: asks first, with the
  Core2's words. A **Forget**, B **Cancel**.
- **Back.**

The pairing flow is the Core2's, with lists instead of taps:

1. **Looking for radios.** Up to 60 s, with the time left. Radios are
   listed as they're found, as on the Core2, each a row; B moves, A
   chooses, and the last row is **Cancel**, which goes back to the radio
   the device had. Until a radio turns up, the list says how to put one in
   pairing mode.
2. **A Kenwood: the number.** "Pairing with TH-D75", then the six-digit
   number at 28 px and "Does it show this number?" **A: Yes**, **B: No**.
   The device waits for your answer, as the Core2 does.
3. **A Kenwood: Press OK on the radio,** counting down. **B: Cancel.**
4. **A BLE TNC: Selected,** connecting. **A: OK.**
5. **Done:** back to the TNC page, with the new radio connecting.

If it fails: **Didn't pair** and why, as on the Core2. **A: Try again**,
**B: Back**.

## System page

The Core2's rows, one to a line at 12 px: Battery ("87% · 4.05 V"),
Power ("Charging, 86 mA"), Uptime, Firmware, Name, Bluetooth, and Screen
("Dim after 30 s"). Errors come first, each with its whole message,
wrapped. B goes to the next page, so the page doesn't scroll: if the
errors push the rows off the bottom, the rows that don't fit are left
off until the errors clear (provisional).

A is **Settings**, which opens a list:

- **Screen:** the screen settings.
- **Power off:** asks first, with the Core2's words. **A: Power off**,
  **B: Cancel.**
- **Back.**

## The screen

The Core2's modes and timeout, with the same settings, `screen_normal`
(`bright`, `dim`, `dark`) and `screen_timeout_seconds`, 30 s by default
(see [The screen](core2-ui.md#the-screen) on the Core2). They're
[device settings](control-api.md#devicesettingsget-and-devicesettingsset),
in place of the ATOM's `led_*` settings the StickC had through firmware
1.4.0.

**The settings are changed from a management app** (or `ktbctl --device
PORT device settings set screen_normal=dark`), not on the device. The
System page's last row, **Screen**, shows them as the Core2 does: "Dim
after 30 s", "Dark after 1 min", "Bright". The power button is the way to
turn the screen off and on by hand.

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
- **What wakes it on its own,** as on the Core2: a new error, the radio
  connecting or disconnecting, an app connecting or disconnecting, and
  `device.identify`.
- **Installing over USB** lights it, even if the power button had
  turned it off (see [`device.install_notice`](control-api.md#deviceinstall_notice)).

## The LED

The red LED is on the top end, so it can be seen when the StickC is in a
pocket or a pouch with the screen dark. Provisional, for review:

| What | The LED |
|---|---|
| An error | Three fast blinks, then a pause, over and over, as the ATOM's red |
| A frame to the radio (it will transmit) | A short flick, as the ATOM's white flick |
| Identify | Three slow blinks |
| Anything else | Off |

It shows these whatever the screen is doing. Frames from the radio don't
flick it: they're most of the traffic, and the screen shows them.

## Boot

As on the Core2: the screen stays dark until the AXP192 and the screen
are set up and the splash is drawn ("kiss-tnc-bridge", the firmware's
version, a spinner over "Starting…", stacked to fit), then the backlight
comes on. A step taking longer than about 2 s says which. An error at
start shows on the Bridge page.

## Reset

Reset does what it does on the ATOM Lite and the Core2: every setting
back to its default, the radio forgotten, and every authorized
management app removed; the firmware, the device's key and the
authorization code stay.

1. Hold A.
2. Turn the StickC on with the power button. Keep holding A.
3. **Reset:** "Keep holding A to clear the settings, the radio and the
   authorized apps", and a bar running out over 3 s. Let go to cancel:
   the device starts as usual.
4. Still held at the end: "Settings cleared. Restarting…", and it
   restarts.

A is read before any settings are loaded or Bluetooth starts. It's a
GPIO, so unlike the Core2's touch circles (issue #20) nothing has to be
calibrated first.

## Power off

As on the Core2: **Power off** on System asks first, then the screen goes
black and the AXP192 cuts the power; the power button turns it on again.
Whether it stays off with USB plugged in is to check on the device: the
StickC's USB comes in on VBUS, the Core2's on ACIN.

## Errors

The Core2's: the same codes and messages, a red line where each happens,
whole on System, and an error wakes the screen and blinks the LED.

## Not on the device in 1.3

| Action | Instead |
|---|---|
| Authorization mode, the authorization code | Over USB (`ktbctl auth code`), or from an authorized management app; authorizing over BLE comes in Phase E |
| Change the name, or any setting but the screen's | A management app, or `ktbctl --device` over USB |
| Brightness | Fixed in 1.3 |
| Sound | The buzzer is too quiet to use |
| Disconnect an app | Not offered |
| Restart | Power off and on |

## To decide

- The two-button scheme: B moves, A acts, a Back row at the end of every
  list, and no long presses.
- Portrait with USB at the bottom, rather than landscape.
- The LED: errors and frames to the radio, or nothing.
- The pairing number at 28 px, which costs a font's flash, or 20 px.
- Forget on the device (the Core2 has it), under TNC's Change list.

## To judge on the device

- Full and dim brightness, and the screen's readability at 12 px.
- Whether a press counting on release feels slow.
- Power off with USB plugged in.
- Battery life with the screen dim, and dark.
- The LED's flick for frames to the radio: too busy, or useful.
