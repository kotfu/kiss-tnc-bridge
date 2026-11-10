// The device's errors (docs/atom-lite-ui.md, Errors): things it can't fix by
// retrying. Any of them turns the LED red, and device.info lists them with
// what to do, so a management client can say what's wrong.
//
// Most last until the device restarts. phones clears itself once
// advertising starts.
#pragma once

#include <cstdint>
#include <string>

#include "cJSON.h"

namespace ktb::errors {

enum class Error : uint8_t {
    storage,    // NVS can't be used
    bluetooth,  // the controller, Bluedroid or SPP didn't start
    phones,     // the BLE KISS service or advertising didn't start
    device_key, // the device's key couldn't be loaded or made
    debug,      // debug.set error=true
};

// Sets an error with a message saying what happened and what to do, or
// changes its message. Safe from any task, before or after the control API
// starts.
void set(Error e, const std::string &message);
void clear(Error e);
bool is_set(Error e);
bool any();

// For device.info: [{"code": "bluetooth", "message": "..."}], in the order
// above.
cJSON *json();

} // namespace ktb::errors
