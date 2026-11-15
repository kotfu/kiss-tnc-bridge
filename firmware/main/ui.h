// The button and the LED (docs/atom-lite-ui.md): gestures, states,
// events, dark mode and the wake rule.
#pragma once

#include <cstdint>

namespace ktb::ui {

enum class Event : uint8_t {
    power_on,
    pairing_succeeded,
    pairing_failed,
    app_authorized,
    identify,
    reset_done,
    frame_to_phone, // a frame from the radio to a phone: a dip
    frame_to_radio, // a frame from a phone to the radio: a white flick
};

// At power-on, before settings load: if the button is held, shows the
// power-on colors and the reset warning. Returns true if it was still held
// at the end of the warning, so the caller resets. Returns false at once
// if the button isn't held. The Core2 only shows its splash and returns
// false: its Reset is on the System page.
bool reset_requested_at_boot();

// Shows the reset done flashes and the dark gap, blocking. For the reset
// at power-on, before the UI task runs.
void show_reset_done_blocking();

// Starts the UI task, which shows the power-on colors first.
void start();

// Start-up, for a screen's splash (docs/core2-ui.md, Boot): the step
// that's running, and that the device is ready. The LED ignores both.
void starting(const char *step);
void started();

// Queues an event. Safe from any task, not from an interrupt.
void post(Event e);

// Blocks until the events queued so far have been shown, up to timeout_ms.
// For device.reset, which shows its flashes before rebooting.
void wait_idle(uint32_t timeout_ms);

// A firmware update over BLE is running (Phase F).
void set_updating(bool on);

// A USB install is about to start (device.install_notice): shows it until
// the device restarts, since the install resets the ESP32 into its ROM and
// the LED or screen keeps what it last showed. version is the firmware
// being installed, for a screen to show, or empty. Returns once it's
// shown, or after timeout_ms; false if it wasn't shown by then.
bool show_installing(const char *version, uint32_t timeout_ms);

} // namespace ktb::ui
