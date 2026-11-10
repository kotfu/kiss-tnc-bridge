// KISS framing, and AX.25 decoding for display (the control API's frame
// events). The bridge never acts on what's in a frame.
#pragma once

#include <cstddef>
#include <cstdint>
#include <functional>
#include <string>

#include "cJSON.h"

namespace ktb::kiss {

constexpr uint8_t FEND = 0xC0;
constexpr uint8_t FESC = 0xDB;
constexpr uint8_t TFEND = 0xDC;
constexpr uint8_t TFESC = 0xDD;

// Splits a byte stream into frames: the bytes between FENDs, still
// stuffed. Empty frames (back-to-back FENDs) are skipped, and a frame
// longer than max_frame is dropped.
class Splitter {
public:
    static constexpr size_t max_frame = 1024;
    using Handler = std::function<void(const uint8_t *frame, size_t len)>;

    void feed(const uint8_t *data, size_t len, const Handler &on_frame);
    void reset() { len_ = 0; overflow_ = false; }

private:
    uint8_t buf_[max_frame];
    size_t len_ = 0;
    bool overflow_ = false;
};

// A stuffed frame as lowercase hex, as frame events carry it.
std::string hex(const uint8_t *frame, size_t len);

// The "decoded" object of a frame event: channel and command, and for a
// data frame the AX.25 addresses and a one-line summary. Returns null if
// the frame is too short to say anything about.
cJSON *decode(const uint8_t *frame, size_t len);

} // namespace ktb::kiss
