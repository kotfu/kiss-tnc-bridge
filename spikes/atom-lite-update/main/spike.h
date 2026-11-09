// Shared state for the ATOM Lite update spike.
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
// Paused: the radio link is closed and not reconnected until unpaused.
void radio_pause(bool paused);

// ble.cpp
void ble_start();
// Sends bytes from the radio to every subscribed BLE KISS client.
void ble_send_kiss(const uint8_t *data, size_t len);
// Sends a management message (JSON) to one connection.
void ble_mgmt_send(uint16_t conn_id, const char *json);
// Asks for a connection interval, in 1.25 ms units.
void ble_request_interval(uint16_t conn_id, uint16_t min, uint16_t max);

// update.cpp
void update_start();
// Called once Bluetooth and the GATT services are up. On an image's first
// boot this marks it valid (or, with SPIKE_BREAK_SELF_TEST, crashes).
void update_self_test_passed();
// A complete management message from a connection, as text.
void update_command(uint16_t conn_id, const char *json, size_t len);
// A write to the image data characteristic: 4-byte little-endian offset,
// then image bytes. Runs in the Bluetooth task, so it only queues.
void update_data(uint16_t conn_id, const uint8_t *data, size_t len);
void update_disconnected(uint16_t conn_id);

} // namespace spike
