// The StickC Plus SE's first screens of their own (issue #28): the Bridge,
// TNC and System pages in landscape, layout B in docs/stickc-mockups, with B
// moving between them. A trial build, to judge on the device: the ATOM's
// UI (ui.cpp) still runs button A and calls led::show(), which only keeps
// the backlight on, and LVGL draws the pages from what the radio and the
// phones report.
//
// Landscape, with USB to the right. Pins and panel settings are the ones
// tried in spikes/stickc-plus-se.

#include <algorithm>
#include <atomic>
#include <cctype>
#include <cstdio>
#include <cstring>
#include <map>
#include <string>
#include <vector>

#include "axp192.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "driver/spi_master.h"
#include "esp_app_desc.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_lcd_panel_st7789.h"
#include "esp_log.h"
#include "esp_mac.h"
#include "esp_lvgl_port.h"
#include "esp_timer.h"
#include "led.h"
#include "phones.h"
#include "radio.h"
#include "settings.h"
#include "uptime.h"

namespace ktb::led {

namespace {

constexpr const char *TAG = "screen";

constexpr gpio_num_t pin_sda = GPIO_NUM_21;
constexpr gpio_num_t pin_scl = GPIO_NUM_22;
constexpr gpio_num_t pin_mosi = GPIO_NUM_15;
constexpr gpio_num_t pin_sclk = GPIO_NUM_13;
constexpr gpio_num_t pin_dc = GPIO_NUM_23;
constexpr gpio_num_t pin_rst = GPIO_NUM_18;
constexpr gpio_num_t pin_cs = GPIO_NUM_5;
constexpr gpio_num_t pin_red_led = GPIO_NUM_10; // active low
constexpr gpio_num_t pin_b = GPIO_NUM_39;       // button B, active low, pulled up on the board
constexpr spi_host_device_t lcd_host = SPI2_HOST;

// Landscape. The panel is 135x240 in the ST7789's 240x320 memory, at
// column 52 and row 40. Swapped and mirrored this way (tried: the other
// mirror turns it upside down), the visible part starts 40 across and
// 240 - 52 - 135 = 53 down (52 left a line of undrawn pixels at the bottom).
constexpr int W = 240;
constexpr int H = 135;
constexpr int x_gap = 40;
constexpr int y_gap = 53;
constexpr int strip_rows = 20;

constexpr uint8_t brightness = 80; // fixed for this trial

// ---- the look: the Core2's colors ----

const lv_color_t c_bg = lv_color_hex(0x000000);
const lv_color_t c_fg = lv_color_hex(0xffffff);
const lv_color_t c_dim = lv_color_hex(0x9aa0a6);
const lv_color_t c_faint = lv_color_hex(0x5c6066);
const lv_color_t c_line = lv_color_hex(0x26292d);
const lv_color_t c_green = lv_color_hex(0x3ddc84);
const lv_color_t c_amber = lv_color_hex(0xffb300);
const lv_color_t c_red = lv_color_hex(0xff5252);
const lv_color_t c_blue = lv_color_hex(0x6aa9ff);
const lv_color_t c_glint = lv_color_hex(0xb8f5d2);

constexpr int bar_h = 20;
constexpr int pad = 6;
constexpr int line_w = W - 2 * pad;
constexpr int64_t light_ms = 300;

bool ready = false;
// The power button's short press turns the screen off and on (B also
// turns it on). The backlight is on once init() has drawn the screen;
// from then on tick() keeps it as screen_on says.
std::atomic<bool> lit{false};
std::atomic<bool> screen_on{true};

// The start-up screen until main() has started everything, as the
// Core2's: the step under way shows once it's taken this long.
constexpr int64_t slow_step_ms = 2000;
std::atomic<const char *> step{nullptr};
std::atomic<int64_t> step_since{0};
std::atomic<bool> is_started{false};
lv_display_t *display = nullptr;
lv_obj_t *main_scr = nullptr;
lv_obj_t *splash_scr = nullptr;
lv_obj_t *splash_status = nullptr;

lv_obj_t *box(lv_obj_t *parent, int x, int y, int w, int h)
{
    lv_obj_t *o = lv_obj_create(parent);
    lv_obj_remove_style_all(o);
    lv_obj_set_pos(o, x, y);
    lv_obj_set_size(o, w, h);
    lv_obj_set_scrollable(o, false);
    return o;
}

void fill(lv_obj_t *o, lv_color_t c, int radius = 0)
{
    lv_obj_set_style_bg_color(o, c, 0);
    lv_obj_set_style_bg_opa(o, LV_OPA_COVER, 0);
    lv_obj_set_style_radius(o, radius, 0);
}

lv_obj_t *label(lv_obj_t *parent, const lv_font_t *font, lv_color_t color, int x, int y, int w = 0)
{
    lv_obj_t *l = lv_label_create(parent);
    lv_obj_set_style_text_font(l, font, 0);
    lv_obj_set_style_text_color(l, color, 0);
    lv_label_set_text(l, "");
    lv_obj_set_pos(l, x, y);
    if (w > 0) {
        // One line, cut with dots: a label with only a width wraps.
        lv_obj_set_size(l, w, lv_font_get_line_height(font));
        lv_label_set_long_mode(l, LV_LABEL_LONG_MODE_DOTS);
    }
    return l;
}

void set_text(lv_obj_t *l, const std::string &s)
{
    if (strcmp(lv_label_get_text(l), s.c_str()) != 0) {
        lv_label_set_text(l, s.c_str());
    }
}

void show(lv_obj_t *o, bool on) { lv_obj_set_hidden(o, !on); }

std::string duration(uint32_t s)
{
    char b[24];
    if (s < 60) {
        snprintf(b, sizeof(b), "%lus", (unsigned long)s);
    } else if (s < 3600) {
        snprintf(b, sizeof(b), "%lum", (unsigned long)(s / 60));
    } else if (s < 86400) {
        snprintf(b, sizeof(b), "%luh %lum", (unsigned long)(s / 3600), (unsigned long)(s % 3600 / 60));
    } else {
        snprintf(b, sizeof(b), "%lud %luh", (unsigned long)(s / 86400), (unsigned long)(s % 86400 / 3600));
    }
    return b;
}

int64_t now_ms() { return esp_timer_get_time() / 1000; }

// ---- the pages ----

constexpr int page_count = 3;
const char *const page_titles[page_count] = {"Bridge", "TNC", "System"};
int page = 0;
lv_obj_t *pages[page_count];
lv_obj_t *marker, *title;

struct {
    lv_obj_t *batt_pct, *batt_level, *batt_glint;
    lv_obj_t *radio_name, *radio_state, *radio_time;
    lv_obj_t *rx_row, *rx_light, *tx_row, *tx_light;
    lv_obj_t *ble_name, *apps_note;
    lv_obj_t *app_addr[2], *app_time[2], *app_rx[2], *app_tx[2];
} pg;

struct {
    lv_obj_t *name, *model, *address, *state, *time, *hint;
} tn;

struct {
    lv_obj_t *uptime, *last_run, *firmware;
} sy;

struct Seen {
    uint32_t rx = 0, tx = 0;
    int64_t rx_at = -10000, tx_at = -10000;
} seen;

std::map<std::string, Seen> app_seen; // by the app's address

int64_t power_read_at = -10000;

// ---- addresses ----

// Addresses in capitals, whatever case they come in.
std::string upper(std::string s)
{
    for (char &c : s) {
        c = static_cast<char>(toupper(static_cast<unsigned char>(c)));
    }
    return s;
}

// The widest a Bluetooth address can be in a font: twelve of its widest
// hex digit and five colons. Montserrat's digits differ in width, so the
// app rows' times start past this, and line up whatever the addresses.
int address_width(const lv_font_t *f)
{
    int digit = 0;
    for (char c : std::string("0123456789ABCDEF")) {
        digit = std::max(digit, static_cast<int>(lv_font_get_glyph_width(f, c, 0)));
    }
    return 12 * digit + 5 * lv_font_get_glyph_width(f, ':', 0);
}

lv_obj_t *light(lv_obj_t *parent, int x, int y, int size)
{
    lv_obj_t *o = box(parent, x, y, size, size);
    lv_obj_set_style_radius(o, 2, 0);
    lv_obj_set_style_border_width(o, 1, 0);
    lv_obj_set_style_border_color(o, c_faint, 0);
    return o;
}

void set_light(lv_obj_t *o, bool on, lv_color_t c)
{
    lv_obj_set_style_bg_color(o, c, 0);
    lv_obj_set_style_bg_opa(o, on ? LV_OPA_COVER : LV_OPA_TRANSP, 0);
    lv_obj_set_style_border_color(o, on ? c : c_faint, 0);
}

// The top bar: the page marker by B (top left), the title, the battery.
void build_chrome(lv_obj_t *scr)
{
    marker = label(scr, &lv_font_montserrat_12, c_dim, 5, 3);
    lv_label_set_text(marker, "1/2");
    lv_obj_update_layout(marker);
    title = label(scr, &lv_font_montserrat_12, c_fg, 5 + lv_obj_get_width(marker) + 6, 3);
    pg.batt_pct = label(scr, &lv_font_montserrat_12, c_dim, 0, 3);
    lv_obj_align(pg.batt_pct, LV_ALIGN_TOP_RIGHT, -28, 3);
    lv_obj_t *outline = box(scr, W - 24, 6, 18, 9);
    lv_obj_set_style_border_width(outline, 1, 0);
    lv_obj_set_style_border_color(outline, c_dim, 0);
    lv_obj_set_style_radius(outline, 2, 0);
    lv_obj_t *nub = box(scr, W - 6, 9, 2, 3);
    fill(nub, c_dim);
    pg.batt_level = box(outline, 1, 1, 0, 5);
    fill(pg.batt_level, c_dim, 1);
    // While charging, a light stripe moves through the level, as on the
    // Core2 (docs/core2-ui.md, The battery).
    pg.batt_glint = box(pg.batt_level, 0, 0, 2, 5);
    fill(pg.batt_glint, c_glint);
    show(pg.batt_glint, false);
    lv_obj_t *rule = box(scr, 0, bar_h, W, 1);
    fill(rule, c_line);
}

// RX and TX, each a label with its light after it, at the right of the
// radio's two lines. Right-aligned, so the lights line up whatever the
// labels' widths. The light is as tall as the capitals, and level with
// them.
const lv_font_t *const lights_font = &lv_font_montserrat_20;
constexpr int lights_box = 60; // wider than either row
int lights_w = 0;              // the wider row's width, once built

lv_obj_t *light_row(lv_obj_t *p, const char *name, int y, lv_obj_t **li)
{
    lv_font_glyph_dsc_t g;
    lv_font_get_glyph_dsc(lights_font, &g, 'X', 0);
    int cap = g.box_h;
    int cap_top = lights_font->line_height - lights_font->base_line - (g.ofs_y + g.box_h);
    lv_obj_t *row = box(p, W - pad - lights_box, y, lights_box, lights_font->line_height);
    *li = light(row, lights_box - cap, cap_top, cap);
    lv_obj_t *l = label(row, lights_font, c_fg, 0, 0);
    lv_label_set_text(l, name);
    lv_obj_update_layout(l);
    int text_w = lv_obj_get_width(l);
    lv_obj_set_x(l, lights_box - cap - 5 - text_w);
    lights_w = std::max(lights_w, text_w + 5 + cap);
    return row;
}

void build_bridge(lv_obj_t *p)
{
    // Top: the radio and its state and how long, with RX and TX at the
    // right while it's connected.
    pg.radio_name = label(p, &lv_font_montserrat_20, c_fg, pad, 23, line_w);
    pg.radio_state = label(p, &lv_font_montserrat_14, c_green, pad, 46);
    pg.radio_time = label(p, &lv_font_montserrat_12, c_dim, pad, 48);
    pg.rx_row = light_row(p, "RX", 22, &pg.rx_light);
    pg.tx_row = light_row(p, "TX", 44, &pg.tx_light);

    lv_obj_t *divider = box(p, 0, 69, W, 1);
    fill(divider, c_line);

    // Bottom: the name KISS TNC apps see, then each app and how long it's
    // been connected.
    pg.ble_name = label(p, &lv_font_montserrat_20, c_fg, pad, 72, line_w);
    pg.apps_note = label(p, &lv_font_montserrat_12, c_dim, pad, 96, line_w);
    // Each app: its address, how long it's been connected, then a light
    // for frames from it (green) and one for frames to it (amber), as tall
    // as the capitals, at the right.
    const lv_font_t *f = &lv_font_montserrat_12;
    lv_font_glyph_dsc_t g;
    lv_font_get_glyph_dsc(f, &g, 'X', 0);
    int cap = g.box_h;
    int cap_top = f->line_height - f->base_line - (g.ofs_y + g.box_h);
    for (int i = 0; i < 2; i++) {
        int y = 96 + i * 16;
        pg.app_addr[i] = label(p, f, c_fg, pad, y);
        pg.app_time[i] = label(p, f, c_dim, pad + address_width(f) + 8, y);
        pg.app_tx[i] = light(p, W - pad - cap, y + cap_top, cap);
        pg.app_rx[i] = light(p, W - pad - 2 * cap - 4, y + cap_top, cap);
    }
}

// The radio the device has, then how to change it.
void build_tnc(lv_obj_t *p)
{
    tn.name = label(p, &lv_font_montserrat_20, c_fg, pad, 23, line_w);
    tn.model = label(p, &lv_font_montserrat_12, c_dim, pad, 46, line_w);
    tn.address = label(p, &lv_font_montserrat_12, c_dim, pad, 61);
    tn.state = label(p, &lv_font_montserrat_14, c_green, pad, 78);
    tn.time = label(p, &lv_font_montserrat_12, c_dim, pad, 80);

    lv_obj_t *divider = box(p, 0, 98, W, 1);
    fill(divider, c_line);

    // One line, in the middle of the space under the rule.
    tn.hint = label(p, &lv_font_montserrat_12, c_fg, pad, 109, line_w);
}

// Only what's worth seeing on the device: a management app reads the
// rest. Under the rows, room for the screen's timeout (next round).
lv_obj_t *system_row(lv_obj_t *p, const char *name, int y)
{
    lv_obj_t *k = label(p, &lv_font_montserrat_14, c_dim, pad, y);
    lv_label_set_text(k, name);
    lv_obj_t *v = label(p, &lv_font_montserrat_14, c_fg, 0, y);
    lv_obj_align(v, LV_ALIGN_TOP_RIGHT, -pad, y);
    return v;
}

void build_system(lv_obj_t *p)
{
    sy.uptime = system_row(p, "Uptime", 27);
    sy.last_run = system_row(p, "Last run", 47);
    sy.firmware = system_row(p, "Firmware", 67);
    set_text(sy.firmware, esp_app_get_description()->version);

    // The bridge's own Bluetooth address.
    lv_obj_t *a = system_row(p, "Bluetooth", 87);
    uint8_t mac[6] = {};
    esp_read_mac(mac, ESP_MAC_BT);
    char b[18];
    snprintf(b, sizeof(b), "%02X:%02X:%02X:%02X:%02X:%02X", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    set_text(a, b);
}

void update_system(int64_t now)
{
    set_text(sy.uptime, duration(static_cast<uint32_t>(now / 1000)));
    int64_t last = uptime::last_seconds();
    set_text(sy.last_run, last >= 0 ? duration(static_cast<uint32_t>(last)) : "None yet");
}

void show_page(int i)
{
    page = i;
    for (int j = 0; j < page_count; j++) {
        show(pages[j], j == i);
    }
    char b[24];
    snprintf(b, sizeof(b), "%d/%d", i + 1, page_count);
    set_text(marker, b);
    set_text(title, page_titles[i]);
}

// ---- the start-up screen ----

void tick(lv_timer_t *);

void splash_tick(lv_timer_t *t)
{
    if (is_started) {
        tick(nullptr); // the pages filled in before they show
        lv_screen_load(main_scr);
        lv_timer_delete(t);
        return;
    }
    const char *s = step;
    bool slow = s && now_ms() - step_since > slow_step_ms;
    set_text(splash_status, slow ? std::string(s) + "..." : "Starting...");
}

// The Core2's, fitted to 135 px: the name, the firmware, a spinner and
// the step.
void build_splash()
{
    splash_scr = lv_obj_create(nullptr);
    fill(splash_scr, c_bg);
    lv_obj_set_scrollable(splash_scr, false);
    lv_obj_t *t = label(splash_scr, &lv_font_montserrat_20, c_fg, 0, 0);
    lv_label_set_text(t, "kiss-tnc-bridge");
    lv_obj_align(t, LV_ALIGN_TOP_MID, 0, 14);
    lv_obj_t *v = label(splash_scr, &lv_font_montserrat_14, c_dim, 0, 0);
    lv_label_set_text(v, (std::string("firmware ") + esp_app_get_description()->version).c_str());
    lv_obj_align(v, LV_ALIGN_TOP_MID, 0, 42);
    lv_obj_t *sp = lv_spinner_create(splash_scr);
    lv_obj_set_size(sp, 30, 30);
    lv_obj_align(sp, LV_ALIGN_TOP_MID, 0, 66);
    lv_obj_set_style_arc_width(sp, 4, LV_PART_MAIN);
    lv_obj_set_style_arc_color(sp, c_line, LV_PART_MAIN);
    lv_obj_set_style_arc_width(sp, 4, LV_PART_INDICATOR);
    lv_obj_set_style_arc_color(sp, c_blue, LV_PART_INDICATOR);
    splash_status = label(splash_scr, &lv_font_montserrat_12, c_dim, 0, 0);
    lv_label_set_text(splash_status, "Starting...");
    lv_obj_align(splash_status, LV_ALIGN_TOP_MID, 0, 104);
}

// ---- the pages ----

void build(lv_obj_t *scr)
{
    fill(scr, c_bg);
    lv_obj_set_scrollable(scr, false);
    // Each page is the whole screen under the top bar's objects, which are
    // made after them so they draw on top.
    for (int i = 0; i < page_count; i++) {
        pages[i] = box(scr, 0, 0, W, H);
    }
    build_bridge(pages[0]);
    build_tnc(pages[1]);
    build_system(pages[2]);
    build_chrome(scr);
    show_page(0);
}

void update_battery()
{
    axp192::Power p = axp192::read();
    char b[8];
    snprintf(b, sizeof(b), "%d%%", p.percent);
    set_text(pg.batt_pct, p.battery ? b : "");
    int w = p.battery ? (14 * p.percent + 50) / 100 : 0;
    if (p.battery && w < 1) {
        w = 1;
    }
    lv_obj_set_width(pg.batt_level, w);
    lv_obj_set_style_bg_color(pg.batt_level, p.external ? c_green : p.percent <= 20 ? c_red : c_dim, 0);
    show(pg.batt_glint, p.charging && w > 3);
}

// The radio's state and the time beside it, in the Core2's words.
void radio_state(const radio::View &v, std::string &text, lv_color_t &color, std::string &time)
{
    time.clear();
    switch (v.state) {
    case radio::State::none:
        text = "Press A twice";
        color = c_dim;
        time = "to pair a radio";
        break;
    case radio::State::idle:
        if (radio::pairing()) {
            text = "Pairing";
            color = c_blue;
        } else {
            text = "Stopped";
            color = c_dim;
        }
        break;
    case radio::State::connecting:
        text = v.ble ? "Waiting" : "Connecting";
        color = c_amber;
        time = "for " + duration(v.down_s);
        break;
    case radio::State::connected:
        text = "Connected";
        color = c_green;
        time = "for " + duration(v.state_s);
        break;
    case radio::State::retrying: {
        char b[24];
        snprintf(b, sizeof(b), "Retrying in %lus", (unsigned long)v.retry_in_s);
        text = b;
        color = c_amber;
        time = "for " + duration(v.down_s);
        break;
    }
    }
}

// A state label with its time just after it, on the same line.
void set_state(lv_obj_t *state, lv_obj_t *time, const radio::View &v)
{
    std::string text, t;
    lv_color_t color;
    radio_state(v, text, color, t);
    set_text(state, text);
    lv_obj_set_style_text_color(state, color, 0);
    set_text(time, t);
    lv_obj_update_layout(state);
    lv_obj_set_x(time, pad + lv_obj_get_width(state) + 6);
}

void update_tnc(const radio::View &v)
{
    set_text(tn.name, v.present ? v.name : "No radio");
    lv_obj_set_style_text_color(tn.name, v.present ? c_fg : c_dim, 0);
    std::string model = v.make.empty() ? (v.ble ? "BLE KISS TNC" : v.name) : v.make;
    set_text(tn.model, !v.present ? "Nothing paired or selected"
                                  : model + (v.ble ? ", selected" : v.paired ? ", paired" : ", not paired"));
    set_text(tn.address, upper(v.address));
    show(tn.state, v.present);
    show(tn.time, v.present);
    if (v.present) {
        set_state(tn.state, tn.time, v);
    }
    set_text(tn.hint, v.present ? "Press A twice to choose another TNC." : "Press A twice to choose a TNC.");
}

void tick(lv_timer_t *)
{
    int64_t now = now_ms();
    if (axp192::short_presses() > 0) {
        screen_on = !screen_on;
    }
    if (lit) {
        axp192::backlight(screen_on ? brightness : 0);
    }
    if (now - power_read_at >= 1000) {
        power_read_at = now;
        update_battery();
    }
    // Settings, the radio and the phones aren't read until main() has
    // started them: the start-up screen shows till then.
    if (!is_started) {
        return;
    }

    radio::View v = radio::view();
    update_tnc(v);
    update_system(now);
    set_text(pg.radio_name, v.present ? v.name : "No radio");
    lv_obj_set_style_text_color(pg.radio_name, v.present ? c_fg : c_dim, 0);
    set_state(pg.radio_state, pg.radio_time, v);

    bool connected = v.state == radio::State::connected;
    show(pg.rx_row, connected);
    show(pg.tx_row, connected);
    // The name gives way to RX and TX while they show.
    lv_obj_set_width(pg.radio_name, connected ? line_w - lights_w - 8 : line_w);
    if (connected) {
        if (v.rx != seen.rx) {
            seen.rx_at = now;
        }
        if (v.tx != seen.tx) {
            seen.tx_at = now;
        }
        seen.rx = v.rx;
        seen.tx = v.tx;
        set_light(pg.rx_light, now - seen.rx_at < light_ms, c_green);
        set_light(pg.tx_light, now - seen.tx_at < light_ms, c_amber);
    } else {
        seen = Seen{};
    }

    std::vector<phones::SessionView> apps = phones::session_views();
    set_text(pg.ble_name, settings::public_name());
    int n = static_cast<int>(apps.size());
    char note[32];
    snprintf(note, sizeof(note), "No apps connected (of %d)", phones::max_sessions());
    set_text(pg.apps_note, note);
    show(pg.apps_note, n == 0);
    std::map<std::string, Seen> next;
    for (int i = 0; i < 2; i++) {
        bool on = i < n;
        show(pg.app_addr[i], on);
        show(pg.app_time[i], on);
        show(pg.app_rx[i], on);
        show(pg.app_tx[i], on);
        if (!on) {
            continue;
        }
        const auto &a = apps[i];
        set_text(pg.app_addr[i], upper(a.address));
        set_text(pg.app_time[i], duration(a.connected_s));
        auto it = app_seen.find(a.address);
        Seen s = it != app_seen.end() ? it->second : Seen{};
        if (a.rx != s.rx) {
            s.rx_at = now;
        }
        if (a.tx != s.tx) {
            s.tx_at = now;
        }
        s.rx = a.rx;
        s.tx = a.tx;
        next[a.address] = s;
        set_light(pg.app_rx[i], now - s.rx_at < light_ms, c_green);
        set_light(pg.app_tx[i], now - s.tx_at < light_ms, c_amber);
    }
    app_seen = next;
}

// The charging stripe, a pixel further each step, round again at the end.
void move_glint(lv_timer_t *)
{
    static int x = 0;
    if (lv_obj_is_hidden(pg.batt_glint)) {
        return;
    }
    int w = lv_obj_get_width(pg.batt_level);
    x = (x + 1) % (w > 2 ? w : 3);
    lv_obj_set_x(pg.batt_glint, x - 1);
}

// Button B, read every 20 ms: a press counts once it's been down for two
// reads, so a glitch on GPIO39 (an input-only pin with a known erratum)
// isn't taken for one. It moves to the next page, or with the screen off
// turns it on at the page it was on.
void read_b(lv_timer_t *)
{
    static int down = 0;
    if (!is_started) {
        return; // the start-up screen has no pages
    }
    if (gpio_get_level(pin_b) == 0) {
        if (++down == 2) {
            if (!screen_on) {
                screen_on = true;
                axp192::backlight(brightness);
            } else {
                show_page((page + 1) % page_count);
            }
        }
    } else {
        down = 0;
    }
}

// ---- the hardware ----

void start_b()
{
    gpio_config_t cfg = {};
    cfg.pin_bit_mask = 1ULL << pin_b;
    cfg.mode = GPIO_MODE_INPUT;
    gpio_config(&cfg);
}

void red_led_off()
{
    gpio_config_t cfg = {};
    cfg.pin_bit_mask = 1ULL << pin_red_led;
    cfg.mode = GPIO_MODE_OUTPUT;
    gpio_config(&cfg);
    gpio_set_level(pin_red_led, 1);
}

bool start_power()
{
    i2c_master_bus_config_t cfg = {};
    cfg.i2c_port = I2C_NUM_0;
    cfg.sda_io_num = pin_sda;
    cfg.scl_io_num = pin_scl;
    cfg.clk_source = I2C_CLK_SRC_DEFAULT;
    cfg.glitch_ignore_cnt = 7;
    cfg.flags.enable_internal_pullup = true;
    i2c_master_bus_handle_t bus = nullptr;
    if (i2c_new_master_bus(&cfg, &bus) != ESP_OK) {
        ESP_LOGE(TAG, "I2C bus didn't start");
        return false;
    }
    return axp192::init(bus);
}

bool start_screen()
{
    spi_bus_config_t bus = {};
    bus.mosi_io_num = pin_mosi;
    bus.miso_io_num = -1;
    bus.sclk_io_num = pin_sclk;
    bus.quadwp_io_num = -1;
    bus.quadhd_io_num = -1;
    bus.max_transfer_sz = W * strip_rows * 2;
    if (spi_bus_initialize(lcd_host, &bus, SPI_DMA_CH_AUTO) != ESP_OK) {
        ESP_LOGE(TAG, "SPI bus didn't start");
        return false;
    }
    esp_lcd_panel_io_spi_config_t io_cfg = {};
    io_cfg.dc_gpio_num = pin_dc;
    io_cfg.cs_gpio_num = pin_cs;
    io_cfg.pclk_hz = 40 * 1000 * 1000;
    io_cfg.lcd_cmd_bits = 8;
    io_cfg.lcd_param_bits = 8;
    io_cfg.spi_mode = 0;
    io_cfg.trans_queue_depth = 4; // esp_lvgl_port sets on_color_trans_done
    esp_lcd_panel_io_handle_t io = nullptr;
    if (esp_lcd_new_panel_io_spi(static_cast<esp_lcd_spi_bus_handle_t>(lcd_host), &io_cfg, &io) != ESP_OK) {
        ESP_LOGE(TAG, "screen IO didn't start");
        return false;
    }
    esp_lcd_panel_dev_config_t panel_cfg = {};
    panel_cfg.reset_gpio_num = pin_rst;
    panel_cfg.rgb_ele_order = LCD_RGB_ELEMENT_ORDER_RGB;
    panel_cfg.bits_per_pixel = 16;
    esp_lcd_panel_handle_t panel = nullptr;
    if (esp_lcd_new_panel_st7789(io, &panel_cfg, &panel) != ESP_OK) {
        ESP_LOGE(TAG, "screen driver didn't start");
        return false;
    }
    esp_lcd_panel_reset(panel);
    esp_lcd_panel_init(panel);
    esp_lcd_panel_invert_color(panel, true);
    esp_lcd_panel_set_gap(panel, x_gap, y_gap);
    esp_lcd_panel_disp_on_off(panel, true);

    lvgl_port_cfg_t port = ESP_LVGL_PORT_INIT_CONFIG();
    port.task_affinity = 1; // away from the Bluetooth controller on core 0
    if (lvgl_port_init(&port) != ESP_OK) {
        ESP_LOGE(TAG, "LVGL didn't start");
        return false;
    }
    lvgl_port_display_cfg_t d = {};
    d.io_handle = io;
    d.panel_handle = panel;
    d.buffer_size = W * strip_rows;
    d.double_buffer = true;
    d.hres = W;
    d.vres = H;
    d.color_format = LV_COLOR_FORMAT_RGB565;
    d.rotation.swap_xy = true;
    d.rotation.mirror_x = true;
    d.rotation.mirror_y = false;
    d.flags.buff_dma = 1;
    d.flags.swap_bytes = 1;
    lv_display_t *disp = lvgl_port_add_disp(&d);
    if (!disp) {
        ESP_LOGE(TAG, "LVGL display didn't start");
        return false;
    }
    display = disp;
    lvgl_port_lock(0);
    main_scr = lv_display_get_screen_active(disp);
    build(main_scr);
    build_splash();
    lv_screen_load(splash_scr);
    lv_timer_create(splash_tick, 100, nullptr);
    tick(nullptr);
    lv_timer_create(tick, 250, nullptr);
    start_b();
    lv_timer_create(read_b, 20, nullptr);
    lv_timer_create(move_glint, 150, nullptr);
    lvgl_port_unlock();
    return true;
}

} // namespace

void init()
{
    if (ready) {
        return;
    }
    red_led_off();
    if (!start_power() || !start_screen()) {
        ESP_LOGE(TAG, "the screen didn't start: the device runs without it");
        return;
    }
    // Drawn before the backlight comes on, so nothing half-drawn shows.
    lvgl_port_lock(0);
    lv_refr_now(display);
    lvgl_port_unlock();
    lit = true;
    axp192::backlight(brightness);
    ready = true;
    ESP_LOGI(TAG, "ready");
}

void starting(const char *s)
{
    step_since = esp_timer_get_time() / 1000;
    step = s;
}

void started() { is_started = true; }

// The ATOM's LED states and events aren't shown in this trial: the pages
// are, lit from init().
void show(Rgb, uint8_t, float) {}

} // namespace ktb::led
