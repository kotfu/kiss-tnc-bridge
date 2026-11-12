#include "errors.h"

#include <array>
#include <atomic>
#include <mutex>

#include "control.h"
#include "esp_log.h"

namespace ktb::errors {

namespace {

constexpr const char *TAG = "errors";
constexpr size_t count = static_cast<size_t>(Error::debug) + 1;

const char *code(Error e)
{
    switch (e) {
    case Error::storage: return "storage";
    case Error::bluetooth: return "bluetooth";
    case Error::phones: return "phones";
    case Error::device_key: return "device_key";
    case Error::debug: return "debug";
    }
    return "unknown";
}

std::mutex lock;
std::array<std::string, count> messages; // under lock
std::atomic<uint32_t> bits{0};

uint32_t bit(Error e) { return 1u << static_cast<uint32_t>(e); }

// Tells subscribers the list changed: they read device.info for it.
void changed(bool now_any)
{
    if (!control::subscribed(control::topic_state)) {
        return;
    }
    cJSON *e = cJSON_CreateObject();
    cJSON_AddStringToObject(e, "event", "state");
    cJSON_AddStringToObject(e, "what", "device");
    cJSON_AddStringToObject(e, "object", "device");
    cJSON_AddStringToObject(e, "state", now_any ? "error" : "ok");
    control::publish(control::topic_state, e);
}

} // namespace

void set(Error e, const std::string &message)
{
    {
        std::lock_guard<std::mutex> g(lock);
        messages[static_cast<size_t>(e)] = message;
    }
    uint32_t was = bits.fetch_or(bit(e));
    if (!(was & bit(e))) {
        ESP_LOGE(TAG, "%s: %s", code(e), message.c_str());
        changed(true);
    }
}

void clear(Error e)
{
    uint32_t was = bits.fetch_and(~bit(e));
    if (was & bit(e)) {
        ESP_LOGI(TAG, "%s cleared", code(e));
        changed((was & ~bit(e)) != 0);
    }
}

bool is_set(Error e) { return bits & bit(e); }

bool any() { return bits != 0; }

std::vector<Item> list()
{
    std::vector<Item> out;
    uint32_t b = bits;
    std::lock_guard<std::mutex> g(lock);
    for (size_t i = 0; i < count; i++) {
        if (b & (1u << i)) {
            out.push_back({static_cast<Error>(i), messages[i]});
        }
    }
    return out;
}

cJSON *json()
{
    cJSON *a = cJSON_CreateArray();
    uint32_t b = bits;
    std::lock_guard<std::mutex> g(lock);
    for (size_t i = 0; i < count; i++) {
        if (b & (1u << i)) {
            cJSON *o = cJSON_CreateObject();
            cJSON_AddStringToObject(o, "code", code(static_cast<Error>(i)));
            cJSON_AddStringToObject(o, "message", messages[i].c_str());
            cJSON_AddItemToArray(a, o);
        }
    }
    return a;
}

} // namespace ktb::errors
