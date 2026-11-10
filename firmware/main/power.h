// Keeping the draw up for a Tail Bat, whose power chip cuts the output
// below 45 mA (docs/roadmap.md, Power saving). Two ways, both measured on
// a device before either becomes a setting:
//
// - Modem sleep off: the Bluetooth controller stays powered between its
//   scheduled events.
// - Spinning idle cores: a core with nothing to do runs its idle loop
//   instead of waiting for an interrupt, so the CPU draws as if busy.
//   Through the idle hook, so it never takes time from a task.
//
// For now they're debug.set knobs, kept in NVS so they survive the move
// from USB to the Tail Bat, in the development build only.
#pragma once

#include "cJSON.h"
#include "sdkconfig.h"

namespace ktb::power {

// Applies the stored knobs. Call after Bluetooth starts.
void start();

#if CONFIG_KTB_DEBUG_COMMANDS
// debug.set modem_sleep=, spin_cores= (0, 1 or 2). Applied at once and
// stored.
void set_modem_sleep(bool on);
void set_spin_cores(int n);
// For debug.set's reply.
cJSON *json();
#endif

// The NVS namespace, for reset.
constexpr const char *nvs_namespace = "power";

} // namespace ktb::power
