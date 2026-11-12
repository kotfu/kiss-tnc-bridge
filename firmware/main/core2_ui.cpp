// The Core2's user interface (docs/core2-ui.md): the ui:: functions the
// ATOM's ui.cpp has, on a touch screen. Everything on the screen happens
// in LVGL's task: a timer reads the radio, the phones, the errors and the
// AXP192, and other tasks only queue events.
//
// This first build pairs the way the ATOM does, with the first radio
// found; choosing from a list and comparing the number come next.

#include "ui.h"

#include <algorithm>
#include <cctype>
#include <cstdio>
#include <cstring>
#include <map>
#include <string>
#include <vector>

#include "axp192.h"
#include "cJSON.h"
#include "core2_hw.h"
#include "errors.h"
#include "esp_app_desc.h"
#include "esp_bt_device.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "phones.h"
#include "radio.h"
#include "settings.h"

namespace ktb::ui {

namespace {

constexpr const char *TAG = "ui";

// ---- the look (docs/core2-ui.md, Layout) ----

const lv_color_t c_bg = lv_color_hex(0x000000);
const lv_color_t c_fg = lv_color_hex(0xffffff);
const lv_color_t c_dim = lv_color_hex(0x9aa0a6);
const lv_color_t c_faint = lv_color_hex(0x5c6066);
const lv_color_t c_line = lv_color_hex(0x26292d);
const lv_color_t c_panel = lv_color_hex(0x121417);
const lv_color_t c_green = lv_color_hex(0x3ddc84);
const lv_color_t c_amber = lv_color_hex(0xffb300);
const lv_color_t c_red = lv_color_hex(0xff5252);
const lv_color_t c_blue = lv_color_hex(0x2196f3);
const lv_color_t c_banner = lv_color_hex(0x3a1214);
const lv_color_t c_banner_text = lv_color_hex(0xffb4b4);
const lv_color_t c_glint = lv_color_hex(0xb8f5d2);

constexpr int W = core2::width;
constexpr int H = core2::height;
constexpr int tab_h = 32;
constexpr int tab_w = 80;
constexpr int keys_h = 28;
constexpr int page_y = tab_h + 1;
constexpr int key_centre[3] = {53, 160, 267};

// Timing (provisional, docs/core2-ui.md)
constexpr uint32_t tick_ms = 100;
constexpr int64_t light_ms = 300;
constexpr int64_t slow_step_ms = 2000;
constexpr int64_t reset_warning_ms = 3000;

int64_t now_ms() { return esp_timer_get_time() / 1000; }

lv_obj_t *box(lv_obj_t *parent, int x, int y, int w, int h)
{
    lv_obj_t *o = lv_obj_create(parent);
    lv_obj_remove_style_all(o);
    lv_obj_set_pos(o, x, y);
    lv_obj_set_size(o, w, h);
    lv_obj_set_scrollable(o, false);
    return o;
}

lv_obj_t *label(lv_obj_t *parent, const lv_font_t *font, lv_color_t color, const char *text = "")
{
    lv_obj_t *l = lv_label_create(parent);
    lv_obj_set_style_text_font(l, font, 0);
    lv_obj_set_style_text_color(l, color, 0);
    lv_label_set_text(l, text);
    return l;
}

void fill(lv_obj_t *o, lv_color_t c, int radius = 0)
{
    lv_obj_set_style_bg_color(o, c, 0);
    lv_obj_set_style_bg_opa(o, LV_OPA_COVER, 0);
    lv_obj_set_style_radius(o, radius, 0);
}

void show(lv_obj_t *o, bool on)
{
    if (on) {
        lv_obj_set_hidden(o, false);
    } else {
        lv_obj_set_hidden(o, true);
    }
}

// Sets a label's text only when it changes, so nothing redraws for nothing.
void set_text(lv_obj_t *l, const std::string &s)
{
    if (strcmp(lv_label_get_text(l), s.c_str()) != 0) {
        lv_label_set_text(l, s.c_str());
    }
}

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

// The banner's words for each error (docs/core2-ui.md, Errors).
const char *error_title(errors::Error e)
{
    switch (e) {
    case errors::Error::storage: return "Settings can't be saved";
    case errors::Error::bluetooth: return "Bluetooth didn't start";
    case errors::Error::phones: return "Apps can't find the device";
    case errors::Error::device_key: return "The device has no key";
    case errors::Error::debug: return "A test error (debug.set)";
    }
    return "An error";
}

// ---- state ----

QueueHandle_t queue;
bool hw_ok = false;
volatile bool is_started = false;

lv_obj_t *splash_scr, *splash_status, *main_scr, *message_scr, *message_title, *message_text, *message_bar;
std::string step;
int64_t step_since = 0;

enum class Page : uint8_t { bridge, tnc, system };
enum class Flow : uint8_t { none, pairing, paired, pair_failed, screen_settings, power_off, forget };
Page page = Page::bridge;
Flow flow = Flow::none;
int64_t flow_since = 0;

// The screen (docs/core2-ui.md, The screen)
int64_t last_wake = 0;   // a touch, a press, or an event that wakes it
bool turned_off = false; // the power button turned it off
bool swallowing = false; // a touch that only woke the screen, until it lifts
axp192::Backlight lit = axp192::Backlight::off;

// What wakes it, compared each tick
bool was_connected = false;
bool had_radio = false; // the TNC page's keys depend on it
size_t last_sessions = 0;
size_t last_errors = 0;

// Activity lights: when each count last went up
struct Seen {
    uint32_t rx = 0, tx = 0;
    int64_t rx_at = 0, tx_at = 0;
};
Seen radio_seen;
std::map<std::string, Seen> app_seen;

axp192::Power power;
int64_t power_read_at = -10000;

void wake()
{
    last_wake = now_ms();
    turned_off = false;
}

// ---- tabs, soft keys, battery ----

lv_obj_t *tabs[3], *tab_labels[3];
lv_obj_t *pages[3];
lv_obj_t *flow_box; // a flow's screens, in place of the page

struct Key {
    const char *text = nullptr;
    int style = 0; // 0 outline, 1 main (blue), 2 yes (green), 3 danger (red)
    void (*fn)() = nullptr;
};
lv_obj_t *key_objs[3], *key_labels[3];
Key keys[3];

lv_obj_t *batt_pct, *batt_level, *batt_glint;
int glint_x = 0;

void set_keys(Key l = {}, Key m = {}, Key r = {})
{
    keys[0] = l;
    keys[1] = m;
    keys[2] = r;
    bool any = false;
    for (int i = 0; i < 3; i++) {
        bool on = keys[i].text != nullptr;
        any = any || on;
        show(key_objs[i], on);
        if (!on) {
            continue;
        }
        lv_label_set_text(key_labels[i], keys[i].text);
        lv_color_t c = keys[i].style == 1 ? c_blue : keys[i].style == 2 ? c_green : keys[i].style == 3 ? c_red : c_faint;
        bool filled = keys[i].style != 0;
        lv_obj_set_style_border_color(key_objs[i], c, 0);
        lv_obj_set_style_bg_color(key_objs[i], filled ? c : c_bg, 0);
        lv_obj_set_style_text_color(key_labels[i], keys[i].style == 1 ? c_fg : filled ? c_bg : c_fg, 0);
    }
    // The page gives its bottom to the keys, or takes it back.
    int h = H - page_y - (any ? keys_h : 0);
    for (lv_obj_t *p : pages) {
        lv_obj_set_height(p, h);
    }
    lv_obj_set_height(flow_box, h);
}

void key_clicked(lv_event_t *e)
{
    int i = (int)(intptr_t)lv_event_get_user_data(e);
    if (keys[i].fn) {
        keys[i].fn();
    }
}

void show_page(Page p);

void tab_clicked(lv_event_t *e)
{
    if (flow != Flow::none) {
        return; // locked while a flow runs
    }
    show_page(static_cast<Page>((int)(intptr_t)lv_event_get_user_data(e)));
}

void build_chrome(lv_obj_t *scr)
{
    static const char *names[3] = {"Bridge", "TNC", "System"};
    for (int i = 0; i < 3; i++) {
        tabs[i] = box(scr, i * tab_w, 0, tab_w, tab_h);
        lv_obj_set_clickable(tabs[i], true);
        lv_obj_add_event_cb(tabs[i], tab_clicked, LV_EVENT_CLICKED, (void *)(intptr_t)i);
        lv_obj_set_style_border_side(tabs[i], LV_BORDER_SIDE_BOTTOM, 0);
        tab_labels[i] = label(tabs[i], &lv_font_montserrat_14, c_faint, names[i]);
        lv_obj_center(tab_labels[i]);
    }
    lv_obj_t *rule = box(scr, 0, tab_h, W, 1);
    fill(rule, c_line);

    // The battery: the number, then the battery with its level
    // (docs/core2-ui.md, The battery).
    batt_pct = label(scr, &lv_font_montserrat_12, c_dim, "");
    lv_obj_align(batt_pct, LV_ALIGN_TOP_RIGHT, -38, 9);
    lv_obj_t *outline = box(scr, W - 34, 10, 22, 12);
    lv_obj_set_style_border_width(outline, 2, 0);
    lv_obj_set_style_border_color(outline, c_dim, 0);
    lv_obj_set_style_radius(outline, 2, 0);
    lv_obj_t *nub = box(scr, W - 12, 14, 2, 4);
    fill(nub, c_dim);
    batt_level = box(outline, 0, 0, 0, 8); // inside the 2 px border
    fill(batt_level, c_dim, 1);
    batt_glint = box(batt_level, 0, 0, 3, 8);
    fill(batt_glint, c_glint);
    show(batt_glint, false);

    for (int i = 0; i < 3; i++) {
        key_objs[i] = box(scr, key_centre[i] - 45, H - keys_h + 2, 90, keys_h - 5);
        lv_obj_set_style_border_width(key_objs[i], 2, 0);
        lv_obj_set_style_radius(key_objs[i], 6, 0);
        lv_obj_set_style_bg_opa(key_objs[i], LV_OPA_COVER, 0);
        lv_obj_set_clickable(key_objs[i], true);
        lv_obj_add_event_cb(key_objs[i], key_clicked, LV_EVENT_CLICKED, (void *)(intptr_t)i);
        key_labels[i] = label(key_objs[i], &lv_font_montserrat_12, c_fg, "");
        lv_obj_center(key_labels[i]);
        show(key_objs[i], false);
    }
}

void update_tabs()
{
    for (int i = 0; i < 3; i++) {
        bool on = static_cast<int>(page) == i;
        fill(tabs[i], on ? c_panel : c_bg);
        lv_obj_set_style_border_width(tabs[i], on ? 3 : 0, 0);
        lv_obj_set_style_border_color(tabs[i], flow == Flow::none ? c_blue : c_faint, 0);
        lv_obj_set_style_text_color(tab_labels[i], on ? c_fg : c_faint, 0);
        lv_obj_set_style_opa(tabs[i], flow == Flow::none || on ? LV_OPA_COVER : LV_OPA_40, 0);
    }
}

void update_battery()
{
    const axp192::Power &p = power;
    char b[8];
    snprintf(b, sizeof(b), "%d%%", p.percent);
    set_text(batt_pct, p.battery ? b : "");
    int w = p.battery ? (18 * p.percent + 50) / 100 : 0;
    if (p.battery && w < 1) {
        w = 1;
    }
    lv_obj_set_width(batt_level, w);
    lv_color_t c = p.external ? c_green : p.percent <= 20 ? c_red : c_dim;
    lv_obj_set_style_bg_color(batt_level, c, 0);
    show(batt_glint, p.charging && w > 4);
}

// The charging glint, moved a little each tick.
void move_glint()
{
    if (lv_obj_is_hidden(batt_glint)) {
        return;
    }
    int w = lv_obj_get_width(batt_level);
    glint_x = (glint_x + 1) % (w > 3 ? w : 4);
    lv_obj_set_x(batt_glint, glint_x - 1);
}

// ---- building blocks for pages ----

lv_obj_t *small_caps(lv_obj_t *parent, const char *text)
{
    lv_obj_t *l = label(parent, &lv_font_montserrat_12, c_faint, text);
    lv_obj_set_style_text_letter_space(l, 1, 0);
    return l;
}

lv_obj_t *light(lv_obj_t *parent)
{
    lv_obj_t *o = box(parent, 0, 0, 8, 8);
    lv_obj_set_style_border_width(o, 2, 0);
    lv_obj_set_style_border_color(o, c_faint, 0);
    lv_obj_set_style_radius(o, 2, 0);
    return o;
}

void set_light(lv_obj_t *o, bool on, lv_color_t c)
{
    lv_obj_set_style_border_color(o, on ? c : c_faint, 0);
    lv_obj_set_style_bg_color(o, c, 0);
    lv_obj_set_style_bg_opa(o, on ? LV_OPA_COVER : LV_OPA_TRANSP, 0);
}

void banner_clicked(lv_event_t *) { show_page(Page::system); }

lv_obj_t *banner(lv_obj_t *parent, int y, lv_obj_t **text)
{
    lv_obj_t *b = box(parent, 10, y, W - 20, 24);
    fill(b, c_banner, 4);
    lv_obj_set_clickable(b, true);
    lv_obj_add_event_cb(b, banner_clicked, LV_EVENT_CLICKED, nullptr);
    *text = label(b, &lv_font_montserrat_12, c_banner_text, "");
    lv_obj_align(*text, LV_ALIGN_LEFT_MID, 8, 0);
    lv_obj_t *more = label(b, &lv_font_montserrat_16, c_red, ">");
    lv_obj_align(more, LV_ALIGN_RIGHT_MID, -8, 0);
    show(b, false);
    return b;
}

lv_obj_t *pill(lv_obj_t *parent, lv_obj_t **text)
{
    lv_obj_t *p = lv_obj_create(parent);
    lv_obj_remove_style_all(p);
    lv_obj_set_size(p, LV_SIZE_CONTENT, LV_SIZE_CONTENT);
    lv_obj_set_style_pad_hor(p, 8, 0);
    lv_obj_set_style_pad_ver(p, 3, 0);
    fill(p, c_green, 10);
    *text = label(p, &lv_font_montserrat_12, c_bg, "");
    return p;
}

void set_pill(lv_obj_t *p, lv_obj_t *text, const std::string &s, lv_color_t bg, lv_color_t fg)
{
    set_text(text, s);
    lv_obj_set_style_bg_color(p, bg, 0);
    lv_obj_set_style_text_color(text, fg, 0);
}

// The radio's pill and the time beside it, for the Bridge and TNC pages.
void radio_pill(const radio::View &v, std::string &text, lv_color_t &bg, lv_color_t &fg, std::string &time)
{
    fg = c_bg;
    time.clear();
    switch (v.state) {
    case radio::State::none:
        text = "Not paired";
        bg = c_line;
        fg = c_dim;
        break;
    case radio::State::idle:
        if (radio::pairing()) {
            text = "Pairing";
            bg = c_blue;
            fg = c_fg;
        } else if (v.detail.find("Bluetooth") != std::string::npos) {
            text = "Stopped";
            bg = c_line;
            fg = c_dim;
        } else {
            text = "Held";
            bg = c_line;
            fg = c_dim;
        }
        break;
    case radio::State::connecting:
        text = v.ble ? "Waiting for it" : "Connecting";
        bg = c_amber;
        time = "down " + duration(v.down_s);
        break;
    case radio::State::connected:
        text = "Connected";
        bg = c_green;
        time = duration(v.state_s);
        break;
    case radio::State::retrying: {
        char b[24];
        snprintf(b, sizeof(b), "Retrying in %lu s", (unsigned long)v.retry_in_s);
        text = b;
        bg = c_amber;
        time = "down " + duration(v.down_s);
        break;
    }
    }
}

std::string make_line(const radio::View &v)
{
    std::string make = v.ble ? "TIDRADIO" : v.make.rfind("Kenwood", 0) == 0 ? "Kenwood" : v.make;
    if (make.empty()) {
        return v.address;
    }
    return make + ", " + v.address;
}

// ---- Bridge page ----

struct {
    lv_obj_t *radio_name, *radio_pill, *radio_pill_text, *radio_sub, *radio_time;
    lv_obj_t *counters, *rx_num, *tx_num, *rx_light, *tx_light, *counts_note;
    lv_obj_t *radio_banner, *radio_banner_text;
    lv_obj_t *apps_name, *apps_count, *apps_note, *apps_banner, *apps_banner_text;
    lv_obj_t *app_rows[2], *app_addr[2], *app_meta[2], *app_rx[2], *app_tx[2];
} br;

void build_bridge(lv_obj_t *pg)
{
    lv_obj_set_pos(small_caps(pg, "RADIO"), 10, 4);
    br.radio_name = label(pg, &lv_font_montserrat_20, c_fg);
    lv_obj_set_pos(br.radio_name, 10, 18);
    br.radio_pill = pill(pg, &br.radio_pill_text);
    lv_obj_align(br.radio_pill, LV_ALIGN_TOP_RIGHT, -10, 20);
    br.radio_sub = label(pg, &lv_font_montserrat_12, c_dim);
    lv_obj_set_pos(br.radio_sub, 10, 44);
    br.radio_time = label(pg, &lv_font_montserrat_14, c_fg);
    lv_obj_align(br.radio_time, LV_ALIGN_TOP_RIGHT, -10, 42);

    br.counters = box(pg, 10, 62, W - 20, 32);
    for (int i = 0; i < 2; i++) {
        lv_obj_t *c = box(br.counters, i * 155, 0, 145, 32);
        fill(c, c_panel, 6);
        lv_obj_t *lab = label(c, &lv_font_montserrat_12, c_dim, i == 0 ? "RX" : "TX");
        lv_obj_align(lab, LV_ALIGN_LEFT_MID, 8, 0);
        lv_obj_t *num = label(c, &lv_font_montserrat_20, c_fg, "0");
        lv_obj_align(num, LV_ALIGN_RIGHT_MID, -26, 0);
        lv_obj_t *li = light(c);
        lv_obj_align(li, LV_ALIGN_RIGHT_MID, -8, 0);
        if (i == 0) {
            br.rx_num = num;
            br.rx_light = li;
        } else {
            br.tx_num = num;
            br.tx_light = li;
        }
    }
    br.counts_note = label(pg, &lv_font_montserrat_12, c_dim, "Counts start again when it reconnects.");
    lv_obj_set_pos(br.counts_note, 10, 68);
    br.radio_banner = banner(pg, 66, &br.radio_banner_text);

    lv_obj_t *rule = box(pg, 10, 101, W - 20, 1);
    fill(rule, c_line);
    lv_obj_t *apps_on = small_caps(pg, "APPS ON");
    lv_obj_set_pos(apps_on, 10, 109);
    br.apps_name = label(pg, &lv_font_montserrat_14, c_fg);
    lv_obj_update_layout(apps_on);
    lv_obj_set_pos(br.apps_name, 10 + lv_obj_get_width(apps_on) + 8, 107);
    br.apps_count = label(pg, &lv_font_montserrat_12, c_dim);
    lv_obj_align(br.apps_count, LV_ALIGN_TOP_RIGHT, -10, 109);
    for (int i = 0; i < 2; i++) {
        lv_obj_t *row = box(pg, 10, 127 + i * 26, W - 20, 26);
        br.app_rows[i] = row;
        br.app_addr[i] = label(row, &lv_font_montserrat_14, c_fg);
        lv_obj_align(br.app_addr[i], LV_ALIGN_LEFT_MID, 0, 0);
        br.app_tx[i] = light(row);
        lv_obj_align(br.app_tx[i], LV_ALIGN_RIGHT_MID, 0, 0);
        br.app_rx[i] = light(row);
        lv_obj_align(br.app_rx[i], LV_ALIGN_RIGHT_MID, -12, 0);
        br.app_meta[i] = label(row, &lv_font_montserrat_12, c_dim);
        lv_obj_align(br.app_meta[i], LV_ALIGN_RIGHT_MID, -26, 0);
    }
    br.apps_note = label(pg, &lv_font_montserrat_12, c_dim, "None connected.");
    lv_obj_set_pos(br.apps_note, 10, 130);
    br.apps_banner = banner(pg, 128, &br.apps_banner_text);
}

void update_bridge(const radio::View &v, const std::vector<phones::SessionView> &apps,
                   const std::vector<errors::Item> &errs, int64_t now)
{
    bool bt_error = false, phones_error = false;
    for (auto &e : errs) {
        bt_error = bt_error || e.error == errors::Error::bluetooth;
        phones_error = phones_error || e.error == errors::Error::phones;
    }

    set_text(br.radio_name, v.present ? v.name : "No radio");
    lv_obj_set_style_text_color(br.radio_name, v.present ? c_fg : c_dim, 0);
    std::string pt, time;
    lv_color_t bg, fg;
    radio_pill(v, pt, bg, fg, time);
    set_pill(br.radio_pill, br.radio_pill_text, pt, bg, fg);
    set_text(br.radio_sub, v.present ? make_line(v) : "Pair a Kenwood or select a TD-H9 on the TNC tab.");
    set_text(br.radio_time, time);
    lv_obj_set_style_text_color(br.radio_time, v.state == radio::State::connected ? c_fg : c_dim, 0);

    bool connected = v.state == radio::State::connected;
    show(br.radio_banner, bt_error);
    if (bt_error) {
        set_text(br.radio_banner_text, error_title(errors::Error::bluetooth));
    }
    show(br.counters, connected && !bt_error);
    show(br.counts_note, v.present && !connected && !bt_error);
    if (connected) {
        if (v.rx != radio_seen.rx) {
            radio_seen.rx_at = now;
        }
        if (v.tx != radio_seen.tx) {
            radio_seen.tx_at = now;
        }
        radio_seen.rx = v.rx;
        radio_seen.tx = v.tx;
        set_text(br.rx_num, std::to_string(v.rx));
        set_text(br.tx_num, std::to_string(v.tx));
        set_light(br.rx_light, now - radio_seen.rx_at < light_ms, c_green);
        set_light(br.tx_light, now - radio_seen.tx_at < light_ms, c_amber);
    } else {
        radio_seen = Seen{};
    }

    set_text(br.apps_name, settings::public_name());
    char b[16];
    snprintf(b, sizeof(b), "%d of %d", (int)apps.size(), phones::max_sessions());
    set_text(br.apps_count, b);
    show(br.apps_banner, phones_error);
    if (phones_error) {
        set_text(br.apps_banner_text, error_title(errors::Error::phones));
    }
    show(br.apps_note, apps.empty() && !phones_error);
    std::map<std::string, Seen> next;
    for (int i = 0; i < 2; i++) {
        bool on = i < (int)apps.size();
        show(br.app_rows[i], on);
        if (!on) {
            continue;
        }
        const auto &a = apps[i];
        Seen s = app_seen.count(a.address) ? app_seen[a.address] : Seen{};
        if (a.rx != s.rx) {
            s.rx_at = now;
        }
        if (a.tx != s.tx) {
            s.tx_at = now;
        }
        s.rx = a.rx;
        s.tx = a.tx;
        next[a.address] = s;
        set_text(br.app_addr[i], a.address);
        char m[48];
        snprintf(m, sizeof(m), "%s  RX %lu  TX %lu", duration(a.connected_s).c_str(), (unsigned long)a.rx,
                 (unsigned long)a.tx);
        set_text(br.app_meta[i], m);
        set_light(br.app_rx[i], now - s.rx_at < light_ms, c_green);
        set_light(br.app_tx[i], now - s.tx_at < light_ms, c_amber);
    }
    app_seen = next;
}

// ---- TNC page ----

struct {
    lv_obj_t *name, *pill, *pill_text, *model, *address, *banner, *banner_text;
} tn;

void build_tnc(lv_obj_t *pg)
{
    lv_obj_set_pos(small_caps(pg, "RADIO"), 10, 4);
    tn.name = label(pg, &lv_font_montserrat_20, c_fg);
    lv_obj_set_pos(tn.name, 10, 18);
    tn.pill = pill(pg, &tn.pill_text);
    lv_obj_align(tn.pill, LV_ALIGN_TOP_RIGHT, -10, 20);
    tn.model = label(pg, &lv_font_montserrat_12, c_dim);
    lv_obj_set_pos(tn.model, 10, 46);
    tn.address = label(pg, &lv_font_montserrat_12, c_dim);
    lv_obj_set_pos(tn.address, 10, 62);
    tn.banner = banner(pg, 80, &tn.banner_text);
    lv_obj_t *rule = box(pg, 10, 110, W - 20, 1);
    fill(rule, c_line);
    lv_obj_t *hint = label(pg, &lv_font_montserrat_12, c_dim,
                           "To change it, put the new radio in pairing mode first.\n"
                           "Kenwood: Menu > Bluetooth > Pairing Mode.\n"
                           "TD-H9: Bluetooth on, BT Mode set to Receiver.");
    lv_obj_set_pos(hint, 10, 118);
    lv_obj_set_style_text_line_space(hint, 3, 0);
}

void update_tnc(const radio::View &v, const std::vector<errors::Item> &errs)
{
    set_text(tn.name, v.present ? v.name : "No radio");
    lv_obj_set_style_text_color(tn.name, v.present ? c_fg : c_dim, 0);
    std::string pt, time;
    lv_color_t bg, fg;
    radio_pill(v, pt, bg, fg, time);
    set_pill(tn.pill, tn.pill_text, pt, bg, fg);
    std::string model = v.make.empty() ? v.name : v.make;
    set_text(tn.model, !v.present ? "Pair a Kenwood or select a TD-H9."
                                  : model + (v.ble ? ", selected" : v.paired ? ", paired" : ", not paired"));
    set_text(tn.address, v.address);
    bool bt_error = false;
    for (auto &e : errs) {
        bt_error = bt_error || e.error == errors::Error::bluetooth;
    }
    show(tn.banner, bt_error);
    if (bt_error) {
        set_text(tn.banner_text, error_title(errors::Error::bluetooth));
    }
}

// ---- System page ----

struct {
    lv_obj_t *errs;
    lv_obj_t *rows;
    lv_obj_t *battery, *power, *uptime, *firmware, *name, *bluetooth, *screen;
} sy;

void open_screen_settings();

void build_system(lv_obj_t *pg)
{
    lv_obj_set_scrollable(pg, true);
    lv_obj_set_scrollbar_mode(pg, LV_SCROLLBAR_MODE_AUTO);
    lv_obj_set_flex_flow(pg, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_style_pad_all(pg, 6, 0);
    lv_obj_set_style_pad_left(pg, 10, 0);
    lv_obj_set_style_pad_row(pg, 4, 0);

    sy.errs = lv_obj_create(pg);
    lv_obj_remove_style_all(sy.errs);
    lv_obj_set_size(sy.errs, W - 20, LV_SIZE_CONTENT);
    lv_obj_set_flex_flow(sy.errs, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_style_pad_row(sy.errs, 6, 0);
    show(sy.errs, false);

    auto row = [&](const char *key, bool tappable) {
        lv_obj_t *r = lv_obj_create(pg);
        lv_obj_remove_style_all(r);
        lv_obj_set_size(r, W - 20, 17);
        lv_obj_set_scrollable(r, false);
        int x = 0;
        if (tappable) {
            // A button, not a row: the whole width, 40 px tall, outlined.
            lv_obj_set_height(r, 40);
            lv_obj_set_style_margin_top(r, 4, 0);
            fill(r, c_panel, 8);
            lv_obj_set_style_border_width(r, 2, 0);
            lv_obj_set_style_border_color(r, c_faint, 0);
            lv_obj_set_clickable(r, true);
            lv_obj_add_event_cb(r, [](lv_event_t *) { open_screen_settings(); }, LV_EVENT_CLICKED, nullptr);
            lv_obj_t *more = label(r, &lv_font_montserrat_20, c_fg, ">");
            lv_obj_align(more, LV_ALIGN_RIGHT_MID, -10, 0);
            x = 10;
        }
        lv_obj_t *k = label(r, tappable ? &lv_font_montserrat_14 : &lv_font_montserrat_12, tappable ? c_fg : c_dim, key);
        lv_obj_align(k, LV_ALIGN_LEFT_MID, x, 0);
        lv_obj_t *v = label(r, &lv_font_montserrat_14, tappable ? c_dim : c_fg);
        lv_obj_align(v, LV_ALIGN_LEFT_MID, x + 72, 0);
        return v;
    };
    sy.battery = row("Battery", false);
    sy.power = row("Power", false);
    sy.uptime = row("Uptime", false);
    sy.firmware = row("Firmware", false);
    sy.name = row("Name", false);
    sy.bluetooth = row("Bluetooth", false);
    sy.screen = row("Screen", true);
}

std::string screen_line()
{
    uint16_t t = settings::screen_timeout_seconds();
    std::string after = t % 60 == 0 ? std::to_string(t / 60) + " min" : std::to_string(t) + " s";
    switch (settings::screen_normal()) {
    case settings::ScreenNormal::bright: return "Bright";
    case settings::ScreenNormal::dark: return "Dark after " + after;
    default: return "Dim after " + after;
    }
}

size_t shown_errors = 0;

void update_system(const std::vector<errors::Item> &errs, int64_t now)
{
    if (errs.size() != shown_errors) {
        shown_errors = errs.size();
        lv_obj_clean(sy.errs);
        for (auto &e : errs) {
            lv_obj_t *b = lv_obj_create(sy.errs);
            lv_obj_remove_style_all(b);
            lv_obj_set_size(b, W - 20, LV_SIZE_CONTENT);
            fill(b, c_banner, 6);
            lv_obj_set_style_pad_all(b, 6, 0);
            lv_obj_set_flex_flow(b, LV_FLEX_FLOW_COLUMN);
            label(b, &lv_font_montserrat_12, c_red, error_title(e.error));
            lv_obj_t *m = label(b, &lv_font_montserrat_12, c_banner_text, e.message.c_str());
            lv_obj_set_width(m, W - 32);
            lv_label_set_long_mode(m, LV_LABEL_LONG_MODE_WRAP);
        }
        show(sy.errs, !errs.empty());
    }
    const axp192::Power &p = power;
    char b[48];
    snprintf(b, sizeof(b), "%d%%, %.2f V", p.percent, p.battery_v);
    set_text(sy.battery, p.battery ? b : "No battery");
    if (p.charging) {
        snprintf(b, sizeof(b), "Charging, %.0f mA", p.charge_ma);
    } else if (p.external) {
        snprintf(b, sizeof(b), "External power, full");
    } else {
        snprintf(b, sizeof(b), "On battery, %.0f mA", p.discharge_ma);
    }
    set_text(sy.power, b);
    set_text(sy.uptime, duration(static_cast<uint32_t>(now / 1000)));
    set_text(sy.firmware, esp_app_get_description()->version);
    set_text(sy.name, settings::public_name());
    const uint8_t *a = esp_bt_dev_get_address();
    if (a) {
        snprintf(b, sizeof(b), "%02X:%02X:%02X:%02X:%02X:%02X", a[0], a[1], a[2], a[3], a[4], a[5]);
        set_text(sy.bluetooth, b);
    }
    set_text(sy.screen, screen_line());
}

// ---- flows ----

struct {
    lv_obj_t *pairing, *pair_title, *pair_time, *pair_bar, *pair_text, *pair_small, *pair_digits, *pair_big;
    lv_obj_t *pair_list, *pair_foot;
    lv_obj_t *rows[5], *row_name[5], *row_desc[5], *row_signal[5];
    lv_obj_t *paired, *paired_text;
    lv_obj_t *failed, *failed_why;
    lv_obj_t *screen, *opts[3], *chips[4];
    lv_obj_t *power_off;
    lv_obj_t *forget, *forget_title, *forget_text;
} fl;

constexpr uint16_t timeouts[4] = {15, 30, 60, 300};

lv_obj_t *flow_screen()
{
    lv_obj_t *f = box(flow_box, 0, 0, W, 200);
    show(f, false);
    return f;
}

lv_obj_t *title(lv_obj_t *parent, const char *text, int y, const lv_font_t *font = &lv_font_montserrat_14,
                lv_color_t c = c_fg)
{
    lv_obj_t *l = label(parent, font, c, text);
    lv_obj_set_pos(l, 10, y);
    return l;
}

lv_obj_t *bar(lv_obj_t *parent, int y, lv_color_t c)
{
    lv_obj_t *b = lv_bar_create(parent);
    lv_obj_remove_style_all(b);
    lv_obj_set_pos(b, 10, y);
    lv_obj_set_size(b, W - 20, 4);
    lv_obj_set_style_bg_color(b, c_line, LV_PART_MAIN);
    lv_obj_set_style_bg_opa(b, LV_OPA_COVER, LV_PART_MAIN);
    lv_obj_set_style_radius(b, 2, LV_PART_MAIN);
    lv_obj_set_style_bg_color(b, c, LV_PART_INDICATOR);
    lv_obj_set_style_bg_opa(b, LV_OPA_COVER, LV_PART_INDICATOR);
    lv_obj_set_style_radius(b, 2, LV_PART_INDICATOR);
    lv_bar_set_range(b, 0, 1000);
    return b;
}

void apply_setting(const char *key, cJSON *value)
{
    cJSON *o = cJSON_CreateObject();
    cJSON_AddItemToObject(o, key, value);
    std::string error;
    std::vector<std::string> reboot;
    if (!settings::apply(o, error, reboot)) {
        ESP_LOGW(TAG, "%s", error.c_str());
    }
    cJSON_Delete(o);
}

void update_screen_settings();
void choose_row(int i);

void build_flows()
{
    // Pairing: one screen whose parts update_pairing shows by stage.
    fl.pairing = flow_screen();
    fl.pair_title = title(fl.pairing, "Looking for a radio", 6);
    fl.pair_time = label(fl.pairing, &lv_font_montserrat_12, c_dim);
    lv_obj_align(fl.pair_time, LV_ALIGN_TOP_RIGHT, -10, 8);
    fl.pair_bar = bar(fl.pairing, 30, c_blue);
    fl.pair_text = label(fl.pairing, &lv_font_montserrat_12, c_dim);
    lv_obj_set_pos(fl.pair_text, 10, 44);
    lv_obj_set_width(fl.pair_text, W - 20);
    lv_label_set_long_mode(fl.pair_text, LV_LABEL_LONG_MODE_WRAP);
    fl.pair_small = title(fl.pairing, "The radio should show", 30, &lv_font_montserrat_12, c_dim);
    fl.pair_digits = title(fl.pairing, "", 46, &lv_font_montserrat_36);
    lv_obj_set_style_text_letter_space(fl.pair_digits, 2, 0);
    fl.pair_big = title(fl.pairing, "", 98, &lv_font_montserrat_16);

    // The radios found, while searching: a row each, tapped to choose.
    fl.pair_list = box(fl.pairing, 10, 38, W - 20, 134);
    lv_obj_set_flex_flow(fl.pair_list, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_scrollable(fl.pair_list, true);
    for (int i = 0; i < 5; i++) {
        lv_obj_t *r = box(fl.pair_list, 0, 0, W - 20, 42);
        lv_obj_set_style_border_side(r, LV_BORDER_SIDE_BOTTOM, 0);
        lv_obj_set_style_border_width(r, 1, 0);
        lv_obj_set_style_border_color(r, c_line, 0);
        lv_obj_set_style_bg_color(r, c_panel, LV_STATE_PRESSED);
        lv_obj_set_style_bg_opa(r, LV_OPA_COVER, LV_STATE_PRESSED);
        lv_obj_set_clickable(r, true);
        lv_obj_add_event_cb(r, [](lv_event_t *e) { choose_row((int)(intptr_t)lv_event_get_user_data(e)); },
                            LV_EVENT_CLICKED, (void *)(intptr_t)i);
        fl.row_name[i] = label(r, &lv_font_montserrat_16, c_fg);
        lv_obj_set_pos(fl.row_name[i], 0, 4);
        fl.row_desc[i] = label(r, &lv_font_montserrat_12, c_dim);
        lv_obj_set_pos(fl.row_desc[i], 0, 24);
        fl.row_signal[i] = label(r, &lv_font_montserrat_12, c_dim);
        lv_obj_align(fl.row_signal[i], LV_ALIGN_RIGHT_MID, 0, 0);
        fl.rows[i] = r;
        show(r, false);
    }
    fl.pair_foot = title(fl.pairing, "", 178, &lv_font_montserrat_12, c_dim);
    lv_obj_set_width(fl.pair_foot, W - 20);
    lv_label_set_long_mode(fl.pair_foot, LV_LABEL_LONG_MODE_WRAP);

    fl.paired = flow_screen();
    title(fl.paired, "Done", 6);
    fl.paired_text = title(fl.paired, "", 30, &lv_font_montserrat_20, c_green);
    lv_obj_t *pt = label(fl.paired, &lv_font_montserrat_12, c_dim, "The bridge connects to it now.");
    lv_obj_set_pos(pt, 10, 62);

    fl.failed = flow_screen();
    title(fl.failed, "Pairing", 6);
    title(fl.failed, "Didn't pair", 30, &lv_font_montserrat_20, c_amber);
    // Why, as the radio puts it, then what happens now.
    fl.failed_why = label(fl.failed, &lv_font_montserrat_14, c_fg);
    lv_obj_set_width(fl.failed_why, W - 20);
    lv_label_set_long_mode(fl.failed_why, LV_LABEL_LONG_MODE_WRAP);
    lv_obj_t *ft = label(fl.failed, &lv_font_montserrat_12, c_dim, "The bridge is back on the radio it had.");
    lv_obj_set_width(ft, W - 20);
    lv_label_set_long_mode(ft, LV_LABEL_LONG_MODE_WRAP);
    lv_obj_t *fcol = box(fl.failed, 10, 62, W - 20, 130);
    lv_obj_set_flex_flow(fcol, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_style_pad_row(fcol, 8, 0);
    lv_obj_set_parent(fl.failed_why, fcol);
    lv_obj_set_parent(ft, fcol);

    fl.screen = flow_screen();
    title(fl.screen, "Screen", 2);
    static const char *names[3] = {"Bright", "Dim", "Dark"};
    static const char *lines[3] = {"Always on", "Dims after the timeout; a touch brightens it",
                                   "Off after the timeout; a touch wakes it"};
    for (int i = 0; i < 3; i++) {
        lv_obj_t *o = box(fl.screen, 10, 20 + i * 33, W - 20, 33);
        lv_obj_set_style_border_side(o, LV_BORDER_SIDE_BOTTOM, 0);
        lv_obj_set_style_border_width(o, 1, 0);
        lv_obj_set_style_border_color(o, c_line, 0);
        lv_obj_set_clickable(o, true);
        lv_obj_add_event_cb(
            o,
            [](lv_event_t *e) {
                static const char *vals[3] = {"bright", "dim", "dark"};
                apply_setting("screen_normal", cJSON_CreateString(vals[(int)(intptr_t)lv_event_get_user_data(e)]));
                update_screen_settings();
            },
            LV_EVENT_CLICKED, (void *)(intptr_t)i);
        lv_obj_t *dot = box(o, 0, 8, 16, 16);
        lv_obj_set_style_radius(dot, LV_RADIUS_CIRCLE, 0);
        lv_obj_set_style_border_width(dot, 2, 0);
        fl.opts[i] = dot;
        lv_obj_t *n = label(o, &lv_font_montserrat_14, c_fg, names[i]);
        lv_obj_set_pos(n, 26, 1);
        lv_obj_t *d = label(o, &lv_font_montserrat_12, c_dim, lines[i]);
        lv_obj_set_pos(d, 26, 17);
    }
    title(fl.screen, "Timeout", 124, &lv_font_montserrat_12, c_dim);
    static const char *chip_names[4] = {"15 s", "30 s", "1 min", "5 min"};
    for (int i = 0; i < 4; i++) {
        lv_obj_t *c = box(fl.screen, 10 + i * 76, 140, 70, 30);
        lv_obj_set_style_radius(c, 6, 0);
        lv_obj_set_style_border_width(c, 2, 0);
        lv_obj_set_clickable(c, true);
        lv_obj_add_event_cb(
            c,
            [](lv_event_t *e) {
                apply_setting("screen_timeout_seconds",
                              cJSON_CreateNumber(timeouts[(int)(intptr_t)lv_event_get_user_data(e)]));
                update_screen_settings();
            },
            LV_EVENT_CLICKED, (void *)(intptr_t)i);
        lv_obj_t *l = label(c, &lv_font_montserrat_12, c_dim, chip_names[i]);
        lv_obj_center(l);
        fl.chips[i] = c;
    }

    fl.power_off = flow_screen();
    title(fl.power_off, "Power off?", 4, &lv_font_montserrat_20);
    lv_obj_t *p1 = label(fl.power_off, &lv_font_montserrat_14, c_fg, "The bridge stops, and apps lose the radio.");
    lv_obj_set_pos(p1, 10, 36);
    lv_obj_set_width(p1, W - 20);
    lv_label_set_long_mode(p1, LV_LABEL_LONG_MODE_WRAP);
    lv_obj_t *p2 = label(fl.power_off, &lv_font_montserrat_12, c_dim,
                         "Press the power button on the left side to turn it on again.");
    lv_obj_set_pos(p2, 10, 80);
    lv_obj_set_width(p2, W - 20);
    lv_label_set_long_mode(p2, LV_LABEL_LONG_MODE_WRAP);

    fl.forget = flow_screen();
    fl.forget_title = title(fl.forget, "", 4, &lv_font_montserrat_20);
    fl.forget_text = label(fl.forget, &lv_font_montserrat_14, c_fg);
    lv_obj_set_pos(fl.forget_text, 10, 36);
    lv_obj_set_width(fl.forget_text, W - 20);
    lv_label_set_long_mode(fl.forget_text, LV_LABEL_LONG_MODE_WRAP);
}

// Forget: the confirmation's words for the radio there is now.
void fill_forget()
{
    radio::View v = radio::view();
    set_text(fl.forget_title, "Forget " + v.name + "?");
    set_text(fl.forget_text,
             v.ble ? "The bridge stops connecting to it. To use it again, select it again."
                   : "The bridge stops using it and deletes its pairing. To use it again, pair it again, "
                     "after deleting " + settings::public_name() + " from the radio's Bluetooth list.");
}

void update_screen_settings()
{
    int mode = static_cast<int>(settings::screen_normal()); // bright, dim, dark
    for (int i = 0; i < 3; i++) {
        bool on = i == mode;
        lv_obj_set_style_border_color(fl.opts[i], on ? c_blue : c_faint, 0);
        lv_obj_set_style_bg_color(fl.opts[i], c_blue, 0);
        lv_obj_set_style_bg_opa(fl.opts[i], on ? LV_OPA_COVER : LV_OPA_TRANSP, 0);
    }
    uint16_t t = settings::screen_timeout_seconds();
    bool bright = settings::screen_normal() == settings::ScreenNormal::bright;
    for (int i = 0; i < 4; i++) {
        bool on = timeouts[i] == t;
        lv_obj_set_style_border_color(fl.chips[i], on ? c_blue : c_faint, 0);
        lv_obj_set_style_bg_color(fl.chips[i], lv_color_hex(0x0d2a44), 0);
        lv_obj_set_style_bg_opa(fl.chips[i], on ? LV_OPA_COVER : LV_OPA_TRANSP, 0);
        lv_obj_set_style_opa(fl.chips[i], bright ? LV_OPA_40 : LV_OPA_COVER, 0);
    }
    update_tabs();
}

void page_keys();
void power_off_now();

// The pairing screen for the radio's stage (docs/core2-ui.md, Pairing a
// radio): parts shown, keys and the countdown.
int pair_stage_shown = -1;
std::string row_address[5]; // the radio each row shows
bool chosen = false;        // a row was tapped: the search ends there

void update_list()
{
    radio::SearchView sv = radio::search_view();
    int n = std::min<int>(sv.radios.size(), 5);
    for (int i = 0; i < 5; i++) {
        show(fl.rows[i], i < n);
        if (i >= n) {
            continue;
        }
        const radio::FoundRadio &r = sv.radios[i];
        row_address[i] = r.address;
        set_text(fl.row_name[i], r.name);
        std::string maker = r.make.substr(0, r.make.find(' '));
        set_text(fl.row_desc[i], (maker.empty() ? std::string("Unknown") : maker) +
                                     (r.ble ? ", Bluetooth LE" : ", in pairing mode"));
        set_text(fl.row_signal[i], (r.has_rssi ? std::to_string(r.rssi) + " dBm  >" : std::string(">")));
    }
    std::string foot;
    if (chosen) {
        foot = "Stopping the search...";
    } else if (n == 0) {
        foot = "Put the radio in pairing mode. Kenwood: Menu > Bluetooth > Pairing Mode. TD-H9: Bluetooth on.";
    } else {
        foot = "Tap a radio.";
    }
    if (!chosen && sv.others > 0) {
        foot += " " + std::to_string(sv.others) + (sv.others == 1 ? " other device isn't a radio." : " other devices aren't radios.");
    }
    set_text(fl.pair_foot, foot);
}

void choose_row(int i)
{
    if (chosen || row_address[i].empty()) {
        return;
    }
    chosen = true;
    radio::choose_radio(row_address[i]);
    update_list();
}

void update_pairing()
{
    radio::PairView v = radio::pairing_view();
    using S = radio::PairStage;
    S st = v.stage == S::none ? S::searching : v.stage; // before the task starts, or as it ends
    bool compare = st == S::compare;
    bool press_ok = st == S::press_ok;
    if (static_cast<int>(st) != pair_stage_shown) {
        pair_stage_shown = static_cast<int>(st);
        bool searching = st == S::searching;
        show(fl.pair_list, searching);
        show(fl.pair_foot, searching);
        show(fl.pair_small, compare);
        show(fl.pair_digits, compare);
        show(fl.pair_text, !compare && !searching);
        show(fl.pair_bar, !compare);
        show(fl.pair_time, !compare);
        show(fl.pair_big, compare || press_ok);
        lv_obj_set_pos(fl.pair_bar, 10, press_ok ? 66 : 30);
        lv_obj_set_pos(fl.pair_text, 10, press_ok ? 76 : 44);
        lv_obj_set_pos(fl.pair_big, 10, compare ? 98 : 32);
        lv_obj_set_style_text_font(fl.pair_big, compare ? &lv_font_montserrat_16 : &lv_font_montserrat_20, 0);
        lv_obj_align(fl.pair_time, LV_ALIGN_TOP_RIGHT, -10, press_ok ? 76 : 8);
        switch (st) {
        case S::searching:
            set_text(fl.pair_title, "Looking for radios");
            break;
        case S::pairing:
            set_text(fl.pair_text, "Connecting to the radio. If it still knows this device, that's all; "
                                   "otherwise the radio shows a number.");
            break;
        case S::compare: set_text(fl.pair_big, "Does it show this number?"); break;
        case S::press_ok:
            set_text(fl.pair_big, "Press OK on the radio");
            set_text(fl.pair_text, "It gives you about 30 s");
            break;
        default: break;
        }
        if (compare) {
            set_keys({"No", 0, [] { radio::answer_pairing(false); }}, {},
                     {"Yes", 2, [] { radio::answer_pairing(true); }});
        } else {
            set_keys({"Cancel", 0, [] { radio::cancel_pairing(); }});
        }
        wake();
    }
    if (st == S::searching) {
        update_list();
    } else {
        set_text(fl.pair_title, "Pairing with " + (v.name.empty() ? std::string("the radio") : v.name));
    }
    if (compare && v.number.size() == 6) {
        set_text(fl.pair_digits, v.number.substr(0, 3) + " " + v.number.substr(3));
    }
    uint32_t total = v.stage == S::none ? radio::pairing_seconds : v.total_s;
    uint32_t left = v.stage == S::none ? total : v.left_s;
    set_text(fl.pair_time, std::to_string(left) + " s");
    if (total > 0) {
        lv_bar_set_value(fl.pair_bar, static_cast<int32_t>((total - std::min(left, total)) * 1000 / total), LV_ANIM_OFF);
    }
}

void set_flow(Flow f)
{
    flow = f;
    flow_since = now_ms();
    wake();
    show(fl.pairing, f == Flow::pairing);
    show(fl.paired, f == Flow::paired);
    show(fl.failed, f == Flow::pair_failed);
    show(fl.screen, f == Flow::screen_settings);
    show(fl.power_off, f == Flow::power_off);
    show(fl.forget, f == Flow::forget);
    show(flow_box, f != Flow::none);
    for (int i = 0; i < 3; i++) {
        show(pages[i], f == Flow::none && static_cast<int>(page) == i);
    }
    switch (f) {
    case Flow::none: page_keys(); break;
    case Flow::pairing:
        pair_stage_shown = -1; // update_pairing sets the keys
        chosen = false;
        for (auto &a : row_address) {
            a.clear();
        }
        update_pairing();
        break;
    case Flow::paired: set_keys({}, {}, {"OK", 2, [] { set_flow(Flow::none); }}); break;
    case Flow::pair_failed:
        set_keys({"Back", 0, [] { set_flow(Flow::none); }}, {"Try again", 1, [] {
                     radio::start_pairing();
                     set_flow(Flow::pairing);
                 }});
        break;
    case Flow::screen_settings:
        set_keys({"Back", 0, [] { set_flow(Flow::none); }});
        update_screen_settings();
        break;
    case Flow::power_off:
        set_keys({"Cancel", 0, [] { set_flow(Flow::none); }}, {}, {"Power off", 3, [] { power_off_now(); }});
        break;
    case Flow::forget:
        fill_forget();
        set_keys({"Cancel", 0, [] { set_flow(Flow::none); }}, {}, {"Forget", 3, [] {
                     radio::unpair(control::Pending{}, "");
                     set_flow(Flow::none);
                 }});
        break;
    }
    update_tabs();
}

void open_screen_settings() { set_flow(Flow::screen_settings); }

// "Powering off" with a spinner for a moment, then black with the
// backlight off, so nothing lingers while the power falls.
void power_off_now()
{
    set_keys(); // nothing more to press
    lv_obj_t *scr = lv_obj_create(nullptr);
    fill(scr, c_bg);
    lv_obj_t *t = label(scr, &lv_font_montserrat_20, c_fg, "Powering off");
    lv_obj_align(t, LV_ALIGN_TOP_MID, 0, 80);
    lv_obj_t *sp = lv_spinner_create(scr);
    lv_obj_set_size(sp, 34, 34);
    lv_obj_align(sp, LV_ALIGN_TOP_MID, 0, 128);
    lv_obj_set_style_arc_width(sp, 4, LV_PART_MAIN);
    lv_obj_set_style_arc_color(sp, c_line, LV_PART_MAIN);
    lv_obj_set_style_arc_width(sp, 4, LV_PART_INDICATOR);
    lv_obj_set_style_arc_color(sp, c_red, LV_PART_INDICATOR);
    lv_screen_load(scr);
    lv_timer_t *after = lv_timer_create(
        [](lv_timer_t *) {
            lv_obj_t *black = lv_obj_create(nullptr);
            fill(black, c_bg);
            lv_screen_load(black);
            lv_refr_now(core2::display());
            axp192::backlight(axp192::Backlight::off);
            axp192::power_off();
        },
        1200, nullptr);
    lv_timer_set_repeat_count(after, 1);
}

void page_keys()
{
    switch (page) {
    case Page::bridge: set_keys(); break;
    case Page::tnc: {
        Key pair{"Pair or select", 1, [] {
            radio::start_pairing();
            set_flow(Flow::pairing);
        }};
        if (radio::view().present) {
            set_keys({"Forget", 0, [] { set_flow(Flow::forget); }}, pair);
        } else {
            set_keys({}, pair);
        }
        break;
    }
    case Page::system: set_keys({}, {"Power off", 1, [] { set_flow(Flow::power_off); }}); break;
    }
}

void show_page(Page p)
{
    page = p;
    for (int i = 0; i < 3; i++) {
        show(pages[i], static_cast<int>(p) == i);
    }
    page_keys();
    update_tabs();
}

// ---- touch ----

// Which soft key a touch on the strip is over, or -1 in a gap between
// zones (provisional, to measure on a device).
int zone(int x)
{
    if (x < 100) {
        return 0;
    }
    if (x >= 114 && x < 206) {
        return 1;
    }
    if (x >= 220) {
        return 2;
    }
    return -1;
}

bool screen_is_off() { return lit == axp192::Backlight::off; }

lv_point_t last_point = {0, 0};
bool was_pressed = false;

// LVGL's pointer. A touch on the strip is a touch on that soft key's
// label, so tapping the circle and tapping the label do the same. While
// the screen is off, a touch only wakes it (the wake rule).
void read_touch(lv_indev_t *, lv_indev_data_t *data)
{
    core2::TouchPoint pts[2];
    int n = core2::read_touch(pts, 2);
    data->point = last_point;
    data->state = LV_INDEV_STATE_RELEASED;
    if (n == 0) {
        swallowing = false;
        was_pressed = false;
        return;
    }
    if (!was_pressed) {
        ESP_LOGI(TAG, "touch at %u, %u", pts[0].x, pts[0].y); // to measure the zones
    }
    was_pressed = true;
    if (!swallowing && screen_is_off()) {
        swallowing = true;
        wake();
    }
    if (swallowing) {
        return;
    }
    last_wake = now_ms();
    if (pts[0].y < H) {
        last_point = {(int32_t)pts[0].x, (int32_t)pts[0].y};
        data->point = last_point;
        data->state = LV_INDEV_STATE_PRESSED;
        return;
    }
    int z = zone(pts[0].x);
    if (z < 0 || keys[z].text == nullptr) {
        return;
    }
    last_point = {key_centre[z], H - keys_h / 2};
    data->point = last_point;
    data->state = LV_INDEV_STATE_PRESSED;
}

// ---- the screen's light ----

void set_backlight(axp192::Backlight b)
{
    if (b != lit) {
        lit = b;
        axp192::backlight(b);
    }
}

void update_light(int64_t now)
{
    if (turned_off) {
        set_backlight(axp192::Backlight::off);
        return;
    }
    bool awake = flow != Flow::none || now - last_wake < settings::screen_timeout_seconds() * 1000LL;
    switch (settings::screen_normal()) {
    case settings::ScreenNormal::bright: set_backlight(axp192::Backlight::full); break;
    case settings::ScreenNormal::dim: set_backlight(awake ? axp192::Backlight::full : axp192::Backlight::dim); break;
    case settings::ScreenNormal::dark: set_backlight(awake ? axp192::Backlight::full : axp192::Backlight::off); break;
    }
}

// ---- the timer ----

void on_event(Event e)
{
    switch (e) {
    case Event::pairing_succeeded: {
        radio::View v = radio::view();
        set_text(fl.paired_text, (v.ble ? "Selected " : "Paired with ") + v.name);
        set_flow(Flow::paired);
        break;
    }
    case Event::pairing_failed:
        if (flow == Flow::pairing || flow == Flow::none) {
            std::string why = radio::pairing_failure();
            if (why.empty()) {
                why = "Another pairing or scan is running.";
            } else {
                why[0] = static_cast<char>(toupper(static_cast<unsigned char>(why[0])));
                why += ".";
            }
            set_text(fl.failed_why, why);
            set_flow(Flow::pair_failed);
        }
        break;
    case Event::identify: wake(); break;
    default: break;
    }
}

void tick(lv_timer_t *)
{
    int64_t now = now_ms();
    Event e;
    while (xQueueReceive(queue, &e, 0) == pdTRUE) {
        on_event(e);
    }

    if (axp192::short_presses() > 0) {
        if (screen_is_off()) {
            wake();
        } else {
            turned_off = true;
        }
    }
    if (now - power_read_at >= 1000) {
        power = axp192::read();
        power_read_at = now;
        update_battery();
    }
    move_glint();

    radio::View v = radio::view();
    std::vector<phones::SessionView> apps = phones::session_views();
    std::vector<errors::Item> errs = errors::list();

    // What wakes the screen on its own (docs/core2-ui.md, The screen)
    bool connected = v.state == radio::State::connected;
    if (connected != was_connected || apps.size() != last_sessions || errs.size() > last_errors) {
        wake();
    }
    was_connected = connected;
    if (v.present != had_radio) {
        had_radio = v.present;
        if (flow == Flow::none && page == Page::tnc) {
            page_keys(); // Forget comes and goes with the radio
        }
    }
    last_sessions = apps.size();
    last_errors = errs.size();

    if (flow == Flow::pairing) {
        update_pairing();
    }

    update_bridge(v, apps, errs, now);
    update_tnc(v, errs);
    update_system(errs, now);
    update_light(now);
}

// ---- splash and reset ----

lv_obj_t *splash_title(lv_obj_t *scr, int y)
{
    lv_obj_t *t = label(scr, &lv_font_montserrat_20, c_fg, "kiss-tnc-bridge");
    lv_obj_align(t, LV_ALIGN_TOP_MID, 0, y);
    std::string ver = std::string("firmware ") + esp_app_get_description()->version;
    lv_obj_t *v = label(scr, &lv_font_montserrat_14, c_dim, ver.c_str());
    lv_obj_align(v, LV_ALIGN_TOP_MID, 0, y + 30);
    return t;
}

void splash_tick(lv_timer_t *t)
{
    if (is_started) {
        lv_screen_load(main_scr);
        lv_timer_delete(t);
        return;
    }
    bool slow = !step.empty() && now_ms() - step_since > slow_step_ms;
    set_text(splash_status, slow ? step + "..." : "Starting...");
}

void build_splash()
{
    splash_scr = lv_obj_create(nullptr);
    fill(splash_scr, c_bg);
    splash_title(splash_scr, 62);
    lv_obj_t *sp = lv_spinner_create(splash_scr);
    lv_obj_set_size(sp, 34, 34);
    lv_obj_align(sp, LV_ALIGN_TOP_MID, 0, 128);
    lv_obj_set_style_arc_width(sp, 4, LV_PART_MAIN);
    lv_obj_set_style_arc_color(sp, c_line, LV_PART_MAIN);
    lv_obj_set_style_arc_width(sp, 4, LV_PART_INDICATOR);
    lv_obj_set_style_arc_color(sp, c_blue, LV_PART_INDICATOR);
    splash_status = label(splash_scr, &lv_font_montserrat_12, c_dim, "Starting...");
    lv_obj_align(splash_status, LV_ALIGN_TOP_MID, 0, 174);

    message_scr = lv_obj_create(nullptr);
    fill(message_scr, c_bg);
    message_title = label(message_scr, &lv_font_montserrat_20, c_amber, "Reset");
    lv_obj_set_pos(message_title, 16, 18);
    message_text = label(message_scr, &lv_font_montserrat_14, c_fg, "");
    lv_obj_set_pos(message_text, 16, 52);
    lv_obj_set_width(message_text, W - 32);
    lv_label_set_long_mode(message_text, LV_LABEL_LONG_MODE_WRAP);
    message_bar = bar(message_scr, 110, c_amber);
    lv_obj_set_x(message_bar, 16);
    lv_obj_set_width(message_bar, W - 32);
}

// Both outer circles held: one touch on the strip's left zone and one on
// its right.
bool reset_held()
{
    core2::TouchPoint pts[2];
    int n = core2::read_touch(pts, 2);
    bool left = false, right = false;
    for (int i = 0; i < n; i++) {
        if (pts[i].y >= H) {
            left = left || zone(pts[i].x) == 0;
            right = right || zone(pts[i].x) == 2;
        }
    }
    return left && right;
}

} // namespace

bool reset_requested_at_boot()
{
    queue = xQueueCreate(16, sizeof(Event));
    hw_ok = core2::start();
    if (!core2::display()) {
        return false; // no screen: the device runs without one
    }
    core2::lock(portMAX_DELAY);
    build_splash();
    lv_screen_load(splash_scr);
    lv_refr_now(core2::display());
    core2::unlock();
    set_backlight(axp192::Backlight::full);
    if (!hw_ok) {
        return false; // no touch: no reset gesture
    }

    // Held means held for three readings in a row.
    for (int i = 0; i < 3; i++) {
        if (!reset_held()) {
            return false;
        }
        vTaskDelay(pdMS_TO_TICKS(30));
    }
    ESP_LOGW(TAG, "reset gesture at power-on: let go to cancel");
    core2::lock(portMAX_DELAY);
    lv_label_set_text(message_text, "Keep holding to clear the settings, the radio and the authorized apps. "
                                    "Let go to cancel.");
    lv_bar_set_value(message_bar, 0, LV_ANIM_OFF);
    lv_screen_load(message_scr);
    core2::unlock();
    int64_t start = now_ms();
    int up = 0;
    while (now_ms() - start < reset_warning_ms) {
        vTaskDelay(pdMS_TO_TICKS(50));
        up = reset_held() ? 0 : up + 1;
        if (up >= 2) {
            ESP_LOGI(TAG, "let go: reset cancelled");
            core2::lock(portMAX_DELAY);
            lv_screen_load(splash_scr);
            core2::unlock();
            return false;
        }
        core2::lock(portMAX_DELAY);
        lv_bar_set_value(message_bar, static_cast<int32_t>((now_ms() - start) * 1000 / reset_warning_ms), LV_ANIM_OFF);
        core2::unlock();
    }
    return true;
}

void show_reset_done_blocking()
{
    if (!core2::display()) {
        return;
    }
    core2::lock(portMAX_DELAY);
    lv_label_set_text(message_title, "Reset");
    lv_label_set_text(message_text, "Settings cleared. Restarting...");
    show(message_bar, false);
    lv_screen_load(message_scr);
    lv_refr_now(core2::display());
    core2::unlock();
    vTaskDelay(pdMS_TO_TICKS(1500));
}

void starting(const char *s)
{
    step = s;
    step_since = now_ms();
}

void start()
{
    if (!core2::display()) {
        return;
    }
    core2::lock(portMAX_DELAY);
    main_scr = lv_obj_create(nullptr);
    fill(main_scr, c_bg);
    lv_obj_set_scrollable(main_scr, false);
    for (int i = 0; i < 3; i++) {
        pages[i] = box(main_scr, 0, page_y, W, H - page_y);
    }
    flow_box = box(main_scr, 0, page_y, W, H - page_y);
    show(flow_box, false);
    build_chrome(main_scr);
    build_bridge(pages[0]);
    build_tnc(pages[1]);
    build_system(pages[2]);
    build_flows();
    show_page(Page::bridge);

    lv_indev_t *indev = lv_indev_create();
    lv_indev_set_type(indev, LV_INDEV_TYPE_POINTER);
    lv_indev_set_read_cb(indev, read_touch);
    lv_indev_set_display(indev, core2::display());

    wake();
    lv_timer_create(tick, tick_ms, nullptr);
    lv_timer_create(splash_tick, 100, nullptr);
    core2::unlock();
}

void started() { is_started = true; }

void post(Event e)
{
    if (!queue || e == Event::frame_to_phone || e == Event::frame_to_radio) {
        return; // frames show as counts and lights, read each tick
    }
    if (e == Event::reset_done) {
        show_reset_done_blocking();
        return;
    }
    xQueueSend(queue, &e, 0);
}

void wait_idle(uint32_t timeout_ms) { vTaskDelay(pdMS_TO_TICKS(timeout_ms < 200 ? timeout_ms : 200)); }

void set_updating(bool) {}

} // namespace ktb::ui
