// The control API (docs/control-api.md): one JSON request per line, one
// response per line, and events. USB serial is the only connection until
// BLE management arrives in Phase E.
//
// Most commands answer at once. Some (bluetooth.scan, bluetooth.pair)
// answer later, from another task, through a Pending; until then they send
// events to the connection that asked.
#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

#include "cJSON.h"

namespace ktb::control {

// A connection a client speaks the API over.
class Conn {
public:
    virtual ~Conn() = default;
    // Sends one message, a whole line.
    virtual void send_line(const char *text, size_t len) = 0;
};

// Event topics for subscribe.
enum Topic : uint32_t {
    topic_state = 1 << 0,
    topic_frames = 1 << 1,
    topic_activity = 1 << 2,
};

// A request to be answered later.
struct Pending {
    Conn *conn = nullptr;
    double id = 0;
    bool valid() const { return conn != nullptr; }
};

// Answers a pending request. Take ownership of result. Safe from any task.
void reply_ok(const Pending &p, cJSON *result);
void reply_error(const Pending &p, const char *code, const std::string &message);

// Sends an event to one connection (adds "v"). Takes ownership.
void send_event(Conn *conn, cJSON *event);

// True if any connection subscribed to the topic: check before building an
// event nobody will get.
bool subscribed(Topic topic);
// Sends an event to every connection subscribed to the topic. Takes
// ownership.
void publish(Topic topic, cJSON *event);

// A line from the USB serial port (console::LineHandler).
void handle_line(char *line, size_t len);

} // namespace ktb::control
