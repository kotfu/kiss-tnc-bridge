# Core2 user interface

The M5Stack Core2 v1.3 has a 320×240 color screen with a capacitive touch
panel (an FT6336U). The panel runs past the bottom of the screen, where
three circles are printed on the glass: those are touch zones, not
buttons. The power button on its side goes to the AXP192 power chip, not
to the ESP32, and the reset button next to it restarts the ESP32 the way
unplugging would. This is the design of what you can do with them in
firmware 1.2, the Core2's first firmware (Phase G of the
[roadmap](roadmap.md#phase-g-core2-and-stickc-plus-se-firmware)).
Everything else is done from a management app over the
[control API](control-api.md), as on the ATOM Lite.

The [mockups](core2-mockups/index.html) draw every screen at the
Core2's real pixels; open the file in a browser. Where this page and the
mockups differ, this page wins.

Status: decided on 11 November 2026 from the mockups, before the firmware
ran on a Core2. Anything marked provisional, and the layout's details,
are to be judged on the device.

## What's in 1.2

Firmware 1.2 makes the Core2 a bridge like the ATOM Lite, with a screen
to watch it and set it up:

- **Bridge page:** the radio, the KISS TNC apps connected, and the
  device's name. Where the device starts.
- **TNC page:** the radio the device has, and pairing a Kenwood or
  selecting a TD-H9.
- **System page:** battery, power, uptime, firmware, the name and
  Bluetooth address, errors, the screen settings, and power off.
- **The screen's modes** and its timeout, the Core2's version of the
  ATOM's `led_normal`.
- **Reset** with a gesture at power-on.

Not in 1.2: sound, vibration, authorization mode on the device, brightness
as a setting, the frame log on the microSD card, Wi-Fi, and the StickC
Plus SE. The ATOM Lite doesn't change.

## Layout

| Part | Where | What |
|---|---|---|
| Tab bar | The top 32 px | Bridge, TNC and System, 80 px each, and the [battery](#the-battery) at the right |
| Page | Between the tab bar and the soft keys | The page, or a step of a [flow](#flows) |
| Soft keys | The bottom 28 px, when a screen has any | A label above each circle (see [Soft keys](#soft-keys)) |
| Touch strip | Below the screen | The three circles |

A screen with no soft keys gives their 28 px to the page.

Text is Montserrat, LVGL's built-in font, at 11 to 20 px, and 36 px for
the pairing number. The background is black. Colors mean the same
everywhere:

| Color | Means |
|---|---|
| Green | Connected; RX activity; external power; yes |
| Amber | Waiting or retrying; TX activity |
| Red | An error; a low battery; power off |
| Blue | The selected tab; a screen's main action |
| Grey | Quiet: labels, details, the battery on battery power |

The screen is about 8 px to the millimetre, so anything to touch is at
least 44 px tall: list rows are 44 px and buttons 48 px.

## Navigation

### Tabs

The tabs along the top move between the three pages: tap one. The
selected tab is lit, with a blue line under it. A fourth page fits later
at 66 px a tab, with the battery dropping its percentage; past four, a
"More" tab would take the rest.

### Soft keys

The three circles are soft keys. When a screen has something for them,
its bottom edge shows a label right above each circle that does
something, and a circle with no label does nothing. Tapping the label on
the screen does the same as tapping the circle under it. Each position
always means the same kind of thing:

| Left | Middle | Right |
|---|---|---|
| Back, Cancel, No | The screen's main action | Yes, OK, Confirm |

A tap counts when you let go inside the zone it started in, so picking
the device up, or sliding a finger off a key, does nothing. The zones are
three bands across the strip, with gaps between them; their edges are
measured on a Core2 (provisional). Long presses are kept for later: in
1.2 a long press is a tap.

### Flows

Pairing, the screen settings and power off are **flows**: a few screens
that follow from one action. While one runs, the tabs are dimmed and
don't respond, so a half-answered question can't get lost behind
another tab, and the left key is always the way out. A flow keeps the
screen lit until it ends.

## The battery

The right end of the tab bar shows the battery's percentage, then the
battery with its level:

| Power | The level |
|---|---|
| On battery | Grey, the outline's own color |
| On battery, 20% or less | Red |
| External power, charging | Green, with stripes moving through it about twice a second |
| External power, full | Solid green |

The AXP192 reports external power and charging apart, so a full battery
on USB shows solid green, not stripes. External power is whatever is
plugged into the USB port, which may be another battery. The percentage
is the firmware's estimate (the AXP192 has no fuel gauge; see the
roadmap's [Core2 firmware](roadmap.md#core2-v13-firmware-decisions-6-7-8)).

## Bridge page

Where the device starts, and what it shows unless you've gone elsewhere.

**Radio:** the radio's name (`TH-D75`, `TD-H9`) in large type, its make
and Bluetooth address, its state, and how long it's been in that state.

| Radio | State shown | Time shown |
|---|---|---|
| Connected | **Connected**, green | How long it's been connected |
| Connecting | **Connecting**, amber | — |
| A Kenwood, link down | **Retrying in 8 s**, amber, counting down | How long it's been down |
| A TD-H9, link down | **Waiting for it**, amber (the device connects as soon as it advertises) | How long it's been down |
| Pairing or selecting (started from a management app) | **Pairing**, blue | — |
| No radio | **No radio** in place of the name, and **Not paired**, grey, with "Pair a Kenwood or select a TD-H9 on the TNC tab" | — |
| Bluetooth didn't start | **Stopped**, grey, with the error's [banner](#errors) | — |

While the radio is connected, **RX** and **TX** show the frames counted
since it connected: RX from the radio, TX to it. Each has an activity
light, RX green and TX amber, lit for about 0.3 s for each frame, as in
`ktbctl tui`. While the link is down the counts are replaced by "Counts
start again when it reconnects." The device has no clock set in 1.2, so
times are always how long ago, never a time of day.

**Apps:** the heading always names the device, **Apps on KTB-EF26**,
with how many KISS TNC apps are connected out of how many can be: "2 of
2". The name is the one KISS TNC apps see when they look for a bridge,
so it's there whether apps are connected or not. That's how you tell two
bridges apart, as identify does on the ATOM Lite. Below the heading, one
row for each app:

- its Bluetooth address;
- how long it's been connected;
- RX and TX since it connected, from the app's side: RX from the app,
  TX to it;
- an activity light for each.

Phones connect from random Bluetooth addresses that change, an iPhone's
about every 15 minutes, so an address tells two connections apart but
doesn't say which phone is which. With no apps the heading is followed
by "None connected."

Durations read `45s`, `21m`, `2h 14m` and `3d 2h`.

## TNC page and pairing

The TNC page shows the radio the device has: its name, its make and
model, whether it's paired (a Kenwood) or selected (a TD-H9), its
address, and its state. Under it, what to do on the radio before
changing it:

- Kenwood: Menu › Bluetooth › Pairing Mode
- TD-H9: Bluetooth on, BT Mode set to Receiver (see
  [td-h9-ble-kiss.md](td-h9-ble-kiss.md#setting-up-the-radio))

The middle key, **Pair or select**, starts the flow. The device keeps one
radio, so the page has no list of TNCs or adapters as in `ktbctl tui`.

1. **Looking for radios.** Up to 60 s, with the time left. The device
   scans Bluetooth Classic and LE together, as the ATOM's double press
   does, and lists each radio as it's found: a Kenwood whose name starts
   with `TH-D74` or `TH-D75`, or a TD-H9 whose name starts with `TD-H9`,
   with its make and signal strength. Anything else is counted ("3 other
   devices aren't radios") and not listed. Bridging stops while the
   device looks, as on the ATOM. Tap a radio to pair or select it.
   **Cancel** (left) goes back to the radio the device had.
2. **A Kenwood: comparing the number.** "Pairing with TH-D75", the
   six-digit number, and "Does it show this number?" **No** (left) ends
   pairing; **Yes** (right) confirms the device's side. Unlike the ATOM,
   which accepts the number without asking, the Core2 waits for your
   answer. Nothing is on the middle key, so a thumb in the middle can't
   answer.
3. **A Kenwood: Press OK on the radio.** With the time the radio gives,
   about 30 s, counting down. **Cancel** (left).
4. **A TD-H9: Selected.** "Selected TD-H9, connecting", and that the
   device remembers its address and connects whenever it's on. It isn't
   paired. **OK** (right) goes back to the TNC page.
5. **Done.** When the Kenwood confirms, the TNC page comes back with the
   new radio, connecting.

If pairing fails, times out, or is refused: **Didn't pair**, and that the
device is back on the radio it had. **Back** (left) goes to the TNC page;
**Try again** (middle) starts looking again.

A new radio replaces the old one, as on the ATOM, and a Kenwood paired
before a TD-H9 is forgotten when the TD-H9 is selected.

## System page

| Row | Shows |
|---|---|
| Battery | The percentage and the voltage: "87% · 4.05 V" |
| Power | "Charging, 280 mA", "External power, full", or "On battery, 95 mA" |
| Uptime | How long since the device started |
| Firmware | The version |
| Name | The name KISS TNC apps see (`public_name`) |
| Bluetooth | The device's Bluetooth address |
| Screen | The [screen mode](#the-screen) and timeout, "Dim after 30 s", with ›: tap it for the screen settings |

Errors, when there are any, come first, each with its whole message
(see [Errors](#errors)), and the rows scroll below them.

The middle key is **Power off** (see [Power off](#power-off)).

## The screen

Like the ATOM's LED, the screen has a mode for while nothing is
happening, and a timeout:

| `screen_normal` | After the timeout |
|---|---|
| `bright` | Nothing: the screen stays at full brightness |
| `dim` (the default) | The backlight drops low (provisional: 25%). A touch brings it back, and also does what it touched, since you can see it |
| `dark` | The screen goes off. The first touch only wakes it, and the next one acts |

`screen_timeout_seconds` is how long the screen stays at full brightness
after the last touch or wake: 30 by default, the same on battery and on
external power. The screen settings offer 15 s, 30 s, 1 min and 5 min;
the control API takes any number of seconds (provisional range). Both
are device settings, beside `led_normal` and `led_wake_seconds` in
[`device.settings`](control-api.md#devicesettingsget-and-devicesettingsset);
a Core2 has these and an ATOM Lite has the LED's. Brightness itself is
fixed in 1.2.

**The wake rule:** while the screen is dark, a touch anywhere, the
circles included, only wakes it: the touch does nothing else, even when
the finger stays down. That keeps a tap on a screen you can't see from
pressing anything. Dim needs no such rule, since the screen is still
readable.

**The power button:** a short press turns the screen off at once, in any
mode, and the next short press, or a touch, wakes it. In `bright` it then
stays on. A long hold, about 6 s (provisional), always cuts the power in
the AXP192, whatever the firmware is doing.

**What wakes it on its own,** and restarts the timeout:

- an error;
- the radio connecting or disconnecting;
- a KISS TNC app connecting or disconnecting;
- `device.identify` from a management app.

Frames don't wake it. A flow keeps it lit until the flow ends.

**Screen settings,** from the Screen row on System: the three modes, each
with a line saying what it does, and the four timeouts, with the timeout
greyed out for `bright`. A tap takes effect at once. **Back** (left)
returns to System.

## Boot

The screen stays dark until the firmware has set up the AXP192 and the
screen and drawn the splash: "kiss-tnc-bridge", "firmware 1.2.0", and
LVGL's spinner (`lv_spinner`, an arc sweeping round) over "Starting…",
on black. Then the backlight comes on, about half a second after the
power button. The splash stays until the Bridge page is ready, about a
second. Nothing waits for you or for a card.

If one step of starting takes longer than about 2 s (provisional), the
line under the spinner says which: "Starting Bluetooth…". Steps are too
quick to read otherwise, so a normal start shows only "Starting…", and a
device that hangs says where.

An error at startup goes on the screen as on the Bridge page, where the
ATOM blinks red.

## Reset

Reset does what it does on the ATOM Lite (see
[atom-lite-ui.md](atom-lite-ui.md#reset)): every setting back to its
default, the radio forgotten, and every authorized management app
removed. The firmware, the device's key and the authorization code stay.

The Core2 has no button to hold at power-on, so the gesture is two
circles:

1. Hold the left and right circles.
2. Turn the device on with the power button, or press the reset button
   on the side if it's on. Keep holding.
3. The firmware sees both held before it loads any settings or starts
   Bluetooth. **Reset:** "Keep holding to clear the settings, the radio
   and the authorized apps", the soft keys over both circles reading
   **Hold**, and a bar running out over 3 s (provisional). Let go of
   either to cancel: the device starts as usual and nothing is cleared.
4. Still held at the end, the device resets: "Settings cleared.
   Restarting…", and it restarts.
5. Let go. The device starts with no radio.

Two zones held together is hard to do by accident, by a hand picking the
device up or a bag. As on the ATOM, no setting can get in reset's way.

## Power off

**Power off** on System asks first: "Power off? The bridge stops, and
apps lose the radio. Press the power button on the side to turn it on
again." **Cancel** (left) goes back; **Power off** (right, red) has the
AXP192 cut the power.

Whether the Core2 stays off with USB plugged in, or starts again at once,
is to check on the device. If it starts again, the confirmation says so.

## Errors

The errors are the ATOM's (see
[atom-lite-ui.md](atom-lite-ui.md#errors)), with the same codes and
messages as `device.info`. Each shows as a red banner where it happens,
and the banner ends in ›: tap it and the System page opens with every
error's whole message at the top.

| Error | Banner | Where |
|---|---|---|
| `bluetooth` | Bluetooth didn't start | Under Radio, on the Bridge and TNC pages |
| `phones` | Apps can't find the device | Under the Apps heading |
| `storage` | Settings can't be saved | System only |
| `device_key` | The device has no key | System only |

An error wakes the screen.

## Not on the device in 1.2

| Action | Instead |
|---|---|
| Start authorization mode, show the authorization code | Over USB (`ktbctl device code`), or from an authorized management app. Authorizing over BLE comes in Phase E |
| Change the name, or any setting but the screen's | A management app, over USB |
| Brightness | Fixed in 1.2 |
| Sound and vibration | Later: see the roadmap |
| Forget the radio | Pairing or selecting a new one replaces it; reset removes it |
| Disconnect an app | Not offered |
| Restart | The reset button on the side |
| Roll back a firmware update | Reflash from the web installer or `ktbctl` |

## To judge on the device

- The touch zones' edges, and whether a release inside the zone is
  enough to stop accidental taps.
- Full and dim brightness, and the 30 s default.
- Text sizes and contrast at arm's length; the activity lights' 0.3 s.
- Whether the panel is an ILI9342C or an ILI9342E (Core2s since August
  2026 have the E).
- Power off with USB plugged in, and the power button's hold time.
- The reset gesture: holding two circles while pressing the power button.
