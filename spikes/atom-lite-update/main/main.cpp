// ATOM Lite update spike: the memory spike's bridge (SPP to a TH-D74/D75,
// the BLE KISS service, a stand-in management service) plus firmware
// updates over BLE, rollback and signed images. See README.md.

#include <cinttypes>
#include <cstdio>
#include <vector>

#include "esp_bt.h"
#include "esp_bt_main.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "nvs_flash.h"

#include "spike.h"

namespace spike {

StreamBufferHandle_t from_radio;
StreamBufferHandle_t to_radio;
Stats stats;

namespace {

constexpr const char *TAG = "spike";
constexpr uint32_t internal_8bit = MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT;

void log_tasks()
{
    UBaseType_t count = uxTaskGetNumberOfTasks();
    std::vector<TaskStatus_t> tasks(count + 4);
    count = uxTaskGetSystemState(tasks.data(), tasks.size(), nullptr);
    ESP_LOGI(TAG, "tasks: %u (name, priority, stack never used in bytes)", (unsigned)count);
    for (UBaseType_t i = 0; i < count; i++) {
        ESP_LOGI(TAG, "  %-16s prio %2u  stack free %5u", tasks[i].pcTaskName,
                 (unsigned)tasks[i].uxCurrentPriority,
                 (unsigned)(tasks[i].usStackHighWaterMark * sizeof(StackType_t)));
    }
}

void report_task(void *)
{
    const TickType_t interval = pdMS_TO_TICKS(CONFIG_SPIKE_REPORT_INTERVAL_S * 1000);
    unsigned n = 0;
    for (;;) {
        vTaskDelay(interval);
        multi_heap_info_t info;
        heap_caps_get_info(&info, internal_8bit);
        ESP_LOGI(TAG,
                 "t=%" PRIu64 "s heap free=%u min=%u largest=%u | radio %s connects=%" PRIu32
                 " rx=%" PRIu32 "B/%" PRIu32 "f tx=%" PRIu32 "B/%" PRIu32 "f drop=%" PRIu32 "/%" PRIu32
                 " | ble conns=%d kiss_subs=%d notify_fail=%" PRIu32 " mgmt=%" PRIu32,
                 esp_timer_get_time() / 1000000, (unsigned)info.total_free_bytes,
                 (unsigned)info.minimum_free_bytes, (unsigned)info.largest_free_block,
                 stats.radio_bridging ? "bridging" : "down", stats.radio_connects.load(),
                 stats.radio_rx_bytes.load(), stats.radio_rx_frames.load(),
                 stats.radio_tx_bytes.load(), stats.radio_tx_frames.load(),
                 stats.dropped_from_radio.load(), stats.dropped_to_radio.load(),
                 stats.ble_connections.load(), stats.kiss_subscribers.load(),
                 stats.notify_failures.load(), stats.mgmt_messages.load());
        if (++n % 6 == 0) {
            log_tasks();
        }
    }
}

} // namespace

void heap_mark(const char *label)
{
    multi_heap_info_t info;
    heap_caps_get_info(&info, internal_8bit);
    ESP_LOGI(TAG, "heap @ %-22s free=%6u min=%6u largest=%6u", label,
             (unsigned)info.total_free_bytes, (unsigned)info.minimum_free_bytes,
             (unsigned)info.largest_free_block);
}

} // namespace spike

extern "C" void app_main()
{
    using namespace spike;

    heap_mark("app_main");

    esp_err_t err = nvs_flash_init();
    if (err == ESP_ERR_NVS_NO_FREE_PAGES || err == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_ERROR_CHECK(nvs_flash_erase());
        err = nvs_flash_init();
    }
    ESP_ERROR_CHECK(err);
    heap_mark("nvs");

    from_radio = xStreamBufferCreate(4096, 1);
    to_radio = xStreamBufferCreate(2048, 1);
    heap_mark("stream buffers");

    esp_bt_controller_config_t bt_cfg = BT_CONTROLLER_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_bt_controller_init(&bt_cfg));
    heap_mark("controller init");
    ESP_ERROR_CHECK(esp_bt_controller_enable(ESP_BT_MODE_BTDM));
    heap_mark("controller enable");

    esp_bluedroid_config_t bd_cfg = BT_BLUEDROID_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_bluedroid_init_with_cfg(&bd_cfg));
    ESP_ERROR_CHECK(esp_bluedroid_enable());
    heap_mark("bluedroid");

    radio_start();
    heap_mark("spp");

    update_start();
    ble_start();
    heap_mark("gatt registered");

    xTaskCreate(report_task, "report", 3072, nullptr, 2, nullptr);
}
