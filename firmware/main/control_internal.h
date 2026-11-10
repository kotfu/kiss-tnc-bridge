// Shared by the control API's command files.
#pragma once

#include <functional>
#include <string>

#include "cJSON.h"
#include "control.h"

namespace ktb::control {

// What a command produces: a result, an error, or nothing yet (deferred:
// it answers later through its Pending). `after` runs once the response
// is on its way, for a reboot.
struct Reply {
    cJSON *result = nullptr;
    const char *code = nullptr;
    std::string message;
    bool deferred = false;
    std::function<void()> after;

    static Reply ok(cJSON *r)
    {
        Reply x;
        x.result = r;
        return x;
    }
    static Reply empty() { return ok(cJSON_CreateObject()); }
    static Reply error(const char *code, std::string message)
    {
        Reply x;
        x.code = code;
        x.message = std::move(message);
        return x;
    }
    static Reply later()
    {
        Reply x;
        x.deferred = true;
        return x;
    }
};

using Handler = Reply (*)(const cJSON *req, const Pending &p);

struct Command {
    const char *name;
    Handler handler;
};

// The radio, Bluetooth, TNC, port, status and subscribe commands
// (control_radio.cpp).
extern const Command radio_commands[];
extern const size_t radio_command_count;

// Adds topics to a connection's subscription, or removes them; returns
// what's subscribed afterwards.
uint32_t subscribe(Conn *conn, uint32_t topics);
uint32_t unsubscribe(Conn *conn, uint32_t topics);

} // namespace ktb::control
