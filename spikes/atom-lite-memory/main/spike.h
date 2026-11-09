// Shared state for the ATOM Lite memory spike.
#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>

#include "freertos/FreeRTOS.h"
#include "freertos/stream_buffer.h"

namespace spike {

// Bytes from the radio, filled by the SPP callback, drained by the radio task.
extern StreamBufferHandle_t from_radio;
// Bytes from BLE KISS clients, filled by the GATT callback, drained by the
// radio writer task.
extern StreamBufferHandle_t to_radio;

struct Stats {
    std::atomic<uint32_t> radio_connects{0};
    std::atomic<bool> radio_bridging{false};
    std::atomic<uint32_t> radio_rx_bytes{0};
    std::atomic<uint32_t> radio_rx_frames{0};
    std::atomic<uint32_t> radio_tx_bytes{0};
    std::atomic<uint32_t> radio_tx_frames{0};
    std::atomic<uint32_t> dropped_from_radio{0};
    std::atomic<uint32_t> dropped_to_radio{0};
    std::atomic<int> ble_connections{0};
    std::atomic<int> kiss_subscribers{0};
    std::atomic<uint32_t> notify_failures{0};
    std::atomic<uint32_t> mgmt_messages{0};
};
extern Stats stats;

// Logs free, minimum-ever and largest-block internal heap, tagged with a
// milestone, so the log shows what each stage of start-up costs.
void heap_mark(const char *label);

// Counts KISS frames in a byte stream: a FEND that ends a non-empty frame.
class FrameCounter {
public:
    uint32_t feed(const uint8_t *data, size_t len)
    {
        uint32_t frames = 0;
        for (size_t i = 0; i < len; i++) {
            if (data[i] == 0xC0) {
                if (in_frame_) {
                    frames++;
                }
                in_frame_ = false;
            } else {
                in_frame_ = true;
            }
        }
        return frames;
    }

private:
    bool in_frame_ = false;
};

// radio.cpp
void radio_start();

// ble.cpp
void ble_start();
// Sends bytes from the radio to every subscribed BLE KISS client.
void ble_send_kiss(const uint8_t *data, size_t len);

} // namespace spike
