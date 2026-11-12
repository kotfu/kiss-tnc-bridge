#include "led.h"

#include "board.h"
#include "driver/rmt_encoder.h"
#include "driver/rmt_tx.h"
#include "esp_check.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"

namespace ktb::led {

namespace {

constexpr const char *TAG = "led";
constexpr uint32_t resolution_hz = 10'000'000; // 0.1 us ticks

// SK6812 bit timings: 0 is 0.3 us high, 0.9 us low; 1 is 0.6 us high,
// 0.6 us low; a frame ends with at least 80 us low.
constexpr rmt_symbol_word_t bit0 = {.duration0 = 3, .level0 = 1, .duration1 = 9, .level1 = 0};
constexpr rmt_symbol_word_t bit1 = {.duration0 = 6, .level0 = 1, .duration1 = 6, .level1 = 0};
constexpr rmt_symbol_word_t latch = {.duration0 = 500, .level0 = 0, .duration1 = 500, .level1 = 0};

rmt_channel_handle_t channel;
rmt_encoder_handle_t encoder;
uint8_t grb[3];
Rgb last{1, 1, 1}; // not a color we'd show, so the first show() writes

size_t encode(const void *data, size_t size, size_t written, size_t free,
              rmt_symbol_word_t *symbols, bool *done, void *)
{
    if (free < 8) {
        return 0;
    }
    size_t byte = written / 8;
    if (byte < size) {
        uint8_t v = static_cast<const uint8_t *>(data)[byte];
        for (int i = 0; i < 8; i++) {
            symbols[i] = (v & (0x80 >> i)) ? bit1 : bit0;
        }
        return 8;
    }
    symbols[0] = latch;
    *done = true;
    return 1;
}

// led_brightness is perceptual: squaring it gives even steps to the eye.
uint8_t scale(uint8_t c, float k)
{
    if (c == 0 || k <= 0) {
        return 0;
    }
    float v = c * k + 0.5f;
    if (v < 1) {
        return 1; // dim, but still the right hue
    }
    return v > 255 ? 255 : static_cast<uint8_t>(v);
}

} // namespace

// Boards without an RGB LED show nothing here.
constexpr bool present = board::rgb_led != GPIO_NUM_NC;

void init()
{
    if constexpr (!present) {
        return;
    }
    rmt_tx_channel_config_t cfg = {};
    cfg.gpio_num = board::rgb_led;
    cfg.clk_src = RMT_CLK_SRC_DEFAULT;
    cfg.resolution_hz = resolution_hz;
    cfg.mem_block_symbols = 64;
    cfg.trans_queue_depth = 2;
    ESP_ERROR_CHECK(rmt_new_tx_channel(&cfg, &channel));
    rmt_simple_encoder_config_t enc = {};
    enc.callback = encode;
    ESP_ERROR_CHECK(rmt_new_simple_encoder(&enc, &encoder));
    ESP_ERROR_CHECK(rmt_enable(channel));
    show(off, 0);
}

void show(Rgb color, uint8_t brightness, float level)
{
    if constexpr (!present) {
        return;
    }
    float b = brightness / 100.0f;
    float k = b * b * level;
    Rgb out{scale(color.r, k), scale(color.g, k), scale(color.b, k)};
    if (out == last) {
        return;
    }
    rmt_tx_wait_all_done(channel, pdMS_TO_TICKS(10));
    last = out;
    grb[0] = out.g;
    grb[1] = out.r;
    grb[2] = out.b;
    rmt_transmit_config_t tx = {};
    if (rmt_transmit(channel, encoder, grb, sizeof(grb), &tx) != ESP_OK) {
        ESP_LOGW(TAG, "transmit failed");
        last = Rgb{1, 1, 1};
    }
}

} // namespace ktb::led
