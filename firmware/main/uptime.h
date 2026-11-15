// How long the device ran last time (#23), for battery tests: run it on a
// battery until it dies, power it up, and read last_uptime in device.info
// (or the Core2's System page).
#pragma once

#include <cstdint>

namespace ktb::uptime {

// Moves the uptime the last run saved to last_uptime, then saves this
// run's every minute. Call once, after storage::init and before anything
// that could restart the device on purpose.
void start();

// How long the last run lasted, in seconds, to the minute; -1 if unknown.
// A run shorter than a minute saves nothing, so the restarts of a battery
// near empty leave the run before them here.
int64_t last_seconds();

} // namespace ktb::uptime
