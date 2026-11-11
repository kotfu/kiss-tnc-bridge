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
// if the button isn't held.
bool reset_requested_at_boot();

// Shows the reset done flashes and the dark gap, blocking. For the reset
// at power-on, before the UI task runs.
void show_reset_done_blocking();

// Starts the UI task, which shows the power-on colors first.
void start();

// Queues an event. Safe from any task, not from an interrupt.
void post(Event e);

// Blocks until the events queued so far have been shown, up to timeout_ms.
// For device.reset, which shows its flashes before rebooting.
void wait_idle(uint32_t timeout_ms);

// A firmware update over BLE is running (Phase F).
void set_updating(bool on);

} // namespace ktb::ui
