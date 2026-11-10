#include "activity.h"

#include <mutex>
#include <vector>

#include "control.h"
#include "esp_timer.h"

namespace ktb::activity {

namespace {

constexpr uint64_t interval_us = 100'000;

struct Slot {
    std::string object;
    Dir dir;
    uint64_t pending;
};

std::mutex lock;
std::vector<Slot> slots;
esp_timer_handle_t timer;

void flush(void *)
{
    std::vector<Slot> due;
    {
        std::lock_guard<std::mutex> g(lock);
        for (Slot &s : slots) {
            if (s.pending) {
                due.push_back(s);
                s.pending = 0;
            }
        }
        // Objects that went quiet are dropped, so departed sessions don't
        // linger.
        if (due.empty()) {
            slots.clear();
        }
    }
    for (const Slot &s : due) {
        cJSON *e = cJSON_CreateObject();
        cJSON_AddStringToObject(e, "event", "activity");
        cJSON_AddStringToObject(e, "object", s.object.c_str());
        cJSON_AddStringToObject(e, "dir", s.dir == Dir::rx ? "rx" : "tx");
        cJSON_AddNumberToObject(e, "bytes", static_cast<double>(s.pending));
        control::publish(control::topic_activity, e);
    }
}

} // namespace

void start()
{
    esp_timer_create_args_t args = {};
    args.callback = flush;
    args.name = "activity";
    ESP_ERROR_CHECK(esp_timer_create(&args, &timer));
    ESP_ERROR_CHECK(esp_timer_start_periodic(timer, interval_us));
}

bool wanted() { return control::subscribed(control::topic_activity); }

void add(const std::string &object, Dir dir, size_t bytes)
{
    std::lock_guard<std::mutex> g(lock);
    for (Slot &s : slots) {
        if (s.dir == dir && s.object == object) {
            s.pending += bytes;
            return;
        }
    }
    slots.push_back(Slot{object, dir, bytes});
}

} // namespace ktb::activity
