#include "settings.h"

#include <atomic>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <mutex>

#include "board.h"
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
constexpr LedNormal default_led_normal = LedNormal::bright;
constexpr uint16_t default_wake_seconds = 30;
constexpr uint8_t default_brightness = 50;
constexpr ScreenNormal default_screen_normal = ScreenNormal::dim;
constexpr uint16_t default_screen_timeout = 30;
constexpr LedWhen default_led_when = LedWhen::screen_dark;
constexpr LedPattern default_led_pattern = LedPattern::breathe;
constexpr bool default_led_blink = true;

// NVS keys
constexpr const char *key_name = "public_name";
constexpr const char *key_led_normal = "led_normal";
constexpr const char *key_wake = "led_wake_s";
constexpr const char *key_brightness = "led_bright";
constexpr const char *key_screen_normal = "screen_normal";
constexpr const char *key_screen_timeout = "screen_tmo_s";
constexpr const char *key_led_when = "led_when";
constexpr const char *key_led_pattern = "led_pattern";
constexpr const char *key_led_blink = "led_blink";

std::mutex name_lock;
std::string name;
std::atomic<LedNormal> normal{default_led_normal};
std::atomic<uint16_t> wake_seconds{default_wake_seconds};
std::atomic<uint8_t> brightness{default_brightness};
std::atomic<ScreenNormal> screen{default_screen_normal};
std::atomic<uint16_t> screen_timeout{default_screen_timeout};
std::atomic<LedWhen> when{default_led_when};
std::atomic<LedPattern> pattern{default_led_pattern};
std::atomic<bool> blink{default_led_blink};

const char *screen_normal_name(ScreenNormal v)
{
    switch (v) {
    case ScreenNormal::bright: return "bright";
    case ScreenNormal::dark: return "dark";
    case ScreenNormal::breathe: return "breathe";
    default: return "dim";
    }
}

bool parse_screen_normal(const char *s, ScreenNormal &out)
{
    if (strcmp(s, "bright") == 0) {
        out = ScreenNormal::bright;
    } else if (strcmp(s, "dim") == 0) {
        out = ScreenNormal::dim;
    } else if (strcmp(s, "dark") == 0) {
        out = ScreenNormal::dark;
    } else if (board::screen_breathe && strcmp(s, "breathe") == 0) {
        out = ScreenNormal::breathe;
    } else {
        return false;
    }
    return true;
}

const char *led_when_name(LedWhen v)
{
    switch (v) {
    case LedWhen::always: return "always";
    case LedWhen::never: return "never";
    default: return "screen_dark";
    }
}

bool parse_led_when(const char *s, LedWhen &out)
{
    if (strcmp(s, "always") == 0) {
        out = LedWhen::always;
    } else if (strcmp(s, "screen_dark") == 0) {
        out = LedWhen::screen_dark;
    } else if (strcmp(s, "never") == 0) {
        out = LedWhen::never;
    } else {
        return false;
    }
    return true;
}

const char *led_pattern_name(LedPattern v)
{
    switch (v) {
    case LedPattern::bright: return "bright";
    case LedPattern::dim: return "dim";
    default: return "breathe";
    }
}

bool parse_led_pattern(const char *s, LedPattern &out)
{
    if (strcmp(s, "bright") == 0) {
        out = LedPattern::bright;
    } else if (strcmp(s, "dim") == 0) {
        out = LedPattern::dim;
    } else if (strcmp(s, "breathe") == 0) {
        out = LedPattern::breathe;
    } else {
        return false;
    }
    return true;
}

const char *led_normal_name(LedNormal v)
{
    switch (v) {
    case LedNormal::breathe: return "breathe";
    case LedNormal::dark: return "dark";
    case LedNormal::dim: return "dim";
    default: return "bright";
    }
}

bool parse_led_normal(const char *s, LedNormal &out)
{
    if (strcmp(s, "bright") == 0) {
        out = LedNormal::bright;
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
        if (nvs_get_u8(h, key_screen_normal, &u8) == ESP_OK && u8 <= static_cast<uint8_t>(ScreenNormal::breathe)) {
            // breathe, saved on a board that no longer has it (the StickC's
            // red LED has its own settings), is dark.
            screen = !board::screen_breathe && u8 == static_cast<uint8_t>(ScreenNormal::breathe)
                         ? ScreenNormal::dark
                         : static_cast<ScreenNormal>(u8);
        }
        if (nvs_get_u16(h, key_screen_timeout, &u16) == ESP_OK && u16 >= wake_min && u16 <= wake_max) {
            screen_timeout = u16;
        }
        if (nvs_get_u8(h, key_led_when, &u8) == ESP_OK && u8 <= static_cast<uint8_t>(LedWhen::never)) {
            when = static_cast<LedWhen>(u8);
        }
        if (nvs_get_u8(h, key_led_pattern, &u8) == ESP_OK && u8 <= static_cast<uint8_t>(LedPattern::breathe)) {
            pattern = static_cast<LedPattern>(u8);
        }
        if (nvs_get_u8(h, key_led_blink, &u8) == ESP_OK && u8 <= 1) {
            blink = u8 == 1;
        }
        nvs_close(h);
    }
    {
        std::lock_guard<std::mutex> g(name_lock);
        name = loaded_name;
    }
    if constexpr (board::screen_settings) {
        ESP_LOGI(TAG, "public_name \"%s\", screen_normal %s, screen_timeout_seconds %u", loaded_name.c_str(),
                 screen_normal_name(screen), (unsigned)screen_timeout);
        if constexpr (board::status_led) {
            ESP_LOGI(TAG, "led_when %s, led_pattern %s, led_activity_blink %s", led_when_name(when),
                     led_pattern_name(pattern), blink ? "true" : "false");
        }
    } else {
        ESP_LOGI(TAG, "public_name \"%s\", led_normal %s, led_wake_seconds %u, led_brightness %u",
                 loaded_name.c_str(), led_normal_name(normal), (unsigned)wake_seconds,
                 (unsigned)brightness);
    }
}

std::string public_name()
{
    std::lock_guard<std::mutex> g(name_lock);
    return name;
}

LedNormal led_normal() { return normal; }
uint16_t led_wake_seconds() { return wake_seconds; }
uint8_t led_brightness() { return brightness; }
ScreenNormal screen_normal() { return screen; }
uint16_t screen_timeout_seconds() { return screen_timeout; }
LedWhen led_when() { return when; }
LedPattern led_pattern() { return pattern; }
bool led_activity_blink() { return blink; }

cJSON *to_json()
{
    cJSON *o = cJSON_CreateObject();
    cJSON_AddStringToObject(o, "public_name", public_name().c_str());
    if constexpr (board::screen_settings) {
        cJSON_AddStringToObject(o, "screen_normal", screen_normal_name(screen));
        cJSON_AddNumberToObject(o, "screen_timeout_seconds", screen_timeout);
        if constexpr (board::status_led) {
            cJSON_AddStringToObject(o, "led_when", led_when_name(when));
            cJSON_AddStringToObject(o, "led_pattern", led_pattern_name(pattern));
            cJSON_AddBoolToObject(o, "led_activity_blink", blink);
        }
    } else {
        cJSON_AddStringToObject(o, "led_normal", led_normal_name(normal));
        cJSON_AddNumberToObject(o, "led_wake_seconds", wake_seconds);
        cJSON_AddNumberToObject(o, "led_brightness", brightness);
    }
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
    bool name_given = false;      // only a name given is saved, so the default stays the default
    bool name_to_default = false; // null: forget the name set, and follow default_name()
    LedNormal new_normal = normal;
    uint16_t new_wake = wake_seconds;
    uint8_t new_brightness = brightness;
    ScreenNormal new_screen = screen;
    uint16_t new_screen_timeout = screen_timeout;
    LedWhen new_when = when;
    LedPattern new_pattern = pattern;
    bool new_blink = blink;

    for (const cJSON *item = obj->child; item; item = item->next) {
        const char *key = item->string;
        long n;
        if (strcmp(key, "public_name") == 0) {
            name_given = true;
            if (cJSON_IsNull(item)) {
                new_name = default_name();
                name_to_default = true;
                continue;
            }
            if (!cJSON_IsString(item) || !valid_name(item->valuestring, error)) {
                if (error.empty()) {
                    error = "public_name must be a string, or null for the default";
                }
                return false;
            }
            new_name = item->valuestring;
            name_to_default = false;
        } else if (board::screen_settings && strcmp(key, "screen_normal") == 0) {
            if (!cJSON_IsString(item) || !parse_screen_normal(item->valuestring, new_screen)) {
                error = board::screen_breathe ? "screen_normal must be \"bright\", \"dim\", \"dark\" or \"breathe\""
                                              : "screen_normal must be \"bright\", \"dim\" or \"dark\"";
                return false;
            }
        } else if (board::screen_settings && strcmp(key, "screen_timeout_seconds") == 0) {
            if (!whole_number(item, wake_min, wake_max, n)) {
                error = "screen_timeout_seconds must be a whole number from 5 to 3600";
                return false;
            }
            new_screen_timeout = static_cast<uint16_t>(n);
        } else if (board::status_led && strcmp(key, "led_when") == 0) {
            if (!cJSON_IsString(item) || !parse_led_when(item->valuestring, new_when)) {
                error = "led_when must be \"always\", \"screen_dark\" or \"never\"";
                return false;
            }
        } else if (board::status_led && strcmp(key, "led_pattern") == 0) {
            if (!cJSON_IsString(item) || !parse_led_pattern(item->valuestring, new_pattern)) {
                error = "led_pattern must be \"bright\", \"dim\" or \"breathe\"";
                return false;
            }
        } else if (board::status_led && strcmp(key, "led_activity_blink") == 0) {
            if (!cJSON_IsBool(item)) {
                error = "led_activity_blink must be true or false";
                return false;
            }
            new_blink = cJSON_IsTrue(item);
        } else if (!board::screen_settings && strcmp(key, "led_normal") == 0) {
            if (!cJSON_IsString(item) || !parse_led_normal(item->valuestring, new_normal)) {
                error = "led_normal must be \"bright\", \"dim\", \"breathe\" or \"dark\"";
                return false;
            }
        } else if (!board::screen_settings && strcmp(key, "led_wake_seconds") == 0) {
            if (!whole_number(item, wake_min, wake_max, n)) {
                error = "led_wake_seconds must be a whole number from 5 to 3600";
                return false;
            }
            new_wake = static_cast<uint16_t>(n);
        } else if (!board::screen_settings && strcmp(key, "led_brightness") == 0) {
            if (!whole_number(item, 0, 100, n)) {
                error = "led_brightness must be a whole number from 0 to 100";
                return false;
            }
            new_brightness = static_cast<uint8_t>(n);
        } else {
            error = std::string("unknown setting: ") + key +
                    (board::status_led ? " (known: public_name, screen_normal, screen_timeout_seconds, led_when, "
                                         "led_pattern, led_activity_blink)"
                     : board::screen_settings ? " (known: public_name, screen_normal, screen_timeout_seconds)"
                                              : " (known: public_name, led_normal, led_wake_seconds, led_brightness)");
            return false;
        }
    }

    nvs_handle_t h;
    esp_err_t err = nvs_open(nvs_namespace, NVS_READWRITE, &h);
    if (err == ESP_OK) {
        if (name_to_default) {
            err = nvs_erase_key(h, key_name);
            if (err == ESP_ERR_NVS_NOT_FOUND) {
                err = ESP_OK; // already the default
            }
        } else if (name_given) {
            err = nvs_set_str(h, key_name, new_name.c_str());
        }
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
            err = nvs_set_u8(h, key_screen_normal, static_cast<uint8_t>(new_screen));
        }
        if (err == ESP_OK) {
            err = nvs_set_u16(h, key_screen_timeout, new_screen_timeout);
        }
        if (err == ESP_OK) {
            err = nvs_set_u8(h, key_led_when, static_cast<uint8_t>(new_when));
        }
        if (err == ESP_OK) {
            err = nvs_set_u8(h, key_led_pattern, static_cast<uint8_t>(new_pattern));
        }
        if (err == ESP_OK) {
            err = nvs_set_u8(h, key_led_blink, new_blink ? 1 : 0);
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
    screen = new_screen;
    screen_timeout = new_screen_timeout;
    when = new_when;
    pattern = new_pattern;
    blink = new_blink;
    ESP_LOGI(TAG, "saved");
    return true;
}

} // namespace ktb::settings
