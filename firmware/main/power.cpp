#include "power.h"

#include "esp_bt.h"
#include "esp_freertos_hooks.h"
#include "esp_log.h"
#include "nvs.h"
#include "sdkconfig.h"

namespace ktb::power {

namespace {

constexpr const char *TAG = "power";

bool modem_sleep = true;
int spin_cores = 0;

// Returning false keeps the idle task spinning instead of waiting for an
// interrupt.
bool spin() { return false; }

void apply()
{
    esp_err_t err = modem_sleep ? esp_bt_sleep_enable() : esp_bt_sleep_disable();
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "modem sleep %s: %s", modem_sleep ? "on" : "off", esp_err_to_name(err));
    }
    for (int core = 0; core < 2; core++) {
        esp_deregister_freertos_idle_hook_for_cpu(spin, core);
        if (core < spin_cores) {
            esp_register_freertos_idle_hook_for_cpu(spin, core);
        }
    }
    ESP_LOGI(TAG, "modem sleep %s, %d idle core%s spinning", modem_sleep ? "on" : "off", spin_cores,
             spin_cores == 1 ? "" : "s");
}

#if CONFIG_KTB_DEBUG_COMMANDS
void save()
{
    nvs_handle_t h;
    if (nvs_open(nvs_namespace, NVS_READWRITE, &h) != ESP_OK) {
        return;
    }
    nvs_set_u8(h, "modem_sleep", modem_sleep ? 1 : 0);
    nvs_set_u8(h, "spin_cores", static_cast<uint8_t>(spin_cores));
    nvs_commit(h);
    nvs_close(h);
}
#endif

} // namespace

void start()
{
#if CONFIG_KTB_DEBUG_COMMANDS
    nvs_handle_t h;
    if (nvs_open(nvs_namespace, NVS_READONLY, &h) == ESP_OK) {
        uint8_t v;
        if (nvs_get_u8(h, "modem_sleep", &v) == ESP_OK) {
            modem_sleep = v != 0;
        }
        if (nvs_get_u8(h, "spin_cores", &v) == ESP_OK && v <= 2) {
            spin_cores = v;
        }
        nvs_close(h);
    }
#endif
    apply();
}

#if CONFIG_KTB_DEBUG_COMMANDS
void set_modem_sleep(bool on)
{
    modem_sleep = on;
    apply();
    save();
}

void set_spin_cores(int n)
{
    spin_cores = n;
    apply();
    save();
}

cJSON *json()
{
    cJSON *o = cJSON_CreateObject();
    cJSON_AddBoolToObject(o, "modem_sleep", modem_sleep);
    cJSON_AddNumberToObject(o, "spin_cores", spin_cores);
    return o;
}
#endif

} // namespace ktb::power
