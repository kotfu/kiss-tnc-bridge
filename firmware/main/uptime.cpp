#include "uptime.h"

#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "nvs.h"

namespace ktb::uptime {

namespace {

constexpr const char *TAG = "uptime";

// Its own namespace, so device.reset (storage::reset) leaves it alone.
constexpr const char *nvs_namespace = "uptime";
constexpr const char *key_run = "run";   // this run's uptime, saved each minute
constexpr const char *key_last = "last"; // the last run's
constexpr uint32_t save_every_s = 60;

int64_t last = -1;

// Saves this run's uptime each minute. NVS writes are power-safe, and one
// a minute is far inside the flash's wear.
void saver(void *)
{
    for (;;) {
        vTaskDelay(pdMS_TO_TICKS(save_every_s * 1000));
        nvs_handle_t h;
        if (nvs_open(nvs_namespace, NVS_READWRITE, &h) != ESP_OK) {
            continue;
        }
        uint32_t s = static_cast<uint32_t>(esp_timer_get_time() / 1000000);
        if (nvs_set_u32(h, key_run, s) == ESP_OK) {
            nvs_commit(h);
        }
        nvs_close(h);
    }
}

} // namespace

void start()
{
    nvs_handle_t h;
    if (nvs_open(nvs_namespace, NVS_READWRITE, &h) == ESP_OK) {
        uint32_t run = 0;
        if (nvs_get_u32(h, key_run, &run) == ESP_OK) {
            // The last run saved at least once: it's the last run now, and
            // this one starts with nothing saved.
            nvs_set_u32(h, key_last, run);
            nvs_erase_key(h, key_run);
            nvs_commit(h);
        }
        uint32_t l = 0;
        if (nvs_get_u32(h, key_last, &l) == ESP_OK) {
            last = l;
        }
        nvs_close(h);
    }
    if (last >= 0) {
        ESP_LOGI(TAG, "the last run lasted %lld s", (long long)last);
    }
    xTaskCreate(saver, "uptime", 2560, nullptr, 1, nullptr);
}

int64_t last_seconds() { return last; }

} // namespace ktb::uptime
