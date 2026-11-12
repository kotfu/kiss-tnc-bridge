// The phones: the BLE KISS API service
// (https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md) that
// KISS TNC apps connect to, the `phones` app port of the bridge ktbr0.
//
// Each phone subscribed to KISS notifications is a session,
// "ktbr0/phones/<its address>". A phone's writes are reassembled into whole
// frames before they go to the radio, so two phones writing at once don't
// interleave; frames to phones go whole, split to the MTU.
#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

#include "cJSON.h"
#include "control.h"

namespace ktb::phones {

// Registers the GATT service and starts advertising. Call after
// Bluetooth is up (radio::start).
void start();

// Waits until advertising has started, up to timeout_ms: the self-test.
bool wait_advertising(uint32_t timeout_ms);

// Sessions: phones subscribed to KISS notifications.
int sessions();

// The most sessions at once.
int max_sessions();

// Each session as a screen shows it (docs/core2-ui.md, Bridge page): the
// phone's address, how long it's been a session, and frames since then,
// from the phone's side: rx from the phone, tx to it. Safe from any task.
struct SessionView {
    std::string address;
    uint32_t connected_s = 0;
    uint32_t rx = 0;
    uint32_t tx = 0;
};
std::vector<SessionView> session_views();

// A frame from the radio (stuffed, without FENDs) to every session.
// Returns how many got it.
int send_frame(const uint8_t *frame, size_t len);

// The name changed: advertise the new one.
void name_changed();

// session.kick: disconnects the phone with this session id.
void kick(const control::Pending &p, const std::string &session);

// The phones port and the sessions, for status.
cJSON *port_json();

} // namespace ktb::phones
