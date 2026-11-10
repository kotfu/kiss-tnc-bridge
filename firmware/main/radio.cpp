#include "radio.h"

#include "esp_log.h"
#include "esp_timer.h"
#include "nvs.h"
#include "ui.h"

namespace ktb::radio {

namespace {

constexpr const char *TAG = "radio";

std::atomic<Link> link_state{Link::none};
std::atomic<bool> pairing_now{false};
std::atomic<int> phone_count{0};
esp_timer_handle_t pairing_timer;

void finish_pairing(bool succeeded)
{
    bool was = pairing_now.exchange(false);
    if (!was) {
        return;
    }
    esp_timer_stop(pairing_timer);
    if (succeeded) {
        ESP_LOGI(TAG, "pairing succeeded");
        link_state = Link::down;
        ui::post(ui::Event::pairing_succeeded);
    } else {
        ESP_LOGI(TAG, "pairing failed or cancelled");
        ui::post(ui::Event::pairing_failed);
    }
}

void pairing_timeout(void *)
{
    ESP_LOGI(TAG, "pairing timed out");
    finish_pairing(false);
}

} // namespace

void load()
{
    esp_timer_create_args_t args = {};
    args.callback = pairing_timeout;
    args.name = "pairing";
    ESP_ERROR_CHECK(esp_timer_create(&args, &pairing_timer));

    nvs_handle_t h;
    uint8_t addr[6];
    size_t len = sizeof(addr);
    if (nvs_open(nvs_namespace, NVS_READONLY, &h) == ESP_OK) {
        if (nvs_get_blob(h, "address", addr, &len) == ESP_OK && len == sizeof(addr)) {
            link_state = Link::down;
        }
        nvs_close(h);
    }
    ESP_LOGI(TAG, "%s", link_state == Link::none ? "no radio paired" : "radio paired");
}

Link link() { return link_state; }
bool pairing() { return pairing_now; }
int phones() { return phone_count; }

void start_pairing()
{
    if (pairing_now.exchange(true)) {
        return;
    }
    ESP_LOGI(TAG, "pairing for %u s (stand-in: no Bluetooth yet)", (unsigned)pairing_seconds);
    esp_timer_start_once(pairing_timer, static_cast<uint64_t>(pairing_seconds) * 1000000);
}

void cancel_pairing()
{
    finish_pairing(false);
}

#if CONFIG_KTB_DEBUG_COMMANDS
void debug_set_link(Link l) { link_state = l; }
void debug_set_phones(int n) { phone_count = n; }
void debug_finish_pairing(bool succeeded) { finish_pairing(succeeded); }
#endif

} // namespace ktb::radio
