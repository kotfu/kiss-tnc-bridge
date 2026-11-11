// The USB serial port: the firmware's log and the control API share it.
// Protocol messages are whole lines that start with '{'; log lines never
// do. Every line, log or protocol, goes out in one piece, so a management app
// can pick out the protocol lines (docs/control-api.md, USB serial).
#pragma once

#include <cstddef>

namespace ktb::console {

// The largest request line accepted, in bytes (device.info's max_message).
constexpr size_t max_message = 4096;

// Takes over the console UART and routes the log through it. Call first.
void start();

// Writes one line (a newline is added), without interleaving with the log.
void write_line(const char *text, size_t len);

// Called for each line received, without its line ending. The buffer is
// writable and valid only during the call. A line longer than
// max_message is reported with len = 0 and line = nullptr.
using LineHandler = void (*)(char *line, size_t len);

// Starts the task that reads lines from the port.
void serve(LineHandler handler);

// Waits until everything written has left the UART, before a restart.
void flush();

} // namespace ktb::console
