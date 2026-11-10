// The radio: Bluetooth Classic SPP to a Kenwood TH-D74 or TH-D75 (or any
// SPP TNC in KISS mode), its KISS init, reconnecting, and pairing.
//
// One task owns all of it. The button and the control API send it
// requests, and it answers them as it goes; Bluedroid's callbacks only
// pass it events. In the control API it's the `radio` TNC and the `radio`
// TNC port of the fixed bridge `ktbr0`.
#pragma once

#include <atomic>
#include <cstdint>
#include <string>

#include "cJSON.h"
#include "control.h"

namespace ktb::radio {

// What the LED shows of the radio.
enum class Link : uint8_t {
    none,      // no radio paired
    down,      // paired, not connected (or connecting)
    connected, // bridging: connected and in KISS mode
};

// Starts Bluetooth (both Classic and BLE) and the radio task. Call after
// settings load. False if Bluetooth didn't start, which is the bluetooth
// error: the radio's commands then fail at once.
bool start();

Link link();
bool pairing();

// Phones with a KISS session (or what debug.set says).
int phones();

// A frame from a phone (stuffed, without FENDs) for the radio. Safe from
// the Bluetooth task: it only queues.
void send_frame(const uint8_t *frame, size_t len, const std::string &session);

// Pairing searches for 60 seconds and keeps the old radio until a new one
// pairs (docs/atom-lite-ui.md, Pairing).
constexpr uint32_t pairing_seconds = 60;

// The button: pair with the first TH-D74 or TH-D75 found in pairing mode,
// confirming its number at once; or cancel that.
void start_pairing();
void cancel_pairing();

// The control API. Each answers p, at once or later, from the radio task.
void scan(const control::Pending &p, uint32_t seconds);
void scan_stop(const control::Pending &p, double scan_request);
// Pairs with address, or the radio's own address if address is empty.
void pair(const control::Pending &p, const std::string &address);
void pair_reply(const control::Pending &p, double pair_request, bool accept);
void unpair(const control::Pending &p, const std::string &address);
// tnc.update: model and/or address may be empty to keep them.
void update(const control::Pending &p, const std::string &model, const std::string &address,
            bool non_disruptive);
// port.connect, port.disconnect, port.release
enum class Hold : uint8_t { none, up, down };
void hold(const control::Pending &p, Hold h);

// public_name changed: the name the radio sees when pairing.
void name_changed();

// For status: the adapters, tncs and bridges arrays. Safe from any task.
cJSON *status_json();
// For tnc.models.
cJSON *models_json();

// Forgets every Bluetooth bond through Bluedroid, if it's running, so it
// doesn't write them back after reset erases its storage.
void remove_all_bonds();

#if CONFIG_KTB_DEBUG_COMMANDS
// debug.set: what the LED shows of the radio, and the phone count (-1
// goes back to the real one).
void debug_override_link(bool on, Link l);
void debug_set_phones(int n);
#endif

// The NVS namespace, for reset.
constexpr const char *nvs_namespace = "radio";

} // namespace ktb::radio
