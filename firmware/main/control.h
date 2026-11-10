// The control API over USB serial (docs/control-api.md): one JSON request
// per line, one response per line. The firmware's subset so far:
// device.info, device.settings.get and .set, device.reboot, device.reset,
// device.identify, and auth.info, auth.code, auth.rotate_code, auth.window
// and auth.clients. Development builds add debug.*.
#pragma once

#include <cstddef>

namespace ktb::control {

// A line from the USB serial port (console::LineHandler).
void handle_line(char *line, size_t len);

} // namespace ktb::control
