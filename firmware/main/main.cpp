// kiss-tnc-bridge firmware for M5Stack boards (board.h). See README.md.

#include "auth.h"
#include "console.h"
#include "control.h"
#include "errors.h"
#include "esp_log.h"
#include "esp_system.h"
#include "activity.h"
#include "firmware.h"
#include "phones.h"
#include "power.h"
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
    if (!auth::load()) {
        // Without its key the device can't be managed over BLE; USB still
        // works, and a clean install makes a new key.
        errors::set(errors::Error::device_key,
                    "the device's key couldn't be loaded or made: it can't be managed over "
                    "Bluetooth. Reinstall the firmware with a clean install");
    }
    ui::start();
    // Whatever fails from here on, the control API still starts, so a
    // management app on USB can see the errors.
    bool bluetooth = radio::start();
    if (bluetooth) {
        power::start();
        phones::start();
    }
    activity::start();

    console::serve(control::handle_line);

    // The self-test: no errors, so settings loaded, the key in hand and
    // Bluetooth up; the UI and the control API running; and phones can find
    // the device. It doesn't wait for the radio, which may be off. A new
    // image that fails it is rolled back at its deadline.
    if (bluetooth && !phones::wait_advertising(10000)) {
        errors::set(errors::Error::phones,
                    "advertising didn't start within 10 s: phones can't find the device. "
                    "Unplug the device and plug it in");
    }
    if (!errors::any()) {
        firmware::self_test_passed();
    } else {
        ESP_LOGE(TAG, "self-test failed");
    }
    ESP_LOGI(TAG, "ready");
}
