#include "storage.h"

#include "auth.h"
#include "errors.h"
#include "esp_log.h"
#include "nvs.h"
#include "nvs_flash.h"
#include "power.h"
#include "radio.h"
#include "settings.h"

namespace ktb::storage {

namespace {

constexpr const char *TAG = "storage";

// Bluedroid keeps its bonds, the radio's link key among them, in this NVS
// namespace.
constexpr const char *bluedroid_namespace = "bt_config.conf";

bool erase_namespace(const char *ns)
{
    nvs_handle_t h;
    esp_err_t err = nvs_open(ns, NVS_READWRITE, &h);
    if (err == ESP_ERR_NVS_NOT_FOUND) {
        return true; // never written
    }
    if (err == ESP_OK) {
        err = nvs_erase_all(h);
        if (err == ESP_OK) {
            err = nvs_commit(h);
        }
        nvs_close(h);
    }
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "couldn't erase %s: %s", ns, esp_err_to_name(err));
        return false;
    }
    return true;
}

} // namespace

bool init()
{
    esp_err_t err = nvs_flash_init();
    if (err == ESP_ERR_NVS_NO_FREE_PAGES || err == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        // NVS is unreadable as it stands. Erasing it loses the device's key
        // and setup code too, as a clean install would.
        ESP_LOGW(TAG, "NVS unreadable (%s): erasing it", esp_err_to_name(err));
        err = nvs_flash_erase();
        if (err == ESP_OK) {
            err = nvs_flash_init();
        }
    }
    if (err != ESP_OK) {
        // The device carries on with default settings: USB still works, so
        // a client can see the error.
        errors::set(errors::Error::storage,
                    std::string("NVS can't be used (") + esp_err_to_name(err) +
                        "): settings, the radio pairing and the device's key can't be kept. "
                        "Reinstall the firmware with a clean install");
        return false;
    }
    return true;
}

bool reset()
{
    ESP_LOGW(TAG, "reset: clearing settings, the radio pairing and trusted clients");
    radio::remove_all_bonds();
    bool ok = erase_namespace(settings::nvs_namespace);
    ok = erase_namespace(radio::nvs_namespace) && ok;
    ok = erase_namespace(auth::clients_namespace) && ok;
    ok = erase_namespace(power::nvs_namespace) && ok;
    ok = erase_namespace(bluedroid_namespace) && ok;
    return ok;
}

} // namespace ktb::storage
