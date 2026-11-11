// ATOM Lite BLE TNC spike: the ATOM as a GATT client to a TIDRADIO TD-H9's
// KISS service (docs/td-h9-ble-kiss.md), with Bluetooth configured as
// firmware 1.1 would have it. See README.md for the procedure.
//
// Bluedroid's callbacks run on its own task and only call Bluedroid's
// APIs, which queue. The console task does the same, so there are no locks;
// the state is a spike's, read from both without care.

#include <atomic>
#include <cctype>
#include <cinttypes>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

#include "driver/uart.h"
#include "driver/uart_vfs.h"
#include "esp_bt.h"
#include "esp_bt_main.h"
#include "esp_gap_ble_api.h"
#include "esp_gap_bt_api.h"
#include "esp_gatt_common_api.h"
#include "esp_gattc_api.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "nvs_flash.h"

namespace {

constexpr const char *TAG = "spike";

// TIDRADIO's KISS service, and the standard client configuration descriptor.
constexpr uint16_t SVC_UUID = 0xAF00;
constexpr uint16_t TX_UUID = 0xAF01; // write without response
constexpr uint16_t RX_UUID = 0xAF02; // notify
constexpr uint16_t CCCD_UUID = 0x2902;
constexpr const char *NAME_PREFIX = "TD-H9";

constexpr uint16_t LOCAL_MTU = 517;
constexpr int64_t RETRY_MS = 15000; // the firmware's reconnect interval
constexpr uint32_t WATCH_SECONDS = 30;
constexpr uint32_t internal_8bit = MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT;

enum class State : uint8_t { idle, searching, watching, connecting, setting_up, ready };

const char *state_name(State s)
{
    switch (s) {
    case State::idle: return "idle";
    case State::searching: return "searching";
    case State::watching: return "watching";
    case State::connecting: return "connecting";
    case State::setting_up: return "setting up";
    case State::ready: return "ready";
    }
    return "?";
}

std::atomic<State> state{State::idle};
esp_gatt_if_t gattc_if = ESP_GATT_IF_NONE;
uint16_t conn_id = 0;
esp_bd_addr_t radio_bda = {};
esp_ble_addr_type_t radio_type = BLE_ADDR_TYPE_PUBLIC;
bool have_radio = false;
bool want_link = true; // reconnect after a drop
uint16_t svc_start = 0, svc_end = 0, tx_handle = 0, rx_handle = 0;
uint16_t mtu = 23;
uint16_t conn_interval = 0, conn_latency = 0, conn_timeout = 0;

int64_t t_search = 0;  // when the search or watch started
int64_t t_connect = 0; // when this connect attempt started
int64_t t_down = 0;    // when the link dropped, 0 if it hasn't
uint32_t scan_seconds = 0;
bool found_this_search = false;

std::atomic<uint32_t> adverts{0}, h9_adverts{0}, classic_found{0};
std::atomic<uint32_t> rx_notifs{0}, rx_bytes{0}, rx_frames{0}, tx_writes{0}, tx_frames{0};
std::atomic<uint32_t> connects{0}, drops{0}, open_failures{0};
uint32_t beacon_count = 0;

esp_timer_handle_t retry_timer;

int64_t now_ms() { return esp_timer_get_time() / 1000; }

std::string bda_str(const uint8_t *b)
{
    char s[18];
    snprintf(s, sizeof(s), "%02X:%02X:%02X:%02X:%02X:%02X", b[0], b[1], b[2], b[3], b[4], b[5]);
    return s;
}

bool parse_bda(const char *s, esp_bd_addr_t out)
{
    unsigned v[6];
    if (sscanf(s, "%x:%x:%x:%x:%x:%x", &v[0], &v[1], &v[2], &v[3], &v[4], &v[5]) != 6) {
        return false;
    }
    for (int i = 0; i < 6; i++) {
        out[i] = static_cast<uint8_t>(v[i]);
    }
    return true;
}

// ---- KISS and AX.25, enough to log what goes by ----

std::vector<uint8_t> unstuff(const uint8_t *f, size_t len)
{
    std::vector<uint8_t> out;
    for (size_t i = 0; i < len; i++) {
        if (f[i] == 0xDB && i + 1 < len) {
            i++;
            out.push_back(f[i] == 0xDC ? 0xC0 : f[i] == 0xDD ? 0xDB : f[i]);
        } else {
            out.push_back(f[i]);
        }
    }
    return out;
}

std::string ax25_call(const uint8_t *a)
{
    std::string s;
    for (int i = 0; i < 6; i++) {
        char c = static_cast<char>(a[i] >> 1);
        if (c != ' ') s.push_back(c);
    }
    int ssid = (a[6] >> 1) & 0x0F;
    if (ssid) s += "-" + std::to_string(ssid);
    return s;
}

void log_frame(const uint8_t *stuffed, size_t len)
{
    std::vector<uint8_t> f = unstuff(stuffed, len);
    if (f.size() < 1 + 14 + 2 || (f[0] & 0x0F) != 0) {
        ESP_LOGI(TAG, "rx frame %u bytes, command 0x%02x", (unsigned)f.size(), f.empty() ? 0 : f[0]);
        return;
    }
    // Command byte, then destination, source and any digipeaters.
    size_t i = 1 + 14;
    while (i < f.size() && !(f[i - 1] & 1)) {
        i += 7;
    }
    std::string info;
    if (i + 2 <= f.size()) {
        for (size_t j = i + 2; j < f.size() && info.size() < 60; j++) {
            info.push_back(isprint(f[j]) ? static_cast<char>(f[j]) : '.');
        }
    }
    ESP_LOGI(TAG, "rx frame %u bytes: %s>%s: %s", (unsigned)f.size(), ax25_call(&f[8]).c_str(),
             ax25_call(&f[1]).c_str(), info.c_str());
}

// Splits notifications into frames, across notifications, skipping the
// empty ones the TD-H9's padding makes.
uint8_t split_buf[1024];
size_t split_len = 0;

void feed(const uint8_t *data, size_t len)
{
    for (size_t i = 0; i < len; i++) {
        if (data[i] == 0xC0) {
            if (split_len > 0) {
                rx_frames++;
                log_frame(split_buf, split_len);
            }
            split_len = 0;
        } else if (split_len < sizeof(split_buf)) {
            split_buf[split_len++] = data[i];
        }
    }
}

void ax25_addr(std::vector<uint8_t> &out, const std::string &call, bool last)
{
    std::string base = call;
    int ssid = 0;
    size_t dash = call.find('-');
    if (dash != std::string::npos) {
        base = call.substr(0, dash);
        ssid = atoi(call.c_str() + dash + 1);
    }
    for (size_t i = 0; i < 6; i++) {
        char c = i < base.size() ? static_cast<char>(toupper(base[i])) : ' ';
        out.push_back(static_cast<uint8_t>(c << 1));
    }
    out.push_back(static_cast<uint8_t>(0x60 | ((ssid & 0x0F) << 1) | (last ? 1 : 0)));
}

// A KISS frame: an APRS status packet from the spike's callsign.
std::vector<uint8_t> beacon()
{
    std::vector<uint8_t> ax;
    ax25_addr(ax, "APZKTB", false);
    ax25_addr(ax, CONFIG_SPIKE_CALLSIGN, false);
    ax25_addr(ax, "WIDE1-1", true);
    ax.push_back(0x03);
    ax.push_back(0xF0);
    char info[48];
    snprintf(info, sizeof(info), ">kiss-tnc-bridge atom td-h9 spike %u", (unsigned)++beacon_count);
    ax.insert(ax.end(), info, info + strlen(info));

    std::vector<uint8_t> k = {0xC0, 0x00};
    for (uint8_t b : ax) {
        if (b == 0xC0) {
            k.push_back(0xDB);
            k.push_back(0xDC);
        } else if (b == 0xDB) {
            k.push_back(0xDB);
            k.push_back(0xDD);
        } else {
            k.push_back(b);
        }
    }
    k.push_back(0xC0);
    return k;
}

// ---- The link ----

void write_tx(const uint8_t *data, size_t len)
{
    static uint8_t buf[LOCAL_MTU];
    if (len > static_cast<size_t>(mtu - 3) || len > sizeof(buf)) {
        ESP_LOGW(TAG, "write of %u bytes is over the MTU's %u", (unsigned)len, (unsigned)(mtu - 3));
        return;
    }
    memcpy(buf, data, len);
    esp_err_t err = esp_ble_gattc_write_char(gattc_if, conn_id, tx_handle, len, buf,
                                             ESP_GATT_WRITE_TYPE_NO_RSP, ESP_GATT_AUTH_REQ_NONE);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "write_char: %s", esp_err_to_name(err));
    } else {
        tx_writes++;
    }
}

void connect()
{
    if (!have_radio || gattc_if == ESP_GATT_IF_NONE) return;
    state = State::connecting;
    t_connect = now_ms();
    ESP_LOGI(TAG, "connecting to %s (address type %d)", bda_str(radio_bda).c_str(), radio_type);
    esp_err_t err = esp_ble_gattc_open(gattc_if, radio_bda, radio_type, true);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "gattc_open: %s", esp_err_to_name(err));
        state = State::idle;
        esp_timer_start_once(retry_timer, RETRY_MS * 1000);
    }
}

void retry(void *)
{
    if (state == State::idle && want_link) connect();
}

void start_scan(State s, uint32_t seconds)
{
    state = s;
    scan_seconds = seconds;
    found_this_search = false;
    t_search = now_ms();
    esp_ble_scan_params_t p = {};
    p.scan_type = BLE_SCAN_TYPE_ACTIVE; // the name may be in the scan response
    p.own_addr_type = BLE_ADDR_TYPE_PUBLIC;
    p.scan_filter_policy = BLE_SCAN_FILTER_ALLOW_ALL;
    p.scan_interval = 0x50; // 50 ms
    p.scan_window = 0x30;   // 30 ms
    p.scan_duplicate = BLE_SCAN_DUPLICATE_DISABLE;
    esp_ble_gap_set_scan_params(&p);
#if CONFIG_SPIKE_CLASSIC_INQUIRY
    if (s == State::searching) {
        // 0x30 is the longest inquiry: 48 x 1.28 s, about 61 s.
        esp_bt_gap_start_discovery(ESP_BT_INQ_MODE_GENERAL_INQUIRY, 0x30, 0);
    }
#endif
}

void stop_search()
{
    esp_ble_gap_stop_scanning();
#if CONFIG_SPIKE_CLASSIC_INQUIRY
    esp_bt_gap_cancel_discovery();
#endif
}

// ---- Bluedroid callbacks ----

std::string adv_name(esp_ble_gap_cb_param_t::ble_scan_result_evt_param &r)
{
    uint8_t len = 0;
    uint16_t total = r.adv_data_len + r.scan_rsp_len;
    uint8_t *n = esp_ble_resolve_adv_data_by_type(r.ble_adv, total, ESP_BLE_AD_TYPE_NAME_CMPL, &len);
    if (!n) n = esp_ble_resolve_adv_data_by_type(r.ble_adv, total, ESP_BLE_AD_TYPE_NAME_SHORT, &len);
    return n ? std::string(reinterpret_cast<char *>(n), len) : std::string();
}

void gap_ble_cb(esp_gap_ble_cb_event_t event, esp_ble_gap_cb_param_t *p)
{
    switch (event) {
    case ESP_GAP_BLE_SCAN_PARAM_SET_COMPLETE_EVT:
        esp_ble_gap_start_scanning(scan_seconds);
        break;
    case ESP_GAP_BLE_SCAN_START_COMPLETE_EVT:
        ESP_LOGI(TAG, "BLE scan %s for %u s (%s)",
                 p->scan_start_cmpl.status == ESP_BT_STATUS_SUCCESS ? "started" : "FAILED",
                 (unsigned)scan_seconds, state_name(state));
        break;
    case ESP_GAP_BLE_SCAN_RESULT_EVT: {
        auto &r = p->scan_rst;
        if (r.search_evt == ESP_GAP_SEARCH_INQ_CMPL_EVT) {
            if (state == State::searching || state == State::watching) {
                ESP_LOGI(TAG, "scan ended after %lld ms: %u adverts, %u from a TD-H9%s",
                         now_ms() - t_search, (unsigned)adverts.load(), (unsigned)h9_adverts.load(),
                         found_this_search ? "" : ", none found");
                state = State::idle;
            }
            break;
        }
        if (r.search_evt != ESP_GAP_SEARCH_INQ_RES_EVT) break;
        adverts++;
        std::string name = adv_name(r);
        if (name.compare(0, strlen(NAME_PREFIX), NAME_PREFIX) != 0) break;
        h9_adverts++;
        ESP_LOGI(TAG, "+%lld ms: %s %s, address type %d, rssi %d", now_ms() - t_search, name.c_str(),
                 bda_str(r.bda).c_str(), r.ble_addr_type, r.rssi);
        if (state == State::searching && !found_this_search) {
            found_this_search = true;
            stop_search();
            memcpy(radio_bda, r.bda, sizeof(radio_bda));
            radio_type = r.ble_addr_type;
            have_radio = true;
            want_link = true;
            ESP_LOGI(TAG, "selected %s after %lld ms", bda_str(radio_bda).c_str(), now_ms() - t_search);
            connect();
        }
        break;
    }
    case ESP_GAP_BLE_UPDATE_CONN_PARAMS_EVT: {
        auto &u = p->update_conn_params;
        conn_interval = u.conn_int;
        conn_latency = u.latency;
        conn_timeout = u.timeout;
        ESP_LOGI(TAG, "connection parameters (status %d): interval %.2f ms, latency %u, timeout %u ms",
                 u.status, u.conn_int * 1.25, u.latency, u.timeout * 10);
        break;
    }
    default:
        break;
    }
}

void gap_bt_cb(esp_bt_gap_cb_event_t event, esp_bt_gap_cb_param_t *p)
{
    switch (event) {
    case ESP_BT_GAP_DISC_RES_EVT:
        classic_found++;
        ESP_LOGI(TAG, "+%lld ms: Classic device %s", now_ms() - t_search, bda_str(p->disc_res.bda).c_str());
        break;
    case ESP_BT_GAP_DISC_STATE_CHANGED_EVT:
        ESP_LOGI(TAG, "Classic inquiry %s",
                 p->disc_st_chg.state == ESP_BT_GAP_DISCOVERY_STARTED ? "started" : "stopped");
        break;
    default:
        break;
    }
}

void link_down()
{
    state = State::idle;
    tx_handle = rx_handle = svc_start = svc_end = 0;
    split_len = 0;
    mtu = 23;
}

void gattc_cb(esp_gattc_cb_event_t event, esp_gatt_if_t ifc, esp_ble_gattc_cb_param_t *p)
{
    switch (event) {
    case ESP_GATTC_REG_EVT:
        gattc_if = ifc;
        if (strlen(CONFIG_SPIKE_RADIO_ADDR) > 0 && parse_bda(CONFIG_SPIKE_RADIO_ADDR, radio_bda)) {
            have_radio = true;
            connect();
        } else {
            start_scan(State::searching, CONFIG_SPIKE_SEARCH_SECONDS);
        }
        break;
    case ESP_GATTC_CONNECT_EVT:
        conn_id = p->connect.conn_id;
        conn_interval = p->connect.conn_params.interval;
        conn_latency = p->connect.conn_params.latency;
        conn_timeout = p->connect.conn_params.timeout;
        ESP_LOGI(TAG, "link up after %lld ms: interval %.2f ms, latency %u, timeout %u ms",
                 now_ms() - t_connect, conn_interval * 1.25, conn_latency, conn_timeout * 10);
        break;
    case ESP_GATTC_OPEN_EVT:
        if (p->open.status != ESP_GATT_OK) {
            open_failures++;
            ESP_LOGW(TAG, "open failed after %lld ms: status 0x%02x; again in %lld s", now_ms() - t_connect,
                     p->open.status, RETRY_MS / 1000);
            link_down();
            if (want_link) esp_timer_start_once(retry_timer, RETRY_MS * 1000);
            break;
        }
        conn_id = p->open.conn_id;
        state = State::setting_up;
        if (!want_link) {
            // "drop" came while connecting.
            esp_ble_gap_disconnect(radio_bda);
            break;
        }
        esp_ble_gattc_send_mtu_req(gattc_if, conn_id);
        break;
    case ESP_GATTC_CFG_MTU_EVT: {
        mtu = p->cfg_mtu.mtu;
        ESP_LOGI(TAG, "MTU %u (status %d)", mtu, p->cfg_mtu.status);
        esp_bt_uuid_t svc = {};
        svc.len = ESP_UUID_LEN_16;
        svc.uuid.uuid16 = SVC_UUID;
        esp_ble_gattc_search_service(gattc_if, conn_id, &svc);
        break;
    }
    case ESP_GATTC_SEARCH_RES_EVT:
        if (p->search_res.srvc_id.uuid.len == ESP_UUID_LEN_16 && p->search_res.srvc_id.uuid.uuid.uuid16 == SVC_UUID) {
            svc_start = p->search_res.start_handle;
            svc_end = p->search_res.end_handle;
        }
        break;
    case ESP_GATTC_SEARCH_CMPL_EVT: {
        if (!svc_start) {
            ESP_LOGW(TAG, "no 0x%04X service; disconnecting", SVC_UUID);
            esp_ble_gap_disconnect(radio_bda);
            break;
        }
        esp_gattc_char_elem_t el;
        uint16_t count = 1;
        esp_bt_uuid_t u = {};
        u.len = ESP_UUID_LEN_16;
        u.uuid.uuid16 = TX_UUID;
        if (esp_ble_gattc_get_char_by_uuid(gattc_if, conn_id, svc_start, svc_end, u, &el, &count) == ESP_GATT_OK &&
            count) {
            tx_handle = el.char_handle;
            ESP_LOGI(TAG, "0x%04X handle 0x%04x, properties 0x%02x", TX_UUID, tx_handle, el.properties);
        }
        count = 1;
        u.uuid.uuid16 = RX_UUID;
        if (esp_ble_gattc_get_char_by_uuid(gattc_if, conn_id, svc_start, svc_end, u, &el, &count) == ESP_GATT_OK &&
            count) {
            rx_handle = el.char_handle;
            ESP_LOGI(TAG, "0x%04X handle 0x%04x, properties 0x%02x", RX_UUID, rx_handle, el.properties);
        }
        if (!tx_handle || !rx_handle) {
            ESP_LOGW(TAG, "missing a characteristic; disconnecting");
            esp_ble_gap_disconnect(radio_bda);
            break;
        }
        esp_ble_gattc_register_for_notify(gattc_if, radio_bda, rx_handle);
        break;
    }
    case ESP_GATTC_REG_FOR_NOTIFY_EVT: {
        esp_gattc_descr_elem_t d;
        uint16_t count = 1;
        esp_bt_uuid_t u = {};
        u.len = ESP_UUID_LEN_16;
        u.uuid.uuid16 = CCCD_UUID;
        if (esp_ble_gattc_get_descr_by_char_handle(gattc_if, conn_id, rx_handle, u, &d, &count) != ESP_GATT_OK ||
            !count) {
            ESP_LOGW(TAG, "no CCCD on 0x%04X", RX_UUID);
            break;
        }
        static uint8_t on[2] = {0x01, 0x00};
        esp_ble_gattc_write_char_descr(gattc_if, conn_id, d.handle, sizeof(on), on, ESP_GATT_WRITE_TYPE_RSP,
                                       ESP_GATT_AUTH_REQ_NONE);
        break;
    }
    case ESP_GATTC_WRITE_DESCR_EVT:
        if (p->write.status != ESP_GATT_OK) {
            ESP_LOGW(TAG, "subscribing failed: status 0x%02x", p->write.status);
            break;
        }
        state = State::ready;
        connects++;
        ESP_LOGI(TAG, "READY %lld ms after starting to connect", now_ms() - t_connect);
        if (t_down) {
            ESP_LOGI(TAG, "the link was down for %.1f s", (now_ms() - t_down) / 1000.0);
            t_down = 0;
        }
        break;
    case ESP_GATTC_NOTIFY_EVT:
        if (p->notify.handle == rx_handle) {
            rx_notifs++;
            rx_bytes += p->notify.value_len;
            feed(p->notify.value, p->notify.value_len);
        }
        break;
    case ESP_GATTC_WRITE_CHAR_EVT:
        if (p->write.status != ESP_GATT_OK) {
            ESP_LOGW(TAG, "write status 0x%02x", p->write.status);
        }
        break;
    case ESP_GATTC_CONGEST_EVT:
        ESP_LOGI(TAG, "congested: %s", p->congest.congested ? "yes" : "no");
        break;
    case ESP_GATTC_DISCONNECT_EVT: {
        bool was_up = state == State::ready || state == State::setting_up;
        ESP_LOGW(TAG, "disconnected: reason 0x%02x", p->disconnect.reason);
        link_down();
        if (was_up) {
            drops++;
            t_down = now_ms();
        }
        if (want_link) {
            // Straight back: a direct connect waits for the radio to advertise.
            connect();
        }
        break;
    }
    default:
        break;
    }
}

// ---- Console ----

void report()
{
    multi_heap_info_t info;
    heap_caps_get_info(&info, internal_8bit);
    ESP_LOGI(TAG,
             "t=%llds %s heap free=%u min=%u largest=%u | mtu %u interval %.2f ms | rx %u notif %u B %u "
             "frames | tx %u writes %u frames | connects %u drops %u open_fail %u",
             esp_timer_get_time() / 1000000, state_name(state), (unsigned)info.total_free_bytes,
             (unsigned)info.minimum_free_bytes, (unsigned)info.largest_free_block, mtu, conn_interval * 1.25,
             (unsigned)rx_notifs.load(), (unsigned)rx_bytes.load(), (unsigned)rx_frames.load(),
             (unsigned)tx_writes.load(), (unsigned)tx_frames.load(), (unsigned)connects.load(),
             (unsigned)drops.load(), (unsigned)open_failures.load());
}

void report_task(void *)
{
    for (;;) {
        vTaskDelay(pdMS_TO_TICKS(CONFIG_SPIKE_REPORT_INTERVAL_S * 1000));
        report();
    }
}

void help()
{
    printf("commands:\n"
           "  beacon              send a status packet, one write\n"
           "  split               send a status packet in two writes\n"
           "  drop                disconnect, and stay disconnected\n"
           "  connect             connect to the selected TD-H9 again\n"
           "  search              forget it and search, as a double press would\n"
           "  watch               log every TD-H9 advert for %u s, no connecting\n"
           "  params MIN MAX [LATENCY [TIMEOUT]]  ask for a connection interval (ms)\n"
           "  status\n",
           (unsigned)WATCH_SECONDS);
}

void command(const std::string &line)
{
    char cmd[16] = {};
    sscanf(line.c_str(), "%15s", cmd);
    std::string c = cmd;
    if (c.empty()) return;
    bool idle = state == State::idle;
    if (c == "beacon" || c == "split") {
        if (state != State::ready) {
            printf("not connected\n");
            return;
        }
        std::vector<uint8_t> f = beacon();
        if (c == "beacon") {
            write_tx(f.data(), f.size());
        } else {
            write_tx(f.data(), 20);
            write_tx(f.data() + 20, f.size() - 20);
        }
        tx_frames++;
        ESP_LOGI(TAG, "sent beacon %u, %u bytes%s", (unsigned)beacon_count, (unsigned)f.size(),
                 c == "split" ? " in two writes (20 + the rest)" : "");
    } else if (c == "drop") {
        want_link = false;
        esp_timer_stop(retry_timer);
        if (state == State::connecting) {
            esp_ble_gap_disconnect(radio_bda); // cancels a pending direct connect too
        } else if (!idle) {
            esp_ble_gap_disconnect(radio_bda);
        }
    } else if (c == "connect") {
        want_link = true;
        if (idle) connect();
    } else if (c == "search" || c == "watch") {
        if (!idle) {
            printf("drop first (state: %s)\n", state_name(state));
            return;
        }
        adverts = 0;
        h9_adverts = 0;
        classic_found = 0;
        if (c == "search") {
            have_radio = false;
            start_scan(State::searching, CONFIG_SPIKE_SEARCH_SECONDS);
        } else {
            start_scan(State::watching, WATCH_SECONDS);
        }
    } else if (c == "params") {
        float lo = 0, hi = 0;
        unsigned lat = 0, tout = 4000;
        if (sscanf(line.c_str(), "%*s %f %f %u %u", &lo, &hi, &lat, &tout) < 2 || state != State::ready) {
            printf("params MIN_MS MAX_MS [LATENCY [TIMEOUT_MS]], while connected\n");
            return;
        }
        esp_ble_conn_update_params_t u = {};
        memcpy(u.bda, radio_bda, sizeof(u.bda));
        u.min_int = static_cast<uint16_t>(lo / 1.25f);
        u.max_int = static_cast<uint16_t>(hi / 1.25f);
        u.latency = static_cast<uint16_t>(lat);
        u.timeout = static_cast<uint16_t>(tout / 10);
        esp_err_t err = esp_ble_gap_update_conn_params(&u);
        ESP_LOGI(TAG, "asked for %.2f-%.2f ms, latency %u, timeout %u ms: %s", u.min_int * 1.25, u.max_int * 1.25,
                 u.latency, u.timeout * 10, esp_err_to_name(err));
    } else if (c == "status") {
        report();
    } else {
        help();
    }
}

void console_task(void *)
{
    std::string line;
    for (;;) {
        uint8_t buf[64];
        int n = uart_read_bytes(UART_NUM_0, buf, sizeof(buf), pdMS_TO_TICKS(100));
        for (int i = 0; i < n; i++) {
            char ch = static_cast<char>(buf[i]);
            if (ch == '\r' || ch == '\n') {
                if (!line.empty()) command(line);
                line.clear();
            } else if (line.size() < 80) {
                line.push_back(ch);
            }
        }
    }
}

void heap_mark(const char *label)
{
    multi_heap_info_t info;
    heap_caps_get_info(&info, internal_8bit);
    ESP_LOGI(TAG, "heap @ %-24s free=%6u min=%6u largest=%6u", label, (unsigned)info.total_free_bytes,
             (unsigned)info.minimum_free_bytes, (unsigned)info.largest_free_block);
}

} // namespace

extern "C" void app_main()
{
    heap_mark("app_main");
    esp_err_t err = nvs_flash_init();
    if (err == ESP_ERR_NVS_NO_FREE_PAGES || err == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_ERROR_CHECK(nvs_flash_erase());
        err = nvs_flash_init();
    }
    ESP_ERROR_CHECK(err);

    ESP_ERROR_CHECK(uart_driver_install(UART_NUM_0, 512, 0, 0, nullptr, 0));
    uart_vfs_dev_use_driver(UART_NUM_0);

    esp_timer_create_args_t targs = {};
    targs.callback = retry;
    targs.name = "retry";
    ESP_ERROR_CHECK(esp_timer_create(&targs, &retry_timer));

    esp_bt_controller_config_t bt_cfg = BT_CONTROLLER_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_bt_controller_init(&bt_cfg));
    ESP_ERROR_CHECK(esp_bt_controller_enable(ESP_BT_MODE_BTDM));
    heap_mark("controller, dual mode");
    esp_bluedroid_config_t bd_cfg = BT_BLUEDROID_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_bluedroid_init_with_cfg(&bd_cfg));
    ESP_ERROR_CHECK(esp_bluedroid_enable());
    heap_mark("Bluedroid");

    // As the firmware: modem sleep on.
    err = esp_bt_sleep_enable();
    ESP_LOGI(TAG, "modem sleep: %s", esp_err_to_name(err));

    esp_bt_gap_set_device_name("KTB BLE TNC spike");
    esp_bt_gap_set_scan_mode(ESP_BT_NON_CONNECTABLE, ESP_BT_NON_DISCOVERABLE);
    ESP_ERROR_CHECK(esp_bt_gap_register_callback(gap_bt_cb));
    ESP_ERROR_CHECK(esp_ble_gap_register_callback(gap_ble_cb));
    ESP_ERROR_CHECK(esp_ble_gattc_register_callback(gattc_cb));
    ESP_ERROR_CHECK(esp_ble_gatt_set_local_mtu(LOCAL_MTU));
    ESP_ERROR_CHECK(esp_ble_gattc_app_register(0));
    heap_mark("GATT client registered");

    help();
    xTaskCreate(console_task, "console", 6144, nullptr, 5, nullptr);
    xTaskCreate(report_task, "report", 4096, nullptr, 3, nullptr);
}
