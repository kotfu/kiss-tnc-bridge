// The radio: Bluetooth Classic SPP to a Kenwood TH-D74 or TH-D75 (or any
// SPP TNC in KISS mode), or BLE to a TIDRADIO TD-H9 as a GATT client; its
// KISS init, reconnecting, and pairing or selecting it.
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
#include "esp_gap_ble_api.h"

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

// The radio as a screen shows it (docs/core2-ui.md, Bridge page). Safe
// from any task.
enum class State : uint8_t {
    none,       // no radio paired or selected
    idle,       // held down, pairing, or Bluetooth didn't start: see detail
    connecting, // connecting, or waiting for a TD-H9 to advertise
    connected,  // bridging
    retrying,   // the link failed; it tries again in retry_in_s
};
struct View {
    bool present = false; // a radio is paired or selected
    bool ble = false;     // a TD-H9
    bool paired = false;  // a Kenwood that's bonded
    std::string name;     // its Bluetooth name, "TH-D75"
    std::string make;     // "Kenwood TH-D75", or "" for an unknown model
    std::string address;
    State state = State::none;
    std::string detail;
    uint32_t state_s = 0;    // seconds in this state
    uint32_t down_s = 0;     // not connected: seconds since it was last (or since starting)
    uint32_t retry_in_s = 0; // retrying: seconds to the next try
    uint32_t rx = 0;         // connected: frames from the radio since it connected
    uint32_t tx = 0;         // and to it
};
View view();

// A frame from a phone (stuffed, without FENDs) for the radio. Safe from
// the Bluetooth task: it only queues.
void send_frame(const uint8_t *frame, size_t len, const std::string &session);

// Pairing searches for 60 seconds and keeps the old radio until a new one
// pairs (docs/atom-lite-ui.md, Pairing).
constexpr uint32_t pairing_seconds = 60;

// The button: pair with the first TH-D74 or TH-D75 found in pairing mode,
// or select the first TD-H9 found; or cancel that. The ATOM confirms the
// number at once; a board with a screen shows it and waits for
// answer_pairing.
void start_pairing();
void cancel_pairing();
void answer_pairing(bool same);

// The pairing as a screen shows it (docs/core2-ui.md, Pairing a radio).
// Safe from any task.
enum class PairStage : uint8_t {
    none,      // not pairing
    searching, // looking for a radio
    pairing,   // found one, connecting to it
    compare,   // the number: does the radio show it? answer_pairing
    press_ok,  // this side confirmed: press OK on the radio
};
struct PairView {
    PairStage stage = PairStage::none;
    std::string name;   // the radio's, from pairing on
    std::string number; // compare and press_ok: "774523"
    uint32_t left_s = 0;  // seconds left in this stage
    uint32_t total_s = 0; // and how long it has in all
};
PairView pairing_view();

// The control API. Each answers p, at once or later, from the radio task.
void scan(const control::Pending &p, uint32_t seconds, bool le);
void scan_stop(const control::Pending &p, double scan_request);
// Pairs with address, or the radio's own address if address is empty.
void pair(const control::Pending &p, const std::string &address);
void pair_reply(const control::Pending &p, double pair_request, bool accept);
void unpair(const control::Pending &p, const std::string &address);
// tnc.update: model and/or address may be empty to keep them. The address
// is spp.address for a Classic model and ble.address for the TD-H9.
void update(const control::Pending &p, const std::string &model, const std::string &address, bool address_ble,
            bool non_disruptive);
// port.connect, port.disconnect, port.release
enum class Hold : uint8_t { none, up, down };
void hold(const control::Pending &p, Hold h);

// public_name changed: the name the radio sees when pairing.
void name_changed();

// Every BLE GAP event, from phones' callback (Bluedroid takes one): the
// radio's scans and its TD-H9 link.
void ble_gap_event(esp_gap_ble_cb_event_t event, esp_ble_gap_cb_param_t *param);

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
