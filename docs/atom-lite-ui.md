# ATOM Lite user interface

The ATOM Lite has one button (GPIO39) and one RGB LED (an SK6812 on
GPIO27). This is the design of what you can do with them, worked out at the
start of Phase C (see the [roadmap](roadmap.md#phase-c-atom-lite-firmware-and-web-installer)).
Everything else is done from a trusted client (the iOS app, `ktbctl`) over
the [control API](control-api.md), or by reflashing.

Status: the actions, reset, the management window rule and the LED's
dark mode were decided on 9 November 2026. The gestures and the LED's colors and
patterns are next.

## What you can do on the device

Only what you'd need with no app, no trusted client and no computer:

| Action | What it does |
|---|---|
| **Pair a radio** | Scans for a Kenwood in pairing mode and pairs with it. A new pairing replaces the old one, since the device keeps one radio |
| **Open the management window** | Lets one new client enroll with the setup code (see [below](#the-management-window)) |
| **Reset** | Clears all settings (see [below](#reset)). It doesn't touch the firmware |

The control API calls the management window's gesture a long press,
provisionally. The gestures for all three are still to be designed.

## Reset

Reset puts the device back the way it was right after the firmware was
installed:

- every setting goes back to its default
- the radio pairing is removed
- every trusted management client is removed

The firmware stays, so reset is not a "factory reset". It's also the way out
of a configuration that makes the device misbehave, so it has to work
however the device is set up: a setting can't stop the button from
reaching it.

Reset keeps the device's key and setup code, so a reset device can be
enrolled again with the code you wrote down at install time. To change the
code, a trusted client rotates it (`auth.rotate_code`). To change the key
as well, erase the flash and reinstall the firmware.

## The management window

The window is the time during which the device accepts an enrollment
handshake: 5 minutes (provisional) after it's opened. Trusted clients
don't need it; they connect whenever they like.

- **One enrollment per window.** The window closes after the first
  successful enrollment, including a client that was already trusted and
  enrolled again. Enrolling two clients takes two windows. This keeps a
  window that's left open from letting in more than the client it was
  opened for.
- **A few failed attempts close it.** A wrong code doesn't close the window,
  so someone nearby can't keep closing it before you enroll, but after 3
  (provisional) it closes anyway.
- **The same rule everywhere:** the button, the Core2's menu, a Pi running
  `ktb-gateway`, and `auth.window` from a trusted client.

## The LED

Two settings decide what the LED shows when everything is working (the
radio linked, nothing wrong):

| Setting | Values |
|---|---|
| `led_normal` | `on` (the default): the LED shows the normal state. `dark`: the LED is off when things are normal |
| `led_wake_seconds` | In dark mode, how long the LED stays lit after a press before it goes dark again. Default 30 (provisional) |

Names and defaults are provisional; they join `led_brightness` in
[`device.settings`](control-api.md#devicesettingsget-and-devicesettingsset).

Anything that isn't normal shows in either mode: no radio, the window
open, pairing, updating, an error. Dark mode only darkens the normal state.

**The wake rule:** while the LED is dark, a press of any length only wakes
it, and does nothing else. To pair a radio in dark mode you press once to
light the LED, then do the pairing gesture. Every press restarts the wake
timer.

This also means:

- **You always see the LED when you choose an action.** The LED will show
  which action you'll get as you hold the button, so it has to be lit.
- **Accidental presses can't start anything** in dark mode. A bump in a
  pocket or a bag only wakes the LED.

## Not on the device

Left out on purpose, and how you do each one instead:

| Action | Instead |
|---|---|
| Revoke all trusted clients, rotate the setup code | A trusted client; with none, reset |
| Roll back a bad firmware update | Reflash from the web installer or `ktbctl` |
| Open the window at power-on | The button |
| Safe mode (boot with only USB) | Reset |
| Forget the radio | Pairing a new one replaces it; reset removes it |
| Reboot, reconnect to the radio now, clear an error | Unplug the USB cable or the Tail Bat |
| Pause the radio link | Not offered |
| Disconnect all phones, close the window early | Not needed: unplug, or let the window time out |
| Show the setup code | Over USB (`ktbctl device code`), or from a trusted client |
| Change settings | A trusted client, or USB |

## Still to design

- The gesture for each action, and how the LED shows which action you'll
  get while the button is held.
- The LED's colors and patterns for each state: radio linked, no radio,
  phones connected, window open, pairing, updating, error.
