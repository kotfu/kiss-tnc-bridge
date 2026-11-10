#include "settings.h"

#include <atomic>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <mutex>

#include "esp_log.h"
#include "esp_mac.h"
#include "nvs.h"

namespace ktb::settings {

namespace {

constexpr const char *TAG = "settings";

// Limits. Only about 11 characters of the name fit in the advertisement
// (docs/web-installer.md); apps that connect read the whole name.
constexpr size_t public_name_max = 32;
constexpr uint16_t wake_min = 5;
constexpr uint16_t wake_max = 3600;

// Defaults (provisional, docs/atom-lite-ui.md)
constexpr LedNormal default_led_normal = LedNormal::solid;
constexpr uint16_t default_wake_seconds = 30;
constexpr uint8_t default_brightness = 50;

// NVS keys
constexpr const char *key_name = "public_name";
constexpr const char *key_led_normal = "led_normal";
constexpr const char *key_wake = "led_wake_s";
constexpr const char *key_brightness = "led_bright";

std::mutex name_lock;
std::string name;
std::atomic<LedNormal> normal{default_led_normal};
std::atomic<uint16_t> wake_seconds{default_wake_seconds};
std::atomic<uint8_t> brightness{default_brightness};

const char *led_normal_name(LedNormal v)
{
    switch (v) {
    case LedNormal::breathe: return "breathe";
    case LedNormal::dark: return "dark";
    case LedNormal::dim: return "dim";
    default: return "solid";
    }
}

bool parse_led_normal(const char *s, LedNormal &out)
{
    if (strcmp(s, "solid") == 0) {
        out = LedNormal::solid;
    } else if (strcmp(s, "breathe") == 0) {
        out = LedNormal::breathe;
    } else if (strcmp(s, "dark") == 0) {
        out = LedNormal::dark;
    } else if (strcmp(s, "dim") == 0) {
        out = LedNormal::dim;
    } else {
        return false;
    }
    return true;
}

// "KTB-F272": the last two bytes of the Bluetooth address, so two new
// devices don't advertise the same name.
std::string default_name()
{
    uint8_t mac[6] = {};
    esp_read_mac(mac, ESP_MAC_BT);
    char buf[16];
    snprintf(buf, sizeof(buf), "KTB-%02X%02X", mac[4], mac[5]);
    return buf;
}

bool valid_name(const char *s, std::string &error)
{
    size_t len = strlen(s);
    if (len == 0 || len > public_name_max) {
        error = "public_name must be 1 to 32 bytes";
        return false;
    }
    for (size_t i = 0; i < len; i++) {
        if (static_cast<unsigned char>(s[i]) < 0x20 || s[i] == 0x7f) {
            error = "public_name can't contain control characters";
            return false;
        }
    }
    return true;
}

// A whole number in [lo, hi] from a JSON number.
bool whole_number(const cJSON *item, double lo, double hi, long &out)
{
    if (!cJSON_IsNumber(item)) {
        return false;
    }
    double v = item->valuedouble;
    if (v != std::floor(v) || v < lo || v > hi) {
        return false;
    }
    out = static_cast<long>(v);
    return true;
}

} // namespace

void load()
{
    std::string loaded_name = default_name();
    nvs_handle_t h;
    if (nvs_open(nvs_namespace, NVS_READONLY, &h) == ESP_OK) {
        char buf[public_name_max + 1];
        size_t len = sizeof(buf);
        std::string ignored;
        if (nvs_get_str(h, key_name, buf, &len) == ESP_OK && valid_name(buf, ignored)) {
            loaded_name = buf;
        }
        uint8_t u8;
        if (nvs_get_u8(h, key_led_normal, &u8) == ESP_OK && u8 <= static_cast<uint8_t>(LedNormal::dim)) {
            normal = static_cast<LedNormal>(u8);
        }
        uint16_t u16;
        if (nvs_get_u16(h, key_wake, &u16) == ESP_OK && u16 >= wake_min && u16 <= wake_max) {
            wake_seconds = u16;
        }
        if (nvs_get_u8(h, key_brightness, &u8) == ESP_OK && u8 <= 100) {
            brightness = u8;
        }
        nvs_close(h);
    }
    {
        std::lock_guard<std::mutex> g(name_lock);
        name = loaded_name;
    }
    ESP_LOGI(TAG, "public_name \"%s\", led_normal %s, led_wake_seconds %u, led_brightness %u",
             loaded_name.c_str(), led_normal_name(normal), (unsigned)wake_seconds,
             (unsigned)brightness);
}

std::string public_name()
{
    std::lock_guard<std::mutex> g(name_lock);
    return name;
}

LedNormal led_normal() { return normal; }
uint16_t led_wake_seconds() { return wake_seconds; }
uint8_t led_brightness() { return brightness; }

cJSON *to_json()
{
    cJSON *o = cJSON_CreateObject();
    cJSON_AddStringToObject(o, "public_name", public_name().c_str());
    cJSON_AddStringToObject(o, "led_normal", led_normal_name(normal));
    cJSON_AddNumberToObject(o, "led_wake_seconds", wake_seconds);
    cJSON_AddNumberToObject(o, "led_brightness", brightness);
    return o;
}

bool apply(const cJSON *obj, std::string &error, std::vector<std::string> &reboot_required)
{
    (void)reboot_required; // none of these need one yet
    if (!cJSON_IsObject(obj) || obj->child == nullptr) {
        error = "settings must be an object with at least one setting";
        return false;
    }

    // Check everything first, so a bad value changes nothing.
    std::string new_name = public_name();
    LedNormal new_normal = normal;
    uint16_t new_wake = wake_seconds;
    uint8_t new_brightness = brightness;

    for (const cJSON *item = obj->child; item; item = item->next) {
        const char *key = item->string;
        long n;
        if (strcmp(key, "public_name") == 0) {
            if (!cJSON_IsString(item) || !valid_name(item->valuestring, error)) {
                if (error.empty()) {
                    error = "public_name must be a string";
                }
                return false;
            }
            new_name = item->valuestring;
        } else if (strcmp(key, "led_normal") == 0) {
            if (!cJSON_IsString(item) || !parse_led_normal(item->valuestring, new_normal)) {
                error = "led_normal must be \"solid\", \"dim\", \"breathe\" or \"dark\"";
                return false;
            }
        } else if (strcmp(key, "led_wake_seconds") == 0) {
            if (!whole_number(item, wake_min, wake_max, n)) {
                error = "led_wake_seconds must be a whole number from 5 to 3600";
                return false;
            }
            new_wake = static_cast<uint16_t>(n);
        } else if (strcmp(key, "led_brightness") == 0) {
            if (!whole_number(item, 0, 100, n)) {
                error = "led_brightness must be a whole number from 0 to 100";
                return false;
            }
            new_brightness = static_cast<uint8_t>(n);
        } else {
            error = std::string("unknown setting: ") + key +
                    " (known: public_name, led_normal, led_wake_seconds, led_brightness)";
            return false;
        }
    }

    nvs_handle_t h;
    esp_err_t err = nvs_open(nvs_namespace, NVS_READWRITE, &h);
    if (err == ESP_OK) {
        err = nvs_set_str(h, key_name, new_name.c_str());
        if (err == ESP_OK) {
            err = nvs_set_u8(h, key_led_normal, static_cast<uint8_t>(new_normal));
        }
        if (err == ESP_OK) {
            err = nvs_set_u16(h, key_wake, new_wake);
        }
        if (err == ESP_OK) {
            err = nvs_set_u8(h, key_brightness, new_brightness);
        }
        if (err == ESP_OK) {
            err = nvs_commit(h);
        }
        nvs_close(h);
    }
    if (err != ESP_OK) {
        error = std::string("couldn't save the settings: ") + esp_err_to_name(err);
        return false;
    }

    {
        std::lock_guard<std::mutex> g(name_lock);
        name = new_name;
    }
    normal = new_normal;
    wake_seconds = new_wake;
    brightness = new_brightness;
    ESP_LOGI(TAG, "saved");
    return true;
}

} // namespace ktb::settings
