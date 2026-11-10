// The radio link: pairing and the SPP connection to the TH-D74 or TH-D75.
//
// Milestone 1 has no Bluetooth yet. Pairing is a stand-in that shows the
// pairing state for its 60 seconds and then fails, so the button and the
// LED can be tried; milestone 2 replaces it with the real scan and SPP.
#pragma once

#include <atomic>
#include <cstdint>

namespace ktb::radio {

enum class Link : uint8_t {
    none,      // no radio paired
    down,      // paired, not connected
    connected, // bridging
};

// Reads the pairing from NVS.
void load();

Link link();
bool pairing();

// Pairing scans for 60 seconds and keeps the old radio until a new one
// pairs (docs/atom-lite-ui.md, Pairing).
constexpr uint32_t pairing_seconds = 60;
void start_pairing();
void cancel_pairing();

// How many BLE KISS clients are connected. None until milestone 3.
int phones();

#if CONFIG_KTB_DEBUG_COMMANDS
// debug.set and debug.pairing_result
void debug_set_link(Link l);
void debug_set_phones(int n);
void debug_finish_pairing(bool succeeded);
#endif

// The NVS namespace, for reset.
constexpr const char *nvs_namespace = "radio";

} // namespace ktb::radio
