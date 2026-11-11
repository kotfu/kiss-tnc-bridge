# ATOM Lite user interface

The ATOM Lite has one button (GPIO39) and one RGB LED (an SK6812 on
GPIO27). The small button on its side is the ESP32's reset, wired to the
chip's enable pin: it restarts the device, and the firmware can't see it.
This is the design of what you can do with them, worked out at the
start of Phase C (see the [roadmap](roadmap.md#phase-c-atom-lite-firmware-and-web-installer)).
Everything else is done from a management app (the iOS app, `ktbctl`) over
the [control API](control-api.md), or by reflashing.

Status: the actions and their gestures, the authorization mode rule, and
the LED's states and events were decided on 9 November 2026, and the
LED's timing was judged on an ATOM Lite running the firmware the same day.

## What you can do on the device

Only what you'd need with no app and no computer:

| Action | Gesture | What it does |
|---|---|---|
| **Wake the LED** | Single press, in dark mode | Lights the LED for `led_wake_seconds` (see [the wake rule](#normal-and-dark-mode)). Only in dark mode, and only while the LED is dark |
| **Pair a radio** | Double press | Scans for a Kenwood in pairing mode and pairs with it. A new pairing replaces the old one, since the device keeps one radio |
| **Cancel pairing** | Single press, while pairing | Stops pairing and goes back to the radio the device had |
| **Start authorization mode** | Long press | Lets one new management app be authorized with the authorization code (see [below](#authorization-mode)) |
| **End authorization mode** | Single press, in authorization mode | Ends it before its time runs out |
| **Reset** | Hold the button at power-on | Clears all settings (see [below](#reset)). It doesn't touch the firmware |
| **Restart** | The side button | Restarts the device, as unplugging it and plugging it back in would |

The firmware can't tell the side button from plugging in: both are a
power-on to it, and `device.info`'s `last_reset` says `power_on` for
either.

Reset happens only at power-on, which leaves the running device's
gestures to pairing and authorization mode. A single press never starts anything,
so it's free to wake the LED.

### Pairing

Double press: two presses, the second within 400 ms (provisional) of
letting go of the first. Pairing starts straight away; its state is the
feedback (blue, fast blink).

Pairing scans for 60 seconds. The radio's model, TH-D74 or TH-D75, comes
from its Bluetooth name, so pairing needs no other setup. The device keeps the old radio until a new
one pairs. If pairing fails, times out, or is cancelled with a single
press, it goes back to the radio
it had (or to no radio). So an accidental double press costs only the
bridging that stops while the device scans, and a single press ends that.

Nothing waits on a double press: a single press only wakes the LED or
cancels pairing, so it doesn't need to wait to see whether a second press
follows.

### Starting authorization mode

Long press: hold the button for 3 seconds (provisional). The LED turns
**steady magenta**, meaning "let go now to start authorization mode". Let
go and authorization mode starts: **magenta, slow blink**. Let go before
the LED turns magenta and nothing happens. Holding past it changes
nothing; authorization mode starts when you let go.

A single press in authorization mode ends it, and the LED goes back to the
state it was showing. Ending it shows no confirmation, and neither does
its time running out: whether an authorization worked is for the
management app being authorized to say.

This is a different hold from reset's, which happens only at power-on.

### Dark mode while pairing or in authorization mode

Pairing and authorization mode always show, whatever `led_normal` says, and
the LED never goes dark while either one lasts, even if the wake timer runs
out. So the single press that cancels pairing or ends authorization mode
is never taken as a wake press. When pairing or authorization mode ends, the wake
timer starts again, so you see the state the device goes back to before
the LED goes dark.

### Gestures in other states

| While | The button |
|---|---|
| **Updating** | Does nothing. Pairing or starting authorization mode mid-update isn't worth the risk |
| **Error** | Works as usual: whatever still works can still be used |
| **Pairing** | Only a single press, which cancels pairing |
| **Authorization mode** | Only a single press, which ends authorization mode |
| **The LED is dark** | Only a wake (see [the wake rule](#normal-and-dark-mode)) |

## Reset

Reset puts the device back the way it was right after the firmware was
installed:

- every setting goes back to its default
- the radio pairing is removed
- every authorized management app is removed

The firmware stays, so reset is not a "factory reset". It's also the way out
of a configuration that makes the device misbehave, so it has to work
however the device is set up: a setting can't stop the button from
reaching it.

Reset keeps the device's key and authorization code, so management apps
can be authorized again on a reset device with the code you kept at install
time. To change the code, an authorized management app changes it
(`auth.rotate_code`). To change the key
as well, erase the flash and reinstall the firmware.

### How you reset

Hold the button while you power the device on, and let go when you see
three white flashes:

1. With the device unpowered, press and hold the button.
2. Connect power: plug in the USB cable, or put the ATOM on its Tail Bat
   (Tail Bat in one hand, the ATOM in the other with your thumb on the button.
   Position your thumb so you can see the LED, which is on the button).
3. **Power on:** red, yellow, cyan, green.
4. The firmware sees the button held. **Reset warning:** steady white for 3
   seconds (provisional). Let go now to cancel: the device starts as usual
   and nothing is cleared.
5. Still held at the end of the warning, the device resets. **Reset done:**
   three quick white flashes and the dark gap, then it reboots.
6. Let go. The device starts with no radio: yellow and blue, alternating.

The firmware checks the button before it loads any settings or starts
Bluetooth, so reset works even when a setting makes the device crash or
misbehave. No setting, dark mode included, can get in its way.

With the device already powered, the side button does the same: hold the
button, press and let go of the side button, and carry on from step 3.
That's easier than plugging in with a thumb on the button, on a Tail Bat
especially.

If you're still holding the button when it reboots, it goes through the
warning again: let go during the warning, and nothing more happens. The
firmware doesn't treat that second boot differently.

## Authorization mode

Authorization mode is the time during which the device accepts an
authorization handshake: 5 minutes (provisional) after it starts.
Authorized management apps don't need it; they connect whenever they
like.

- **One authorization each time.** Authorization mode ends after the first
  successful authorization, including an app that was already authorized
  and was authorized again. Authorizing two apps takes authorization mode
  twice. This keeps authorization mode, if it's forgotten, from letting in
  more than the app it was started for.
- **A few failed attempts end it.** A wrong code doesn't end authorization
  mode, so someone nearby can't keep ending it before your app is
  authorized, but after 3 (provisional) it ends anyway.
- **The same rule everywhere:** the button, the Core2's menu, a Pi running
  `ktb-gateway`, and `auth.mode` from an authorized management app.

## The LED

The LED shows one [state](#states) at a time, with brief
[events](#events) on top of it. The colors and patterns below are a first
draft.

### Normal and dark mode

The device is **normal** when it's bridging: the radio connected, with or
without phones. One setting decides what the LED shows then:

| `led_normal` | When things are normal |
|---|---|
| `bright` (the default) | The state's color, steady |
| `dim` | The state's color, steady, at half the brightness to the eye: the same as halving `led_brightness` for the normal states. Frame flicks are dimmed too |
| `breathe` | The state's color in a long, slow pulse, in brightness or between neighboring colors |
| `dark` | Off, until a press wakes it |

`led_wake_seconds` sets how long a press keeps the LED lit in dark mode
before it goes dark again: 30 (provisional). Names and defaults are
provisional; they join `led_brightness` in
[`device.settings`](control-api.md#devicesettingsget-and-devicesettingsset).

Dim, breathe and dark mode change only the normal states. Anything else shows
the same in every mode, which is why most rows in the tables below say
"Same".

**The wake rule:** while the LED is dark, the only gesture is a single
press, which wakes it. A double or long press on a dark LED only wakes it
too. Once the LED is lit, every gesture works. To pair a radio in dark
mode you press once to wake the LED, then double press. Every press
restarts the wake timer.

**Power-on counts as a wake.** After the power-on colors and the yellow
while it connects, a device in dark mode shows cyan or green for
`led_wake_seconds` before it goes dark, so you see that it linked.

This also means:

- **You always see the LED when you choose an action.** The LED will show
  which action you'll get as you hold the button, so it has to be lit.
- **Accidental presses can't start anything** in dark mode. A bump in a
  pocket or a bag only wakes the LED.

### States

A state lasts until something changes. When more than one is true, the
LED shows the highest row: authorization mode can be on, or an update running,
while the device goes on bridging.

| State | What it means | Bright | Dim | Breathe | Dark |
|---|---|---|---|---|---|
| **Error** | Something the device can't fix by retrying (see [Errors](#errors)). Connect a management app to see what; with none, unplug the device and plug it in, then reinstall the firmware. One generic error: blink codes can come later if a cause turns up that you could fix with no management app | Red, three fast blinks, then a pause | Same | Same | Same |
| **Updating** | A firmware update is being written over BLE, about 30 s. Don't unplug. Over USB the device's firmware isn't running, so the web page or `ktbctl` shows progress and the LED isn't ours | White, fast blink | Same | Same | Same |
| **Authorization mode** | Accepting one new management app | Magenta, slow blink | Same | Same | Same |
| **Pairing** | Scanning for a Kenwood in pairing mode | Blue, fast blink | Same | Same | Same |
| **No radio** | No radio is paired: after installing or a reset. Nothing is bridged | Yellow and blue, alternating at the slow blink pace | Same | Same | Same |
| **Radio not connected** | A radio is paired, but the link is down: the radio is off or out of range, or the device has just started. It tries again every 15 s | Yellow, slow blink | Same | Same | Same |
| **Radio connected, no phones** | Normal: bridging is ready, but no KISS TNC app is connected | Cyan, steady | Cyan, steady, dimmed | Cyan, slow pulse | Off; a press shows it steady |
| **Radio connected, phones connected** | Normal: bridging | Green, steady | Green, steady, dimmed | Green, slow pulse | Off; a press shows it steady |

"Phones" means KISS TNC apps connected over BLE. The device doesn't show
when a management app is connected, and a management app doesn't turn cyan
into green.

**Color blindness.** Red-green color blindness is common, and hams are
mostly men, among whom it's most common. Each state that needs attention
differs from the others by its pattern as well as its color, so it reads
without color: the error's three blinks and a pause against pairing's
steady fast blink and the normal states' steady light, and the yellow states' blinks, one of them with blue, which red-green
color blindness doesn't affect. Cyan and green, the two normal states,
differ only in color; telling phones from no phones is the one thing a
color-blind user may lose, and it's not worth a pattern of its own.

### Errors

An error is something the device can't fix by retrying. `device.info`
lists the errors with what to do (see
[control-api.md](control-api.md#deviceinfo)), and the control API keeps
working over USB whatever fails, so a management app can always say what's wrong.
A new firmware image that starts with an error fails its self-test and is
rolled back.

| Error | What happened | What still works | Clears |
|---|---|---|---|
| `storage` | NVS, where settings, the radio pairing and the device's key live, can't be used | The device runs on default settings and forgets everything when unplugged. Bluetooth may not start | Only by a clean install |
| `bluetooth` | The Bluetooth controller, Bluedroid or SPP didn't start | USB only: nothing is bridged | On a power cycle, if it was a fluke; otherwise a reinstall |
| `phones` | The BLE KISS service or advertising didn't start, or advertising didn't start within 10 s of power-on | The radio link, but phones can't find the device | By itself, once advertising starts; otherwise a power cycle |
| `device_key` | The device's X25519 key couldn't be loaded or made | Bridging. No management app can be authorized over BLE (Phase E) | Only by a clean install, which makes a new key |

Not errors, because retrying or the button fixes them:

- **The radio is off, out of range, or doesn't take KISS mode:** radio
  not connected. The device retries every 15 s.
- **The radio has forgotten the device** (its pairing was deleted on the
  radio): the radio refuses the device's key, Bluedroid removes the bond,
  and the LED shows no radio. A TH-D75 says the pairing was refused.
  Pairing again fixes it. The device stops retrying, so the radio isn't
  asked to pair every 15 s. (Tested on a TH-D75: the radio won't delete a
  connected device, so `port.disconnect` held the link down first.)
- **A crash, a watchdog reset or a brownout** restarts the device, which
  carries on. `device.info`'s `last_reset` says why it last started; a
  brownout points at a weak battery or cable.
- **A firmware update rolled back:** the device runs the version it had,
  and `device.info` says which version was rolled back.

### Events

An event is a brief flash on top of the state. Their order doesn't matter.

Most events end with a **dark gap**, the LED off for a moment, so the
event is visibly separate from the state that follows it. Identify and
the confirmations (the three quick flashes) start with the gap too, to
set them apart from the state before them: cancelling pairing, say,
goes from a blue blink to yellow flashes. Frames have no gap: they're a
flick on top of the state, which carries on around them.

| Event | Regular (`bright`, `dim`, `breathe`) | Dark |
|---|---|---|
| **Power on** (the firmware has started) | Red, yellow, cyan, green, one straight after another, each as long as a slow blink, then the dark gap | Same |
| **Frame from the radio to a phone** | A short dip in brightness | Nothing, unless the LED is awake |
| **Frame from a phone to the radio** (the radio will transmit) | A short white flick | Nothing, unless the LED is awake |
| **Pairing succeeded** | The dark gap, green, three quick flashes, then the dark gap | Same |
| **Pairing failed** (no radio found, it refused, or you cancelled) | The dark gap, yellow, three quick flashes, then the dark gap | Same |
| **App authorized** (authorization mode ends) | The dark gap, green, three quick flashes, then the dark gap | Same |
| **Identify** (a management app asked, with `device.identify`) | The dark gap, then magenta, blue, white, three times, one straight after another, each as long as a slow blink, then the dark gap | Same |
| **Reset warning** (the button held at power-on) | Steady white for 3 seconds (provisional); let go to cancel | Same |
| **Reset done** (just before the reboot) | The dark gap, white, three quick flashes, then the dark gap | Same |
| **Long press reached** (the button held for 3 s) | Steady magenta until you let go | Same: the LED is awake |

Red is kept for errors: a failed pairing is yellow, like the radio states
it returns to.

### An example

A new ATOM Lite, after installing the firmware from the web page over USB:

1. The installer resets the ATOM. **Power on:** red, yellow, cyan, green,
   then dark.
2. **No radio:** yellow and blue, alternating.
3. You start pairing. **Pairing:** blue, fast blink.
4. The radio pairs. **Pairing succeeded:** three quick green flashes, then
   dark.
5. **Radio connected, no phones:** cyan, steady.
6. A KISS TNC app connects. **Radio connected, phones connected:** green,
   steady, with a white flick for each frame the app sends and a dip for
   each frame it receives.

### Timing

Judged by eye on an ATOM Lite (9 November 2026). The values are in
[`firmware/main/ui.cpp`](../firmware/main/ui.cpp), and the colors in
[`firmware/main/led.h`](../firmware/main/led.h).

| What | Time |
|---|---|
| Fast blink | 250 ms on, 250 ms off |
| Error blink | Three fast blinks, then 500 ms more dark: red on 250 ms, off 250 ms, three times, then 500 ms off. 2 s in all |
| Slow blink | 800 ms on, 800 ms off |
| Quick flashes | 250 ms on, 250 ms off, three times |
| Dark gap | 1 s |
| Power-on and identify | 800 ms for each color |
| Breathe | 8 s for one breath, down to 15% |
| Dim | The normal states at 25% of their light, which looks like half |
| Frame dip | 100 ms at 15% |
| Frame flick | 100 ms of a greenish white, (170, 255, 170): pure white flicked on green looked magenta |
| Long press | 3 s |
| Double press | The second press within 400 ms of letting go of the first |
| Reset warning | 3 s |

The gestures' durations haven't been changed from the first draft; they
may still change with use. A whole confirmation takes about 3.5 s,
power-on about 4.2 s, and identify about 9.2 s.

Dark mode works on a Tail Bat. At 240 MHz with the LED dark, a charged
Tail Bat kept the ATOM running above its 45 mA cut-off with one phone
connected, with none, and while waiting for the radio (see the roadmap's
[Hardware notes](roadmap.md#hardware-notes)).

## Not on the device

Left out on purpose, and how you do each one instead:

| Action | Instead |
|---|---|
| Revoke all management apps, change the authorization code | An authorized management app; with none, reset |
| Roll back a bad firmware update | Reflash from the web installer or `ktbctl` |
| Start authorization mode at power-on | The button |
| Safe mode (boot with only USB) | Reset |
| Forget the radio | Pairing a new one replaces it; reset removes it |
| Reboot, reconnect to the radio now, clear an error | The side button, or unplug the USB cable or the Tail Bat |
| Pause the radio link | Not offered |
| Disconnect all phones | Not needed: unplug |
| Show the authorization code | Over USB (`ktbctl device code`), or from an authorized management app |
| Change settings | An authorized management app, or USB |
