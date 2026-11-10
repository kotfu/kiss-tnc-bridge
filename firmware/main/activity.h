// The control API's activity events: bytes on the radio port and on each
// session, summed and sent at most about ten times a second per object
// and direction.
#pragma once

#include <cstddef>
#include <string>

namespace ktb::activity {

enum class Dir { rx, tx }; // rx toward the bridge, tx away from it

// Starts the timer that sends the sums.
void start();

// True if anyone subscribed to activity: check before counting.
bool wanted();

// Counts bytes on an object ("ktbr0/radio", "ktbr0/phones/<address>").
void add(const std::string &object, Dir dir, size_t bytes);

} // namespace ktb::activity
