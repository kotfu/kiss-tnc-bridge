// kiss-tnc-bridge firmware for the M5Stack ATOM Lite. See README.md.

#include "auth.h"
#include "console.h"
#include "control.h"
#include "esp_log.h"
#include "esp_system.h"
#include "firmware.h"
#include "radio.h"
#include "settings.h"
#include "storage.h"
#include "ui.h"

namespace {
constexpr const char *TAG = "main";
}

extern "C" void app_main()
{
    using namespace ktb;

    console::start();
    firmware::check_boot();
    storage::init();

    // Reset comes before any setting is read or Bluetooth starts, so no
    // setting can get in its way (docs/atom-lite-ui.md, Reset).
    if (ui::reset_requested_at_boot()) {
        storage::reset();
        ui::show_reset_done_blocking();
        console::flush();
        esp_restart();
    }

    settings::load();
    bool have_key = auth::load();
    ui::start();
    radio::start();
    if (!have_key) {
        // Without its key the device can't be managed over BLE; USB still
        // works, and a clean install makes a new key.
        ui::set_error(true);
    }

    console::serve(control::handle_line);

    // The self-test: settings loaded, the key in hand, Bluetooth up, the UI
    // and the control API running. Milestone 3 adds BLE advertising. It
    // doesn't wait for the radio, which may be off.
    if (have_key) {
        firmware::self_test_passed();
    }
    ESP_LOGI(TAG, "ready");
}
