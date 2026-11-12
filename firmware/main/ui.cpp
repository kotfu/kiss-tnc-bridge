#include "ui.h"

#include <atomic>
#include <cmath>
#include <deque>

#include "auth.h"
#include "board.h"
#include "errors.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "led.h"
#include "radio.h"
#include "settings.h"

namespace ktb::ui {

namespace {

constexpr const char *TAG = "ui";
using led::Rgb;

// Timing, all provisional until judged by eye on a device
// (docs/atom-lite-ui.md, Timing). Milliseconds.
constexpr uint32_t tick_ms = 20;
constexpr uint32_t fast_ms = 250;         // fast blink: on, then off
constexpr uint32_t slow_ms = 800;         // slow blink: on, then off
constexpr uint32_t flash_ms = 250;        // the three quick flashes
constexpr uint32_t gap_ms = 4 * fast_ms;  // the dark gap around an event
constexpr uint32_t breathe_ms = 8000;     // one breath
constexpr float breathe_low = 0.15f;      // the bottom of a breath
// led_normal dim: the normal states at half their brightness to the eye.
// led_brightness is already perceptual (squared), so this is the same as
// halving led_brightness for them.
constexpr float dim_level = 0.25f;
constexpr uint32_t dip_ms = 100;           // frame to a phone
constexpr float dip_level = 0.15f;
constexpr uint32_t flick_ms = 100;         // frame to the radio
constexpr uint32_t long_press_ms = 3000;
constexpr uint32_t double_press_ms = 400; // release to the next press
constexpr uint32_t reset_warning_ms = 3000;

// The reset at power-on happens before settings load, so it can't depend
// on them: it shows at this brightness whatever led_brightness says.
constexpr uint8_t boot_brightness = 50;

struct Step {
    Rgb color;
    uint32_t ms;
};

// The longest sequence: identify, 11 steps.
constexpr size_t max_steps = 16;
struct Sequence {
    Step steps[max_steps];
    size_t count = 0;
    void add(Rgb c, uint32_t ms) { steps[count++] = {c, ms}; }
};

Sequence sequence_for(Event e)
{
    Sequence s;
    // A confirmation: dark gaps on both sides set it apart from the state
    // before it (often a blink, as when pairing is cancelled) and after it.
    auto three_flashes = [&s](Rgb c) {
        s.add(led::off, gap_ms);
        for (int i = 0; i < 3; i++) {
            s.add(c, flash_ms);
            s.add(led::off, flash_ms);
        }
        s.add(led::off, gap_ms);
    };
    switch (e) {
    case Event::power_on:
        // One color straight after another, then dark.
        for (Rgb c : {led::red, led::yellow, led::cyan, led::green}) {
            s.add(c, slow_ms);
        }
        s.add(led::off, gap_ms);
        break;
    case Event::pairing_succeeded:
    case Event::app_authorized:
        three_flashes(led::green);
        break;
    case Event::pairing_failed:
        three_flashes(led::yellow);
        break;
    case Event::reset_done:
        three_flashes(led::white);
        break;
    case Event::identify:
        // The dark gap, the colors straight after one another, the gap.
        s.add(led::off, gap_ms);
        for (int i = 0; i < 3; i++) {
            for (Rgb c : {led::magenta, led::blue, led::white}) {
                s.add(c, slow_ms);
            }
        }
        s.add(led::off, gap_ms);
        break;
    default:
        break;
    }
    return s;
}

const char *event_name(Event e)
{
    switch (e) {
    case Event::power_on: return "power on";
    case Event::pairing_succeeded: return "pairing succeeded";
    case Event::pairing_failed: return "pairing failed";
    case Event::app_authorized: return "app authorized";
    case Event::identify: return "identify";
    case Event::reset_done: return "reset done";
    case Event::frame_to_phone: return "frame to phone";
    case Event::frame_to_radio: return "frame to radio";
    }
    return "?";
}

// States, highest precedence first (docs/atom-lite-ui.md, States)
enum class Shown : uint8_t {
    error,
    updating,
    authorizing,
    pairing,
    no_radio,
    radio_down,
    normal_idle,   // radio connected, no phones: cyan
    normal_phones, // radio connected, phones connected: green
};

bool is_normal(Shown s) { return s == Shown::normal_idle || s == Shown::normal_phones; }

const char *shown_name(Shown s)
{
    switch (s) {
    case Shown::error: return "error";
    case Shown::updating: return "updating";
    case Shown::authorizing: return "authorization mode";
    case Shown::pairing: return "pairing";
    case Shown::no_radio: return "no radio";
    case Shown::radio_down: return "radio not connected";
    case Shown::normal_idle: return "radio connected, no phones";
    case Shown::normal_phones: return "radio connected, phones connected";
    }
    return "?";
}

std::atomic<bool> updating_on{false};

Shown current_state()
{
    if (errors::any()) return Shown::error;
    if (updating_on) return Shown::updating;
    if (auth::mode_is_on()) return Shown::authorizing;
    if (radio::pairing()) return Shown::pairing;
    switch (radio::link()) {
    case radio::Link::none: return Shown::no_radio;
    case radio::Link::down: return Shown::radio_down;
    case radio::Link::connected:
        return radio::phones() > 0 ? Shown::normal_phones : Shown::normal_idle;
    }
    return Shown::no_radio;
}

QueueHandle_t queue;
std::atomic<uint32_t> posted{0};   // sequences posted
std::atomic<uint32_t> finished{0}; // sequences shown
bool led_ready = false;
bool power_on_shown = false;

// A board without a button (board.h) never sees a press.
constexpr bool has_button = board::button != GPIO_NUM_NC;

bool button_down()
{
    if constexpr (!has_button) {
        return false;
    }
    return gpio_get_level(board::button) == 0;
}

void button_init()
{
    if constexpr (!has_button) {
        return;
    }
    gpio_config_t cfg = {};
    // Still compiled without a button, so the shift can't use GPIO_NUM_NC.
    cfg.pin_bit_mask = 1ULL << (has_button ? board::button : 0);
    cfg.mode = GPIO_MODE_INPUT;
    gpio_config(&cfg);
}

void play_blocking(const Sequence &s, uint8_t brightness)
{
    for (size_t i = 0; i < s.count; i++) {
        led::show(s.steps[i].color, brightness);
        vTaskDelay(pdMS_TO_TICKS(s.steps[i].ms));
    }
}

int64_t now_ms() { return esp_timer_get_time() / 1000; }

// What a press does depends on what the LED showed when it went down.
enum class Press : uint8_t {
    none,        // the button is up
    ignored,     // updating, or already used (a wake, the second of a double)
    candidate,   // could become a long press, or the first of a double
    cancel_pair, // a press while pairing: cancels it when let go
    end_authorizing, // a press in authorization mode: ends it when let go
};

class Ui {
public:
    void run();

private:
    void take_events(int64_t now);
    void poll_button(int64_t now, Shown shown, bool dark);
    void render(int64_t now, Shown shown, bool dark);

    // Events
    std::deque<Event> pending;
    Sequence playing;
    size_t step = 0;
    int64_t step_end = 0;
    bool is_playing = false;
    int64_t dip_until = 0;
    int64_t flick_until = 0;

    // States
    Shown shown = Shown::no_radio;
    int64_t shown_since = 0;
    int64_t awake_until = 0;

    // The button, debounced over two ticks
    bool raw_last = false;
    bool down = false;
    Press press = Press::none;
    int64_t press_start = 0;
    bool long_reached = false;
    int64_t last_click_up = -10000; // when the last plain click was let go
};

void Ui::take_events(int64_t now)
{
    Event e;
    while (xQueueReceive(queue, &e, 0) == pdTRUE) {
        if (e == Event::frame_to_phone) {
            dip_until = now + dip_ms;
        } else if (e == Event::frame_to_radio) {
            flick_until = now + flick_ms;
        } else {
            ESP_LOGI(TAG, "event: %s", event_name(e));
            pending.push_back(e);
        }
    }
    if (is_playing && now >= step_end) {
        if (++step < playing.count) {
            step_end = now + playing.steps[step].ms;
        } else {
            is_playing = false;
            finished++;
        }
    }
    if (!is_playing && !pending.empty()) {
        playing = sequence_for(pending.front());
        pending.pop_front();
        step = 0;
        step_end = now + playing.steps[0].ms;
        is_playing = true;
    }
}

void Ui::poll_button(int64_t now, Shown state, bool dark)
{
    bool raw = button_down();
    bool stable = raw == raw_last;
    raw_last = raw;
    if (!stable || raw == down) {
        // Still held: has it become a long press?
        if (down && press == Press::candidate && !long_reached &&
            now - press_start >= long_press_ms) {
            long_reached = true;
            ESP_LOGI(TAG, "long press reached: let go to start authorization mode");
        }
        return;
    }
    down = raw;
    uint32_t wake_ms = settings::led_wake_seconds() * 1000u;

    if (down) {
        press_start = now;
        long_reached = false;
        if (state == Shown::updating) {
            press = Press::ignored;
            return;
        }
        // Every press restarts the wake timer.
        bool was_dark = dark;
        awake_until = now + wake_ms;
        if (was_dark) {
            // The wake rule: on a dark LED, a press only wakes it.
            ESP_LOGI(TAG, "press: wake");
            press = Press::ignored;
            last_click_up = -10000;
        } else if (state == Shown::pairing) {
            press = Press::cancel_pair;
        } else if (state == Shown::authorizing) {
            press = Press::end_authorizing;
        } else if (now - last_click_up <= double_press_ms) {
            ESP_LOGI(TAG, "double press: pair");
            press = Press::ignored;
            last_click_up = -10000;
            radio::start_pairing();
        } else {
            press = Press::candidate;
        }
        return;
    }

    // Let go
    switch (press) {
    case Press::candidate:
        if (long_reached) {
            ESP_LOGI(TAG, "long press: start authorization mode");
            auth::mode_start(auth::mode_default_seconds);
            last_click_up = -10000;
        } else {
            last_click_up = now;
        }
        break;
    case Press::cancel_pair:
        ESP_LOGI(TAG, "press: cancel pairing");
        radio::cancel_pairing();
        break;
    case Press::end_authorizing:
        ESP_LOGI(TAG, "press: end authorization mode");
        auth::mode_end();
        break;
    default:
        break;
    }
    press = Press::none;
    long_reached = false;
}

void Ui::render(int64_t now, Shown state, bool dark)
{
    uint8_t brightness = settings::led_brightness();
    if (long_reached) {
        led::show(led::magenta, brightness);
        return;
    }
    if (is_playing) {
        led::show(playing.steps[step].color, brightness);
        return;
    }

    uint32_t t = static_cast<uint32_t>(now - shown_since);
    bool fast_on = t % (2 * fast_ms) < fast_ms;
    bool slow_on = t % (2 * slow_ms) < slow_ms;
    // Three fast blinks, then one more blink's worth of dark: 8 halves of
    // a fast blink, so an error doesn't look like pairing.
    uint32_t half = t / fast_ms % 8;
    bool error_on = half < 6 && half % 2 == 0;
    Rgb color = led::off;
    float level = 1.0f;
    switch (state) {
    case Shown::error: color = error_on ? led::red : led::off; break;
    case Shown::updating: color = fast_on ? led::white : led::off; break;
    case Shown::authorizing: color = slow_on ? led::magenta : led::off; break;
    case Shown::pairing: color = fast_on ? led::blue : led::off; break;
    case Shown::no_radio: color = slow_on ? led::yellow : led::blue; break;
    case Shown::radio_down: color = slow_on ? led::yellow : led::off; break;
    case Shown::normal_idle:
    case Shown::normal_phones:
        color = state == Shown::normal_idle ? led::cyan : led::green;
        if (dark) {
            color = led::off;
        } else if (settings::led_normal() == settings::LedNormal::dim) {
            level = dim_level;
        } else if (settings::led_normal() == settings::LedNormal::breathe) {
            // A breath starts bright.
            float phase = static_cast<float>(t % breathe_ms) / breathe_ms;
            level = breathe_low + (1 - breathe_low) * (0.5f + 0.5f * cosf(2 * M_PI * phase));
        }
        break;
    }

    // Frames flick on top of the state, unless the LED is dark.
    if (!dark) {
        if (now < flick_until) {
            // In dim mode the flick is dimmed too, so it isn't a glare.
            color = led::flick;
            level = settings::led_normal() == settings::LedNormal::dim && is_normal(state)
                        ? dim_level
                        : 1.0f;
        } else if (now < dip_until) {
            level *= dip_level;
        }
    }
    led::show(color, brightness, level);
}

void Ui::run()
{
    shown = current_state();
    shown_since = now_ms();
    // Power-on counts as a wake.
    awake_until = shown_since + settings::led_wake_seconds() * 1000;
    ESP_LOGI(TAG, "state: %s", shown_name(shown));

    TickType_t wake = xTaskGetTickCount();
    for (;;) {
        int64_t now = now_ms();
        take_events(now);

        Shown state = current_state();
        if (state != shown) {
            ESP_LOGI(TAG, "state: %s", shown_name(state));
            if (is_normal(state) && !is_normal(shown)) {
                // Coming back to normal (linked, pairing over,
                // authorization mode ended): show it for a while before going dark.
                awake_until = now + settings::led_wake_seconds() * 1000;
            }
            shown = state;
            shown_since = now;
        }
        bool dark = settings::led_normal() == settings::LedNormal::dark && is_normal(shown) &&
                    now >= awake_until;

        poll_button(now, shown, dark);
        render(now, shown, dark);
        vTaskDelayUntil(&wake, pdMS_TO_TICKS(tick_ms));
    }
}

void ui_task(void *)
{
    static Ui ui;
    ui.run();
}

} // namespace

bool reset_requested_at_boot()
{
    button_init();
    // Held means down for three samples in a row, so a bounce isn't a hold.
    for (int i = 0; i < 3; i++) {
        if (!button_down()) {
            return false;
        }
        vTaskDelay(pdMS_TO_TICKS(10));
    }
    ESP_LOGW(TAG, "button held at power-on");
    led::init();
    led_ready = true;
    play_blocking(sequence_for(Event::power_on), boot_brightness);
    power_on_shown = true;

    ESP_LOGW(TAG, "reset warning: let go to cancel");
    led::show(led::white, boot_brightness);
    int64_t end = now_ms() + reset_warning_ms;
    int up_samples = 0;
    while (now_ms() < end) {
        vTaskDelay(pdMS_TO_TICKS(tick_ms));
        up_samples = button_down() ? 0 : up_samples + 1;
        if (up_samples >= 2) {
            ESP_LOGI(TAG, "let go: reset cancelled");
            led::show(led::off, boot_brightness);
            return false;
        }
    }
    return true;
}

void show_reset_done_blocking()
{
    if (!led_ready) {
        led::init();
        led_ready = true;
    }
    play_blocking(sequence_for(Event::reset_done), boot_brightness);
}

void start()
{
    button_init();
    if (!led_ready) {
        led::init();
        led_ready = true;
    }
    queue = xQueueCreate(16, sizeof(Event));
    if (!power_on_shown) {
        post(Event::power_on);
    }
    xTaskCreate(ui_task, "ui", 4096, nullptr, 6, nullptr);
}

void post(Event e)
{
    if (!queue) {
        return;
    }
    bool sequence = e != Event::frame_to_phone && e != Event::frame_to_radio;
    if (xQueueSend(queue, &e, 0) == pdTRUE && sequence) {
        posted++;
    }
}

void wait_idle(uint32_t timeout_ms)
{
    uint32_t target = posted;
    int64_t end = now_ms() + timeout_ms;
    while (finished < target && now_ms() < end) {
        vTaskDelay(pdMS_TO_TICKS(tick_ms));
    }
}

void set_updating(bool on) { updating_on = on; }

} // namespace ktb::ui
