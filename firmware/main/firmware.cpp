#include "firmware.h"

#include "esp_app_desc.h"
#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_system.h"
#include "esp_timer.h"

namespace ktb::firmware {

namespace {

constexpr const char *TAG = "firmware";
esp_timer_handle_t deadline;

bool pending_verify()
{
    esp_ota_img_states_t state;
    const esp_partition_t *running = esp_ota_get_running_partition();
    return esp_ota_get_state_partition(running, &state) == ESP_OK &&
           state == ESP_OTA_IMG_PENDING_VERIFY;
}

void deadline_passed(void *)
{
    ESP_LOGE(TAG, "not marked valid within %u s: restarting to roll back",
             (unsigned)self_test_deadline_s);
    esp_restart();
}

} // namespace

void check_boot()
{
    const esp_app_desc_t *app = esp_app_get_description();
    const esp_partition_t *running = esp_ota_get_running_partition();
    ESP_LOGI(TAG, "kiss-tnc-bridge firmware %s, built %s, on %s", app->version, KTB_BUILT,
             running->label);

    if (const esp_partition_t *invalid = esp_ota_get_last_invalid_partition()) {
        esp_app_desc_t desc;
        if (esp_ota_get_partition_description(invalid, &desc) == ESP_OK) {
            ESP_LOGW(TAG, "rolled back from %s on %s", desc.version, invalid->label);
        }
    }
    if (pending_verify()) {
        ESP_LOGW(TAG, "first boot of this image: it has %u s to pass its self-test",
                 (unsigned)self_test_deadline_s);
        esp_timer_create_args_t args = {};
        args.callback = deadline_passed;
        args.name = "self-test";
        ESP_ERROR_CHECK(esp_timer_create(&args, &deadline));
        ESP_ERROR_CHECK(esp_timer_start_once(deadline, self_test_deadline_s * 1000000ULL));
    }
}

void self_test_passed()
{
    if (!pending_verify()) {
        return;
    }
    if (deadline) {
        esp_timer_stop(deadline);
    }
    esp_ota_mark_app_valid_cancel_rollback();
    ESP_LOGI(TAG, "self-test passed: image marked valid");
}

cJSON *info_json()
{
    const esp_app_desc_t *app = esp_app_get_description();
    const esp_partition_t *running = esp_ota_get_running_partition();
    cJSON *o = cJSON_CreateObject();
    cJSON_AddStringToObject(o, "version", app->version);
    cJSON_AddStringToObject(o, "built", KTB_BUILT);
    cJSON_AddStringToObject(o, "partition", running->label);
    cJSON_AddStringToObject(o, "state", pending_verify() ? "pending_verify" : "valid");
    if (const esp_partition_t *invalid = esp_ota_get_last_invalid_partition()) {
        esp_app_desc_t desc;
        if (esp_ota_get_partition_description(invalid, &desc) == ESP_OK) {
            cJSON_AddStringToObject(o, "rolled_back", desc.version);
        }
    }
    return o;
}

} // namespace ktb::firmware
