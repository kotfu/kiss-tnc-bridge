#include "radio.h"

#include <algorithm>
#include <cinttypes>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <vector>

#include "esp_app_desc.h"
#include "esp_bt.h"
#include "esp_bt_device.h"
#include "esp_bt_main.h"
#include "esp_gap_bt_api.h"
#include "esp_gatt_common_api.h"
#include "esp_gattc_api.h"
#include "esp_log.h"
#include "esp_spp_api.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "freertos/stream_buffer.h"
#include "freertos/task.h"
#include "activity.h"
#include "board.h"
#include "errors.h"
#include "freertos/message_buffer.h"
#include "kiss.h"
#include "phones.h"
#include "nvs.h"
#include "settings.h"
#include "ui.h"

// Bluedroid internals (bta_sys.h isn't a public header). Clearing
// HCI_ENABLE_SNIFF_MODE from a link's policy stops the power manager from
// putting it in sniff mode, and wakes it if it's already there. The TH-D75
// sometimes doesn't answer a sniff exit, and the link drops (roadmap,
// Hardware notes).
extern "C" void bta_sys_clear_policy(uint8_t id, uint8_t policy, uint8_t *peer_addr);

namespace ktb::radio {

namespace {

constexpr const char *TAG = "radio";
constexpr uint8_t HCI_ENABLE_SNIFF_MODE = 0x04;

constexpr TickType_t retry_delay = pdMS_TO_TICKS(15000); // bb-link's reconnect interval
constexpr TickType_t connect_timeout = pdMS_TO_TICKS(20000);
constexpr TickType_t reply_timeout = pdMS_TO_TICKS(1000);
constexpr TickType_t write_timeout = pdMS_TO_TICKS(5000);
constexpr TickType_t close_timeout = pdMS_TO_TICKS(5000);
constexpr TickType_t name_timeout = pdMS_TO_TICKS(6000);
// After the radio shows its number: its own window is about 30 s.
constexpr TickType_t confirm_timeout = pdMS_TO_TICKS(30000);
// From starting to page a radio to its pairing finishing.
constexpr TickType_t pair_timeout = pdMS_TO_TICKS(60000);

// Kenwood's TH-D74 and TH-D75 both report this Class of Device: a phone.
constexpr uint32_t cod_kenwood = 0x620204;

// ---- Models (tnc.models) ----

struct Model {
    const char *name;
    const char *radio;       // known radio, or null
    const char *name_prefix; // its Bluetooth name starts with this
    bool kiss_init;          // the TH-D7x init at every connect
    bool ble;                // a BLE TNC, not Classic SPP
};

// generic stays third: it's the fallback for a model that isn't known.
const Model models[] = {
    {"th-d74", "Kenwood TH-D74", "TH-D74", true, false},
    {"th-d75", "Kenwood TH-D75", "TH-D75", true, false},
    {"generic", nullptr, nullptr, false, false},
    {"td-h9", "TIDRADIO TD-H9", "TD-H9", false, true},
};
const Model &generic_model = models[2];

const Model *find_model(const std::string &name)
{
    for (const Model &m : models) {
        if (name == m.name) {
            return &m;
        }
    }
    return nullptr;
}

const Model &model_of(const std::string &name)
{
    const Model *m = find_model(name);
    return m ? *m : generic_model;
}

// The model a Bluetooth name belongs to, or null. ble: the name came from
// a BLE advertisement, not a Classic inquiry (a TD-H9 answers both, under
// different addresses, and only its BLE side has the TNC).
const Model *model_for_name(const std::string &name, bool ble)
{
    for (const Model &m : models) {
        if (m.name_prefix && m.ble == ble && name.compare(0, strlen(m.name_prefix), m.name_prefix) == 0) {
            return &m;
        }
    }
    return nullptr;
}

// ---- Addresses ----

struct Addr {
    esp_bd_addr_t b{};
    bool operator==(const Addr &o) const { return memcmp(b, o.b, sizeof(b)) == 0; }
    bool operator!=(const Addr &o) const { return !(*this == o); }
    uint8_t *raw() { return b; }
    static Addr of(const uint8_t *bda)
    {
        Addr a;
        memcpy(a.b, bda, sizeof(a.b));
        return a;
    }
    std::string str() const
    {
        char s[18];
        snprintf(s, sizeof(s), "%02X:%02X:%02X:%02X:%02X:%02X", b[0], b[1], b[2], b[3], b[4], b[5]);
        return s;
    }
    static bool parse(const std::string &text, Addr &out)
    {
        unsigned v[6];
        char extra;
        if (text.size() != 17 || sscanf(text.c_str(), "%2x:%2x:%2x:%2x:%2x:%2x%c", &v[0], &v[1], &v[2],
                                        &v[3], &v[4], &v[5], &extra) != 6) {
            return false;
        }
        for (int i = 0; i < 6; i++) {
            out.b[i] = static_cast<uint8_t>(v[i]);
        }
        return true;
    }
};

bool bonded(const Addr &a)
{
    int n = esp_bt_gap_get_bond_device_num();
    if (n <= 0) {
        return false;
    }
    std::vector<esp_bd_addr_t> list(n);
    esp_bt_gap_get_bond_device_list(&n, list.data());
    for (int i = 0; i < n; i++) {
        if (memcmp(list[i], a.b, sizeof(esp_bd_addr_t)) == 0) {
            return true;
        }
    }
    return false;
}

// ---- The saved radio (NVS) ----

struct Record {
    bool present = false;
    Addr addr;
    uint8_t addr_type = BLE_ADDR_TYPE_PUBLIC; // a BLE radio's
    std::string model = "th-d75";
    std::string name;
    bool ble() const { return model_of(model).ble; }
};

std::mutex state_lock; // guards rec and port_detail
Record rec;

Record record()
{
    std::lock_guard<std::mutex> g(state_lock);
    return rec;
}

bool save_record(const Record &r)
{
    nvs_handle_t h;
    esp_err_t err = nvs_open(nvs_namespace, NVS_READWRITE, &h);
    if (err != ESP_OK) {
        return false;
    }
    if (r.present) {
        err = nvs_set_blob(h, "address", r.addr.b, sizeof(r.addr.b));
        if (err == ESP_OK) err = nvs_set_str(h, "model", r.model.c_str());
        if (err == ESP_OK) err = nvs_set_str(h, "name", r.name.c_str());
        if (err == ESP_OK) err = nvs_set_u8(h, "addr_type", r.addr_type);
    } else {
        err = nvs_erase_all(h);
    }
    if (err == ESP_OK) {
        err = nvs_commit(h);
    }
    nvs_close(h);
    if (err == ESP_OK) {
        std::lock_guard<std::mutex> g(state_lock);
        rec = r;
    } else {
        ESP_LOGE(TAG, "couldn't save the radio: %s", esp_err_to_name(err));
    }
    return err == ESP_OK;
}

void load_record()
{
    Record r;
    nvs_handle_t h;
    if (nvs_open(nvs_namespace, NVS_READONLY, &h) == ESP_OK) {
        size_t len = sizeof(r.addr.b);
        if (nvs_get_blob(h, "address", r.addr.b, &len) == ESP_OK && len == sizeof(r.addr.b)) {
            r.present = true;
            char buf[64];
            len = sizeof(buf);
            if (nvs_get_str(h, "model", buf, &len) == ESP_OK && find_model(buf)) {
                r.model = buf;
            }
            len = sizeof(buf);
            if (nvs_get_str(h, "name", buf, &len) == ESP_OK) {
                r.name = buf;
            }
            // Missing before 1.1, when every radio was Classic.
            uint8_t type;
            if (nvs_get_u8(h, "addr_type", &type) == ESP_OK) {
                r.addr_type = type;
            }
        }
        nvs_close(h);
    }
    std::lock_guard<std::mutex> g(state_lock);
    rec = r;
}

// ---- What status and the LED see ----

enum class PortState : uint8_t { unpaired, idle, connecting, connected, backoff };

const char *port_state_name(PortState s)
{
    switch (s) {
    case PortState::unpaired: return "unpaired";
    case PortState::idle: return "idle";
    case PortState::connecting: return "connecting";
    case PortState::connected: return "connected";
    case PortState::backoff: return "backoff";
    }
    return "?";
}

std::atomic<PortState> port_state{PortState::unpaired};
std::string port_detail; // under state_lock
std::atomic<bool> pairing_now{false};
std::atomic<bool> busy_now{false}; // a scan or a pairing is running

// For pairing_view
std::mutex pair_view_lock;
PairView pair_view_now;       // left_s unused: from pair_view_until
int64_t pair_view_until = 0;  // esp_timer microseconds

void set_pair_stage(PairStage stage, uint32_t seconds, const std::string &name = "",
                    const std::string &number = "")
{
    std::lock_guard<std::mutex> g(pair_view_lock);
    pair_view_now.stage = stage;
    pair_view_now.name = name;
    pair_view_now.number = number;
    pair_view_now.total_s = seconds;
    pair_view_until = esp_timer_get_time() + int64_t(seconds) * 1000000;
}
// True while Bluetooth and the radio task are up. Otherwise nothing answers
// requests, so post refuses them.
std::atomic<bool> running{false};
std::atomic<Hold> hold_state{Hold::none};
std::atomic<int> debug_phones{-1}; // debug.set phones=, until -1
std::atomic<bool> override_on{false};
std::atomic<Link> override_link{Link::none};

struct Counters {
    std::atomic<uint32_t> frames_up{0};
    std::atomic<uint32_t> frames_down{0};
    std::atomic<uint32_t> dropped{0};
    std::atomic<uint32_t> tnc_connects{0};
} counters;

// For view(): when the port entered its state, and the counters when the
// link last connected, so a screen counts frames since then.
std::atomic<int64_t> state_since_us{0};
std::atomic<int64_t> link_down_us{0}; // when the link last dropped, or 0
std::atomic<uint32_t> link_start_up{0};
std::atomic<uint32_t> link_start_down{0};

void set_port(PortState s, const std::string &detail = "")
{
    PortState was = port_state.exchange(s);
    {
        std::lock_guard<std::mutex> g(state_lock);
        port_detail = detail;
    }
    if (was != s) {
        state_since_us = esp_timer_get_time();
        if (s == PortState::connected) {
            link_start_up = counters.frames_up.load();
            link_start_down = counters.frames_down.load();
        } else if (was == PortState::connected) {
            link_down_us = state_since_us.load();
        }
        ESP_LOGI(TAG, "port: %s%s%s", port_state_name(s), detail.empty() ? "" : ", ",
                 detail.c_str());
        if (control::subscribed(control::topic_state)) {
            cJSON *e = cJSON_CreateObject();
            cJSON_AddStringToObject(e, "event", "state");
            cJSON_AddStringToObject(e, "what", "port");
            cJSON_AddStringToObject(e, "object", "ktbr0/radio");
            cJSON_AddStringToObject(e, "state", port_state_name(s));
            control::publish(control::topic_state, e);
        }
    }
}

// ---- Bluedroid to the task ----

enum : EventBits_t {
    EV_SPP_INIT = 1 << 0,
    EV_OPEN = 1 << 1,
    EV_CLOSED = 1 << 2,
    EV_FAILED = 1 << 3,
    EV_UNCONGESTED = 1 << 4,
    EV_GATTC_REG = 1 << 5,
};
EventGroupHandle_t events;

struct GapEvent {
    // ble_found and ble_scan_done come from BLE scans; the rest are Classic.
    enum Type : uint8_t { found, disc_started, disc_stopped, remote_name, confirm, auth, ble_found, ble_scan_done } type;
    Addr addr;
    uint8_t addr_type = BLE_ADDR_TYPE_PUBLIC;
    bool ok = false;
    bool has_name = false;
    char name[32] = {};
    uint32_t cod = 0;
    int8_t rssi = 0;
    bool has_rssi = false;
    uint32_t number = 0;
};
QueueHandle_t gap_events = nullptr;

StreamBufferHandle_t from_radio;
std::atomic<uint32_t> spp_handle{0};
std::atomic<bool> congested{false};
Addr connect_addr; // the address an SDP lookup is for

struct WriteResult {
    esp_spp_status_t status;
    int len;
};
QueueHandle_t write_results;
SemaphoreHandle_t write_mutex;

void copy_name(char (&dst)[32], const uint8_t *src, size_t len)
{
    size_t n = std::min(len, sizeof(dst) - 1);
    memcpy(dst, src, n);
    dst[n] = '\0';
}

void gap_cb(esp_bt_gap_cb_event_t event, esp_bt_gap_cb_param_t *param)
{
    GapEvent e;
    switch (event) {
    case ESP_BT_GAP_DISC_RES_EVT: {
        e.type = GapEvent::found;
        memcpy(e.addr.b, param->disc_res.bda, sizeof(e.addr.b));
        for (int i = 0; i < param->disc_res.num_prop; i++) {
            esp_bt_gap_dev_prop_t &p = param->disc_res.prop[i];
            switch (p.type) {
            case ESP_BT_GAP_DEV_PROP_BDNAME:
                copy_name(e.name, static_cast<uint8_t *>(p.val), p.len);
                e.has_name = e.name[0] != '\0';
                break;
            case ESP_BT_GAP_DEV_PROP_COD:
                e.cod = *static_cast<uint32_t *>(p.val);
                break;
            case ESP_BT_GAP_DEV_PROP_RSSI:
                e.rssi = *static_cast<int8_t *>(p.val);
                e.has_rssi = true;
                break;
            case ESP_BT_GAP_DEV_PROP_EIR:
                if (!e.has_name) {
                    uint8_t len = 0;
                    uint8_t *eir = static_cast<uint8_t *>(p.val);
                    uint8_t *n = esp_bt_gap_resolve_eir_data(eir, ESP_BT_EIR_TYPE_CMPL_LOCAL_NAME, &len);
                    if (!n) {
                        n = esp_bt_gap_resolve_eir_data(eir, ESP_BT_EIR_TYPE_SHORT_LOCAL_NAME, &len);
                    }
                    if (n && len) {
                        copy_name(e.name, n, len);
                        e.has_name = true;
                    }
                }
                break;
            default:
                break;
            }
        }
        break;
    }
    case ESP_BT_GAP_DISC_STATE_CHANGED_EVT:
        e.type = param->disc_st_chg.state == ESP_BT_GAP_DISCOVERY_STARTED ? GapEvent::disc_started
                                                                          : GapEvent::disc_stopped;
        break;
    case ESP_BT_GAP_READ_REMOTE_NAME_EVT:
        e.type = GapEvent::remote_name;
        memcpy(e.addr.b, param->read_rmt_name.bda, sizeof(e.addr.b));
        e.ok = param->read_rmt_name.stat == ESP_BT_STATUS_SUCCESS;
        if (e.ok) {
            copy_name(e.name, param->read_rmt_name.rmt_name,
                      strnlen(reinterpret_cast<const char *>(param->read_rmt_name.rmt_name),
                              ESP_BT_GAP_MAX_BDNAME_LEN));
            e.has_name = e.name[0] != '\0';
        }
        break;
    case ESP_BT_GAP_CFM_REQ_EVT:
        e.type = GapEvent::confirm;
        memcpy(e.addr.b, param->cfm_req.bda, sizeof(e.addr.b));
        e.number = param->cfm_req.num_val;
        if (!pairing_now) {
            // Nobody asked for a pairing: refuse it.
            ESP_LOGW(TAG, "refused a pairing nobody asked for");
            esp_bt_gap_ssp_confirm_reply(param->cfm_req.bda, false);
            return;
        }
        break;
    case ESP_BT_GAP_PIN_REQ_EVT: {
        // Legacy pairing, if a radio ever asks: bb-link answers 0000.
        esp_bt_pin_code_t pin = {'0', '0', '0', '0'};
        esp_bt_gap_pin_reply(param->pin_req.bda, pairing_now, 4, pin);
        return;
    }
    case ESP_BT_GAP_AUTH_CMPL_EVT:
        e.type = GapEvent::auth;
        memcpy(e.addr.b, param->auth_cmpl.bda, sizeof(e.addr.b));
        e.ok = param->auth_cmpl.stat == ESP_BT_STATUS_SUCCESS;
        copy_name(e.name, param->auth_cmpl.device_name,
                  strnlen(reinterpret_cast<const char *>(param->auth_cmpl.device_name),
                          ESP_BT_GAP_MAX_BDNAME_LEN));
        e.has_name = e.name[0] != '\0';
        if (!e.ok) {
            ESP_LOGW(TAG, "authentication with %s failed, status %d", e.addr.str().c_str(),
                     param->auth_cmpl.stat);
        }
        break;
    case ESP_BT_GAP_MODE_CHG_EVT:
        ESP_LOGI(TAG, "link mode: %s", param->mode_chg.mode == ESP_BT_PM_MD_ACTIVE ? "active"
                                       : param->mode_chg.mode == ESP_BT_PM_MD_SNIFF ? "sniff"
                                                                                    : "other");
        return;
    default:
        return;
    }
    if (xQueueSend(gap_events, &e, 0) != pdTRUE) {
        ESP_LOGW(TAG, "GAP event queue full");
    }
}

void spp_cb(esp_spp_cb_event_t event, esp_spp_cb_param_t *p)
{
    switch (event) {
    case ESP_SPP_INIT_EVT:
        if (p->init.status == ESP_SPP_SUCCESS) {
            xEventGroupSetBits(events, EV_SPP_INIT);
        } else {
            running = false;
            errors::set(errors::Error::bluetooth,
                        "Bluetooth didn't start: SPP failed (status " + std::to_string(p->init.status) +
                            "). Nothing is bridged. Unplug the device and plug it in; if it happens "
                            "again, reinstall the firmware");
        }
        break;
    case ESP_SPP_DISCOVERY_COMP_EVT:
        if (p->disc_comp.status == ESP_SPP_SUCCESS && p->disc_comp.scn_num > 0) {
            ESP_LOGI(TAG, "SPP on channel %u", p->disc_comp.scn[0]);
            esp_spp_connect(ESP_SPP_SEC_AUTHENTICATE, ESP_SPP_ROLE_MASTER, p->disc_comp.scn[0],
                            connect_addr.raw());
        } else {
            ESP_LOGW(TAG, "SDP found no SPP, status %d", p->disc_comp.status);
            xEventGroupSetBits(events, EV_FAILED);
        }
        break;
    case ESP_SPP_CL_INIT_EVT:
        if (p->cl_init.status != ESP_SPP_SUCCESS) {
            ESP_LOGW(TAG, "SPP connect didn't start, status %d", p->cl_init.status);
            xEventGroupSetBits(events, EV_FAILED);
        }
        break;
    case ESP_SPP_OPEN_EVT:
        if (p->open.status == ESP_SPP_SUCCESS) {
            spp_handle = p->open.handle;
            congested = false;
            bta_sys_clear_policy(0, HCI_ENABLE_SNIFF_MODE, connect_addr.raw());
            xEventGroupSetBits(events, EV_OPEN | EV_UNCONGESTED);
        } else {
            ESP_LOGW(TAG, "SPP open failed, status %d", p->open.status);
            xEventGroupSetBits(events, EV_FAILED);
        }
        break;
    case ESP_SPP_CLOSE_EVT: {
        ESP_LOGI(TAG, "SPP closed, status %d%s", p->close.status, p->close.async ? ", by the radio" : "");
        spp_handle = 0;
        WriteResult r{ESP_SPP_FAILURE, 0};
        xQueueOverwrite(write_results, &r);
        xEventGroupSetBits(events, EV_CLOSED | EV_UNCONGESTED);
        break;
    }
    case ESP_SPP_DATA_IND_EVT: {
        size_t sent = xStreamBufferSend(from_radio, p->data_ind.data, p->data_ind.len, 0);
        if (sent < p->data_ind.len) {
            ESP_LOGW(TAG, "dropped %u bytes from the radio", (unsigned)(p->data_ind.len - sent));
        }
        break;
    }
    case ESP_SPP_WRITE_EVT: {
        congested = p->write.cong;
        if (p->write.cong) {
            xEventGroupClearBits(events, EV_UNCONGESTED);
        }
        WriteResult r{p->write.status, p->write.len};
        xQueueOverwrite(write_results, &r);
        break;
    }
    case ESP_SPP_CONG_EVT:
        congested = p->cong.cong;
        if (p->cong.cong) {
            xEventGroupClearBits(events, EV_UNCONGESTED);
        } else {
            xEventGroupSetBits(events, EV_UNCONGESTED);
        }
        break;
    default:
        break;
    }
}

// Writes everything, one esp_spp_write at a time, honouring congestion and
// partial writes as esp_spp_api.h asks.
bool spp_write_all(const uint8_t *data, size_t len)
{
    xSemaphoreTake(write_mutex, portMAX_DELAY);
    bool ok = true;
    int failures = 0;
    while (len > 0) {
        uint32_t handle = spp_handle;
        if (handle == 0) {
            ok = false;
            break;
        }
        if (congested && !(xEventGroupWaitBits(events, EV_UNCONGESTED, pdFALSE, pdFALSE,
                                               write_timeout) & EV_UNCONGESTED)) {
            ESP_LOGW(TAG, "write: still congested");
            ok = false;
            break;
        }
        xQueueReset(write_results);
        if (esp_spp_write(handle, static_cast<int>(len), const_cast<uint8_t *>(data)) != ESP_OK) {
            ok = false;
            break;
        }
        WriteResult r;
        if (xQueueReceive(write_results, &r, write_timeout) != pdTRUE) {
            ESP_LOGW(TAG, "write: no completion");
            ok = false;
            break;
        }
        if (r.status != ESP_SPP_SUCCESS) {
            if (spp_handle != 0 && ++failures <= 3) {
                continue; // congestion: try again
            }
            ok = false;
            break;
        }
        data += r.len;
        len -= r.len;
    }
    xSemaphoreGive(write_mutex);
    return ok;
}

// ---- Requests from the button and the control API ----

enum class ReqType : uint8_t {
    button_pair,
    button_cancel,
    button_answer, // flag: the radio shows the same number
    scan,
    scan_stop,
    pair,
    pair_reply,
    unpair,
    update,
    hold,
};

struct Request {
    ReqType type;
    control::Pending p;
    double ref = 0; // the request a scan_stop or pair_reply is about
    uint32_t seconds = 0;
    bool flag = false; // pair_reply: accept; update: non_disruptive
    char addr[18] = {};
    char model[16] = {};
    Hold hold = Hold::none;
};
QueueHandle_t requests;

// Set when a request changed what the link should be (a new radio, a hold,
// an update): the task stops what it's doing and starts over, without
// waiting out the retry delay.
bool restart = false;

void post(Request &r)
{
    if (!running) {
        control::reply_error(r.p, "failed", "Bluetooth didn't start");
        if (r.type == ReqType::button_pair) {
            pairing_now = false;
            ui::post(ui::Event::pairing_failed);
        }
        return;
    }
    if (xQueueSend(requests, &r, 0) != pdTRUE) {
        control::reply_error(r.p, "busy", "the radio has too many requests waiting");
        if (r.type == ReqType::button_pair) {
            pairing_now = false;
            ui::post(ui::Event::pairing_failed);
        }
    }
}

void handle(const Request &r);

// Takes requests until the timeout passes or one asks for a restart.
void serve_until(TickType_t timeout)
{
    TickType_t start = xTaskGetTickCount();
    for (;;) {
        TickType_t elapsed = xTaskGetTickCount() - start;
        if (restart || (timeout != portMAX_DELAY && elapsed >= timeout)) {
            return;
        }
        Request r;
        TickType_t wait = timeout == portMAX_DELAY ? portMAX_DELAY : timeout - elapsed;
        if (xQueueReceive(requests, &r, wait) == pdTRUE) {
            handle(r);
        }
    }
}

void serve_now()
{
    Request r;
    while (!restart && xQueueReceive(requests, &r, 0) == pdTRUE) {
        handle(r);
    }
}

// Inside a scan or a pairing, a request that would start another one is
// refused.
void refuse_busy(const Request &r)
{
    switch (r.type) {
    case ReqType::button_pair:
        ui::post(ui::Event::pairing_failed);
        break;
    case ReqType::hold:
        hold_state = r.hold;
        restart = true;
        control::reply_ok(r.p, cJSON_CreateObject());
        break;
    default:
        control::reply_error(r.p, "busy", "a Bluetooth scan or pairing is running; one runs at a time");
        break;
    }
}

// ---- BLE: scans, and the TD-H9's KISS service as a GATT client ----
//
// docs/td-h9-ble-kiss.md and spikes/atom-lite-ble-tnc. Bluedroid's GATT
// callbacks only feed the task, as the SPP ones do: the same events bits
// (only one of SPP and BLE is ever in use), the same stream of bytes from
// the radio.

constexpr uint16_t GATTC_APP_ID = 0x52;
constexpr uint16_t TDH9_SERVICE = 0xAF00;
constexpr uint16_t TDH9_TX = 0xAF01; // write without response
constexpr uint16_t TDH9_RX = 0xAF02; // notify
constexpr uint16_t NO_CONN = 0xFFFF;
// A connect attempt that lasted this long was waiting for the radio to
// advertise (Bluedroid gives up after 30 s), so the next starts at once.
constexpr TickType_t ble_waited = pdMS_TO_TICKS(20000);

std::atomic<esp_gatt_if_t> gattc_if{ESP_GATT_IF_NONE};
std::atomic<bool> ble_active{false}; // an attempt or a link: callbacks look at ble_target
std::atomic<bool> ble_up{false};     // ready: subscribed, frames can go
Addr ble_target;
std::atomic<uint16_t> ble_conn{NO_CONN};
std::atomic<uint16_t> ble_mtu{23};
uint16_t ble_svc_start = 0, ble_svc_end = 0, ble_tx = 0, ble_rx = 0;

// A BLE scan's length, until its parameters are set; 0 once it's stopped.
std::atomic<uint32_t> ble_scan_seconds{0};
// Addresses already reported in this scan, so the task isn't flooded with
// every advertisement (a busy room has dozens a second).
std::mutex scan_lock;
std::vector<Addr> scan_seen;

void ble_scan_start(uint32_t seconds)
{
    {
        std::lock_guard<std::mutex> g(scan_lock);
        scan_seen.clear();
    }
    ble_scan_seconds = seconds;
    esp_ble_scan_params_t sp = {};
    sp.scan_type = BLE_SCAN_TYPE_ACTIVE; // a name may be only in the scan response
    sp.own_addr_type = BLE_ADDR_TYPE_PUBLIC;
    sp.scan_filter_policy = BLE_SCAN_FILTER_ALLOW_ALL;
    sp.scan_interval = 0x50; // 50 ms
    sp.scan_window = 0x30;   // 30 ms
    sp.scan_duplicate = BLE_SCAN_DUPLICATE_DISABLE;
    if (esp_ble_gap_set_scan_params(&sp) != ESP_OK) {
        ESP_LOGW(TAG, "BLE scan didn't start");
        ble_scan_seconds = 0;
        GapEvent e;
        e.type = GapEvent::ble_scan_done;
        xQueueSend(gap_events, &e, 0);
    }
}

void ble_scan_stop()
{
    if (ble_scan_seconds.exchange(0) != 0) {
        return; // its parameters aren't set yet: it won't start
    }
    esp_ble_gap_stop_scanning();
}

std::string adv_name(esp_ble_gap_cb_param_t::ble_scan_result_evt_param &r)
{
    uint8_t len = 0;
    uint16_t total = r.adv_data_len + r.scan_rsp_len;
    uint8_t *n = esp_ble_resolve_adv_data_by_type(r.ble_adv, total, ESP_BLE_AD_TYPE_NAME_CMPL, &len);
    if (!n) {
        n = esp_ble_resolve_adv_data_by_type(r.ble_adv, total, ESP_BLE_AD_TYPE_NAME_SHORT, &len);
    }
    return n ? std::string(reinterpret_cast<char *>(n), len) : std::string();
}

void gattc_fail(const char *why)
{
    ESP_LOGW(TAG, "%s", why);
    xEventGroupSetBits(events, EV_FAILED);
    esp_ble_gap_disconnect(ble_target.raw());
}

bool ble_ours(uint16_t conn) { return ble_active && conn == ble_conn; }

void gattc_cb(esp_gattc_cb_event_t event, esp_gatt_if_t ifc, esp_ble_gattc_cb_param_t *p)
{
    if (event == ESP_GATTC_REG_EVT) {
        if (p->reg.status == ESP_GATT_OK) {
            gattc_if = ifc;
            xEventGroupSetBits(events, EV_GATTC_REG);
        } else {
            ESP_LOGE(TAG, "the GATT client didn't register, status %d", p->reg.status);
        }
        return;
    }
    if (ifc != gattc_if) {
        return;
    }
    switch (event) {
    case ESP_GATTC_OPEN_EVT:
        if (!ble_active || Addr::of(p->open.remote_bda) != ble_target) {
            break;
        }
        if (p->open.status != ESP_GATT_OK) {
            // After 30 s with no sign of the radio, or cancelled.
            ESP_LOGI(TAG, "no BLE connection, status 0x%x", p->open.status);
            xEventGroupSetBits(events, EV_FAILED);
            break;
        }
        ble_conn = p->open.conn_id;
        ble_svc_start = ble_svc_end = ble_tx = ble_rx = 0;
        esp_ble_gattc_send_mtu_req(gattc_if, ble_conn);
        break;
    case ESP_GATTC_CFG_MTU_EVT: {
        if (!ble_ours(p->cfg_mtu.conn_id)) {
            break;
        }
        ble_mtu = p->cfg_mtu.status == ESP_GATT_OK ? p->cfg_mtu.mtu : 23;
        esp_bt_uuid_t svc = {};
        svc.len = ESP_UUID_LEN_16;
        svc.uuid.uuid16 = TDH9_SERVICE;
        esp_ble_gattc_search_service(gattc_if, ble_conn, &svc);
        break;
    }
    case ESP_GATTC_SEARCH_RES_EVT:
        if (ble_ours(p->search_res.conn_id) && p->search_res.srvc_id.uuid.len == ESP_UUID_LEN_16 &&
            p->search_res.srvc_id.uuid.uuid.uuid16 == TDH9_SERVICE) {
            ble_svc_start = p->search_res.start_handle;
            ble_svc_end = p->search_res.end_handle;
        }
        break;
    case ESP_GATTC_SEARCH_CMPL_EVT: {
        if (!ble_ours(p->search_cmpl.conn_id)) {
            break;
        }
        if (!ble_svc_start) {
            gattc_fail("the radio has no 0xAF00 service: is it a TD-H9?");
            break;
        }
        esp_gattc_char_elem_t el;
        esp_bt_uuid_t u = {};
        u.len = ESP_UUID_LEN_16;
        for (uint16_t want : {TDH9_TX, TDH9_RX}) {
            uint16_t count = 1;
            u.uuid.uuid16 = want;
            if (esp_ble_gattc_get_char_by_uuid(gattc_if, ble_conn, ble_svc_start, ble_svc_end, u, &el, &count) ==
                    ESP_GATT_OK &&
                count > 0) {
                (want == TDH9_TX ? ble_tx : ble_rx) = el.char_handle;
            }
        }
        if (!ble_tx || !ble_rx) {
            gattc_fail("the radio's 0xAF00 service is missing 0xAF01 or 0xAF02");
            break;
        }
        esp_ble_gattc_register_for_notify(gattc_if, ble_target.raw(), ble_rx);
        break;
    }
    case ESP_GATTC_REG_FOR_NOTIFY_EVT: {
        if (!ble_active || ble_conn == NO_CONN || p->reg_for_notify.handle != ble_rx) {
            break;
        }
        esp_gattc_descr_elem_t d;
        uint16_t count = 1;
        esp_bt_uuid_t u = {};
        u.len = ESP_UUID_LEN_16;
        u.uuid.uuid16 = ESP_GATT_UUID_CHAR_CLIENT_CONFIG;
        if (p->reg_for_notify.status != ESP_GATT_OK ||
            esp_ble_gattc_get_descr_by_char_handle(gattc_if, ble_conn, ble_rx, u, &d, &count) != ESP_GATT_OK ||
            count == 0) {
            gattc_fail("couldn't subscribe to the radio's 0xAF02");
            break;
        }
        static uint8_t notify_on[2] = {0x01, 0x00};
        esp_ble_gattc_write_char_descr(gattc_if, ble_conn, d.handle, sizeof(notify_on), notify_on,
                                       ESP_GATT_WRITE_TYPE_RSP, ESP_GATT_AUTH_REQ_NONE);
        break;
    }
    case ESP_GATTC_WRITE_DESCR_EVT:
        if (!ble_ours(p->write.conn_id)) {
            break;
        }
        if (p->write.status != ESP_GATT_OK) {
            gattc_fail("the radio refused the subscription to 0xAF02");
            break;
        }
        ESP_LOGI(TAG, "BLE link ready, MTU %u", (unsigned)ble_mtu);
        congested = false;
        ble_up = true;
        xEventGroupSetBits(events, EV_OPEN | EV_UNCONGESTED);
        break;
    case ESP_GATTC_NOTIFY_EVT:
        if (ble_ours(p->notify.conn_id) && p->notify.handle == ble_rx) {
            size_t sent = xStreamBufferSend(from_radio, p->notify.value, p->notify.value_len, 0);
            if (sent < p->notify.value_len) {
                ESP_LOGW(TAG, "dropped %u bytes from the radio", (unsigned)(p->notify.value_len - sent));
            }
        }
        break;
    case ESP_GATTC_WRITE_CHAR_EVT:
        if (ble_ours(p->write.conn_id) && p->write.status != ESP_GATT_OK) {
            ESP_LOGW(TAG, "a write to the radio failed, status 0x%x", p->write.status);
        }
        break;
    case ESP_GATTC_CONGEST_EVT:
        if (ble_ours(p->congest.conn_id)) {
            congested = p->congest.congested;
            if (p->congest.congested) {
                xEventGroupClearBits(events, EV_UNCONGESTED);
            } else {
                xEventGroupSetBits(events, EV_UNCONGESTED);
            }
        }
        break;
    case ESP_GATTC_DISCONNECT_EVT:
        // Every GATT app hears about every link: only the radio's matters.
        // Before the link opened, the failed open says it instead.
        if (ble_active && Addr::of(p->disconnect.remote_bda) == ble_target && ble_conn != NO_CONN) {
            ESP_LOGI(TAG, "BLE link closed, reason 0x%x", p->disconnect.reason);
            ble_up = false;
            ble_conn = NO_CONN;
            xEventGroupSetBits(events, EV_CLOSED | EV_FAILED | EV_UNCONGESTED);
        }
        break;
    case ESP_GATTC_CANCEL_OPEN_EVT:
        if (ble_active) {
            xEventGroupSetBits(events, EV_FAILED);
        }
        break;
    default:
        break;
    }
}

// Ends an attempt or a link, and waits for it to end.
void ble_close()
{
    if (!ble_active) {
        return;
    }
    if (ble_conn != NO_CONN) {
        ESP_LOGI(TAG, "closing the link");
        esp_ble_gap_disconnect(ble_target.raw());
        xEventGroupWaitBits(events, EV_CLOSED, pdFALSE, pdFALSE, close_timeout);
    } else {
        esp_ble_gattc_cancel_open_params_t c = {};
        c.gattc_if = gattc_if;
        memcpy(c.remote_bda, ble_target.b, sizeof(c.remote_bda));
        xEventGroupClearBits(events, EV_FAILED);
        if (esp_ble_gattc_cancel_open(&c) == ESP_OK) {
            xEventGroupWaitBits(events, EV_FAILED, pdFALSE, pdFALSE, pdMS_TO_TICKS(2000));
        }
    }
    ble_up = false;
    ble_conn = NO_CONN;
    ble_active = false;
}

enum class BleOpen : uint8_t { opened, waited, failed, interrupted };

// Connects to a TD-H9 and subscribes. A direct connect waits for the radio
// to advertise, up to 30 s; a request arriving meanwhile ends the attempt,
// and stays in the queue for the task.
BleOpen ble_connect(const Record &r)
{
    if (gattc_if == ESP_GATT_IF_NONE) {
        return BleOpen::failed;
    }
    xEventGroupClearBits(events, EV_OPEN | EV_CLOSED | EV_FAILED);
    xStreamBufferReset(from_radio);
    ble_target = r.addr;
    ble_conn = NO_CONN;
    ble_mtu = 23;
    ble_active = true;
    TickType_t start = xTaskGetTickCount();
    Addr a = r.addr;
    if (esp_ble_gattc_open(gattc_if, a.raw(), static_cast<esp_ble_addr_type_t>(r.addr_type), true) != ESP_OK) {
        ble_active = false;
        return BleOpen::failed;
    }
    for (;;) {
        EventBits_t bits =
            xEventGroupWaitBits(events, EV_OPEN | EV_FAILED | EV_CLOSED, pdFALSE, pdFALSE, pdMS_TO_TICKS(100));
        if ((bits & EV_OPEN) && !(bits & EV_CLOSED)) {
            return BleOpen::opened;
        }
        if (bits & (EV_FAILED | EV_CLOSED)) {
            if (ble_conn != NO_CONN) {
                ble_close(); // set up went wrong on an open link
            } else {
                ble_active = false; // the open failed: nothing to close
            }
            return xTaskGetTickCount() - start >= ble_waited ? BleOpen::waited : BleOpen::failed;
        }
        Request q;
        if (xQueuePeek(requests, &q, 0) == pdTRUE) {
            ble_close(); // a search or a scan gets the radio to itself
            return BleOpen::interrupted;
        }
    }
}

// Writes one whole frame, FENDs included: the TD-H9 doesn't put a frame
// back together across writes.
bool ble_write(const uint8_t *data, size_t len)
{
    if (!ble_up) {
        return false;
    }
    size_t most = ble_mtu - 3;
    if (len > most) {
        ESP_LOGW(TAG, "a %u-byte frame doesn't fit in one write to the radio (%u at most): dropped",
                 (unsigned)len, (unsigned)most);
        return false;
    }
    if (congested && !(xEventGroupWaitBits(events, EV_UNCONGESTED, pdFALSE, pdFALSE, write_timeout) &
                       EV_UNCONGESTED)) {
        ESP_LOGW(TAG, "write: still congested");
        return false;
    }
    return esp_ble_gattc_write_char(gattc_if, ble_conn, ble_tx, len, const_cast<uint8_t *>(data),
                                    ESP_GATT_WRITE_TYPE_NO_RSP, ESP_GATT_AUTH_REQ_NONE) == ESP_OK;
}

// ---- The link ----

void drop_link()
{
    if (ble_active) {
        ble_close();
        return;
    }
    if (spp_handle != 0) {
        ESP_LOGI(TAG, "closing the link");
        esp_spp_disconnect(spp_handle);
        xEventGroupWaitBits(events, EV_CLOSED, pdFALSE, pdFALSE, close_timeout);
    }
}

// Pages addr, looks up its SPP channel and connects, with authentication.
// Returns true once the link is open.
bool connect_to(const Addr &addr, TickType_t timeout)
{
    xEventGroupClearBits(events, EV_OPEN | EV_CLOSED | EV_FAILED);
    connect_addr = addr;
    if (esp_spp_start_discovery(connect_addr.raw()) != ESP_OK) {
        return false;
    }
    EventBits_t bits = xEventGroupWaitBits(events, EV_OPEN | EV_FAILED | EV_CLOSED, pdFALSE, pdFALSE, timeout);
    if (bits & EV_OPEN) {
        return true;
    }
    if (!(bits & (EV_FAILED | EV_CLOSED))) {
        ESP_LOGW(TAG, "no connection within %u s", (unsigned)pdTICKS_TO_MS(timeout) / 1000);
    }
    return false;
}

// KISS init for the TH-D74/D75 (spikes/FINDINGS.md, bb-link source review):
// BT (silence means the TNC is already in KISS mode: leave it and ask
// again), ID, TN, then TN 2,<band>.
std::string pending_rx;

bool read_line(std::string &line, TickType_t timeout)
{
    TickType_t start = xTaskGetTickCount();
    for (;;) {
        size_t cr = pending_rx.find('\r');
        if (cr != std::string::npos) {
            line = pending_rx.substr(0, cr);
            pending_rx.erase(0, cr + 1);
            return true;
        }
        TickType_t elapsed = xTaskGetTickCount() - start;
        if (elapsed >= timeout || spp_handle == 0) {
            return false;
        }
        uint8_t buf[64];
        size_t n = xStreamBufferReceive(from_radio, buf, sizeof(buf), timeout - elapsed);
        pending_rx.append(reinterpret_cast<char *>(buf), n);
    }
}

bool command(const std::string &cmd, std::string &reply)
{
    std::string wire = cmd + "\r";
    for (int attempt = 0; attempt < 3; attempt++) {
        if (attempt > 0) {
            vTaskDelay(pdMS_TO_TICKS(200));
        }
        pending_rx.clear();
        xStreamBufferReset(from_radio);
        if (!spp_write_all(reinterpret_cast<const uint8_t *>(wire.data()), wire.size())) {
            return false;
        }
        if (!read_line(reply, reply_timeout)) {
            return false; // silence: no point retrying
        }
        ESP_LOGI(TAG, "init: %s -> %s", cmd.c_str(), reply.c_str());
        if (reply.compare(0, 2, cmd, 0, 2) == 0) {
            return true;
        }
    }
    return false;
}

bool kiss_init(const Model &model)
{
    pending_rx.clear();
    if (!model.kiss_init) {
        return true;
    }
    std::string reply;
    if (!command("BT", reply)) {
        ESP_LOGI(TAG, "init: no answer to BT, so the TNC is in KISS mode: leaving it");
        static const uint8_t kiss_return[] = {0xC0, 0xFF, 0xC0};
        if (!spp_write_all(kiss_return, sizeof(kiss_return))) {
            return false;
        }
        vTaskDelay(pdMS_TO_TICKS(300));
        if (!command("BT", reply)) {
            ESP_LOGE(TAG, "init: still no answer to BT");
            return false;
        }
    }
    command("ID", reply);
    if (!command("TN", reply)) {
        return false;
    }
    int mode = 0;
    int band = 0;
    if (sscanf(reply.c_str(), "TN %d,%d", &mode, &band) != 2) {
        ESP_LOGE(TAG, "init: can't read '%s'", reply.c_str());
        return false;
    }
    if (!command("TN 2," + std::to_string(band), reply)) {
        return false;
    }
    ESP_LOGI(TAG, "init: KISS mode on band %c", band == 0 ? 'A' : 'B');
    pending_rx.clear(); // the KISS stream starts here
    return true;
}

kiss::Splitter splitter;

void frame_from_radio(const uint8_t *frame, size_t len)
{
    counters.frames_down++;
    if (activity::wanted()) {
        activity::add("ktbr0/radio", activity::Dir::rx, len);
    }
    if (control::subscribed(control::topic_frames)) {
        cJSON *e = cJSON_CreateObject();
        cJSON_AddStringToObject(e, "event", "frame");
        cJSON_AddStringToObject(e, "object", "ktbr0/radio");
        cJSON_AddStringToObject(e, "dir", "rx");
        cJSON_AddStringToObject(e, "kiss", kiss::hex(frame, len).c_str());
        if (cJSON *d = kiss::decode(frame, len)) {
            cJSON_AddItemToObject(e, "decoded", d);
        }
        control::publish(control::topic_frames, e);
    }
    if (phones::send_frame(frame, len) > 0) {
        ui::post(ui::Event::frame_to_phone);
    }
}

// Frames from phones on their way to the radio: each message is the
// session id's length (1 byte), the session id, then the frame (stuffed,
// without FENDs).
MessageBufferHandle_t to_radio;

void frame_to_radio_event(const std::string &session, const uint8_t *frame, size_t len)
{
    cJSON *e = cJSON_CreateObject();
    cJSON_AddStringToObject(e, "event", "frame");
    cJSON_AddStringToObject(e, "object", "ktbr0/radio");
    cJSON_AddStringToObject(e, "dir", "tx");
    cJSON_AddStringToObject(e, "session", session.c_str());
    cJSON_AddStringToObject(e, "kiss", kiss::hex(frame, len).c_str());
    if (cJSON *d = kiss::decode(frame, len)) {
        cJSON_AddItemToObject(e, "decoded", d);
    }
    control::publish(control::topic_frames, e);
}

// Writes phones' frames to the radio, whole. A frame that arrives while
// the radio isn't bridging is dropped, as the daemon does.
void writer_task(void *)
{
    static uint8_t msg[1 + 64 + kiss::Splitter::max_frame];
    static uint8_t wire[kiss::Splitter::max_frame + 2];
    for (;;) {
        size_t n = xMessageBufferReceive(to_radio, msg, sizeof(msg), portMAX_DELAY);
        if (n < 1 || n < 1u + msg[0]) {
            continue;
        }
        std::string session(reinterpret_cast<char *>(msg + 1), msg[0]);
        const uint8_t *frame = msg + 1 + msg[0];
        size_t len = n - 1 - msg[0];
        if (port_state != PortState::connected) {
            counters.dropped++;
            continue;
        }
        wire[0] = kiss::FEND;
        memcpy(wire + 1, frame, len);
        wire[len + 1] = kiss::FEND;
        if (!(ble_up ? ble_write(wire, len + 2) : spp_write_all(wire, len + 2))) {
            counters.dropped++;
            continue;
        }
        counters.frames_up++;
        if (activity::wanted()) {
            activity::add("ktbr0/radio", activity::Dir::tx, len);
        }
        if (control::subscribed(control::topic_frames)) {
            frame_to_radio_event(session, frame, len);
        }
        ui::post(ui::Event::frame_to_radio);
    }
}

// Bridging: until the link closes or a request restarts the task.
void bridge()
{
    splitter.reset();
    uint8_t buf[256];
    while (!(xEventGroupGetBits(events) & EV_CLOSED)) {
        size_t n = xStreamBufferReceive(from_radio, buf, sizeof(buf), pdMS_TO_TICKS(100));
        if (n > 0) {
            splitter.feed(buf, n, frame_from_radio);
        }
        serve_now();
        if (restart) {
            drop_link();
            return;
        }
    }
}

// ---- Searching, scanning and pairing ----

struct Found {
    Addr addr;
    std::string name;
    uint32_t cod = 0;
    int8_t rssi = 0;
    bool has_rssi = false;
    bool ble = false; // found by a BLE scan
    uint8_t addr_type = BLE_ADDR_TYPE_PUBLIC;
};

// Asks a device its name. Empty if it doesn't answer.
std::string read_name(const Addr &addr)
{
    Addr a = addr;
    if (esp_bt_gap_read_remote_name(a.raw()) != ESP_OK) {
        return "";
    }
    TickType_t start = xTaskGetTickCount();
    GapEvent e;
    while (xTaskGetTickCount() - start < name_timeout) {
        if (xQueueReceive(gap_events, &e, pdMS_TO_TICKS(100)) == pdTRUE && e.type == GapEvent::remote_name &&
            e.addr == addr) {
            return e.ok ? e.name : "";
        }
    }
    return "";
}

void stop_discovery()
{
    if (esp_bt_gap_cancel_discovery() == ESP_OK) {
        GapEvent e;
        TickType_t start = xTaskGetTickCount();
        while (xTaskGetTickCount() - start < pdMS_TO_TICKS(2000)) {
            if (xQueueReceive(gap_events, &e, pdMS_TO_TICKS(100)) == pdTRUE &&
                e.type == GapEvent::disc_stopped) {
                return;
            }
        }
    }
}

uint8_t inquiry_len(TickType_t left)
{
    uint32_t units = pdTICKS_TO_MS(left) / 1280;
    return static_cast<uint8_t>(std::clamp<uint32_t>(units, ESP_BT_GAP_MIN_INQ_LEN, ESP_BT_GAP_MAX_INQ_LEN));
}

enum class SearchResult : uint8_t { found, none, cancelled };

// The button's search: the first TH-D74 or TH-D75 in pairing mode, or
// the first TD-H9, within the deadline. Kenwood radios report a phone's
// Class of Device, so it goes by name; one with the right class but no
// name in its inquiry response is asked for its name. A TD-H9 advertises
// over BLE whenever nothing is connected to it, so a BLE scan runs
// alongside the inquiries.
SearchResult search_both(TickType_t deadline, Found &out);

SearchResult search(TickType_t deadline, Found &out)
{
    xQueueReset(gap_events);
    ble_scan_start(pdTICKS_TO_MS(deadline - xTaskGetTickCount()) / 1000 + 1);
    SearchResult res = search_both(deadline, out);
    ble_scan_stop();
    return res;
}

SearchResult search_both(TickType_t deadline, Found &out)
{
    std::vector<Addr> unnamed;
    std::vector<Addr> asked;
    while (xTaskGetTickCount() < deadline) {
        if (esp_bt_gap_start_discovery(ESP_BT_INQ_MODE_GENERAL_INQUIRY,
                                       inquiry_len(deadline - xTaskGetTickCount()), 0) != ESP_OK) {
            ESP_LOGW(TAG, "couldn't start a search");
            return SearchResult::none;
        }
        bool stopped = false;
        while (!stopped && xTaskGetTickCount() < deadline) {
            Request r;
            while (xQueueReceive(requests, &r, 0) == pdTRUE) {
                if (r.type == ReqType::button_cancel) {
                    stop_discovery();
                    return SearchResult::cancelled;
                }
                refuse_busy(r);
            }
            GapEvent e;
            if (xQueueReceive(gap_events, &e, pdMS_TO_TICKS(100)) != pdTRUE) {
                continue;
            }
            if (e.type == GapEvent::disc_stopped) {
                stopped = true;
            } else if (e.type == GapEvent::ble_found) {
                if (model_for_name(e.name, true)) {
                    stop_discovery();
                    out.addr = e.addr;
                    out.name = e.name;
                    out.ble = true;
                    out.addr_type = e.addr_type;
                    return SearchResult::found;
                }
            } else if (e.type == GapEvent::found) {
                if (e.has_name && model_for_name(e.name, false)) {
                    stop_discovery();
                    out.addr = e.addr;
                    out.name = e.name;
                    return SearchResult::found;
                }
                if (!e.has_name && e.cod == cod_kenwood &&
                    std::find(unnamed.begin(), unnamed.end(), e.addr) == unnamed.end() &&
                    std::find(asked.begin(), asked.end(), e.addr) == asked.end()) {
                    unnamed.push_back(e.addr);
                }
            }
        }
        if (!stopped) {
            stop_discovery();
        }
        for (const Addr &a : unnamed) {
            asked.push_back(a);
            std::string name = read_name(a);
            ESP_LOGI(TAG, "%s is called \"%s\"", a.str().c_str(), name.c_str());
            if (model_for_name(name, false)) {
                out.addr = a;
                out.name = name;
                return SearchResult::found;
            }
        }
        unnamed.clear();
    }
    return SearchResult::none;
}

cJSON *device_json(const Found &f, const Record &r)
{
    cJSON *d = cJSON_CreateObject();
    cJSON_AddStringToObject(d, "address", f.addr.str().c_str());
    if (!f.name.empty()) {
        cJSON_AddStringToObject(d, "name", f.name.c_str());
    }
    if (f.has_rssi) {
        cJSON_AddNumberToObject(d, "rssi", f.rssi);
    }
    cJSON_AddBoolToObject(d, "paired", !f.ble && bonded(f.addr));
    if (const Model *m = model_for_name(f.name, f.ble)) {
        cJSON_AddStringToObject(d, "known_tnc", m->radio);
        cJSON_AddStringToObject(d, "model", m->name);
    }
    if (r.present && r.addr == f.addr) {
        cJSON_AddStringToObject(d, "tnc", "radio");
    }
    return d;
}

// Addresses and their types from LE scans, for tnc.update, which is given
// only an address. Only the task touches it.
std::vector<std::pair<Addr, uint8_t>> le_types;

uint8_t le_type(const Addr &a)
{
    for (auto &t : le_types) {
        if (t.first == a) {
            return t.second;
        }
    }
    return BLE_ADDR_TYPE_PUBLIC;
}

// bluetooth.scan: everything that answers, with found events as they come.
// With req.flag, over BLE: every device that advertises a name.
void do_scan(const Request &req)
{
    bool le = req.flag;
    busy_now = true;
    TickType_t deadline = xTaskGetTickCount() + pdMS_TO_TICKS(req.seconds * 1000);
    // A button pairing waiting behind this scan was refused: its LED state
    // ends here.
    auto refuse = [](const Request &q) {
        if (q.type == ReqType::button_pair) {
            pairing_now = false;
        }
        refuse_busy(q);
    };
    std::vector<Found> found;
    xQueueReset(gap_events);
    if (le) {
        ble_scan_start(req.seconds);
    } else if (esp_bt_gap_start_discovery(ESP_BT_INQ_MODE_GENERAL_INQUIRY,
                                          inquiry_len(pdMS_TO_TICKS(req.seconds * 1000)), 0) != ESP_OK) {
        busy_now = false;
        control::reply_error(req.p, "failed", "couldn't start the scan");
        return;
    }
    auto stop = [le]() {
        if (le) {
            ble_scan_stop();
        } else {
            stop_discovery();
        }
    };
    Record r = record();
    bool stopped = false;
    while (!stopped) {
        if (xTaskGetTickCount() > deadline + pdMS_TO_TICKS(2000)) {
            stop();
            break;
        }
        Request q;
        while (xQueueReceive(requests, &q, 0) == pdTRUE) {
            if (q.type == ReqType::scan_stop && q.ref == req.p.id) {
                stop();
                stopped = true;
                control::reply_ok(q.p, cJSON_CreateObject());
            } else if (q.type == ReqType::scan_stop) {
                control::reply_error(q.p, "not_found", "no scan with that request id");
            } else {
                refuse(q);
            }
        }
        GapEvent e;
        if (stopped || xQueueReceive(gap_events, &e, pdMS_TO_TICKS(100)) != pdTRUE) {
            continue;
        }
        if (e.type == (le ? GapEvent::ble_scan_done : GapEvent::disc_stopped)) {
            stopped = true;
        } else if (e.type == (le ? GapEvent::ble_found : GapEvent::found)) {
            if (le) {
                auto t = std::find_if(le_types.begin(), le_types.end(), [&](auto &p) { return p.first == e.addr; });
                if (t == le_types.end()) {
                    if (le_types.size() >= 64) {
                        le_types.erase(le_types.begin());
                    }
                    le_types.emplace_back(e.addr, e.addr_type);
                } else {
                    t->second = e.addr_type;
                }
            }
            auto it = std::find_if(found.begin(), found.end(), [&](const Found &f) { return f.addr == e.addr; });
            bool news = false;
            if (it == found.end()) {
                found.push_back(Found{e.addr, e.has_name ? e.name : "", e.cod, e.rssi, e.has_rssi, le, e.addr_type});
                it = found.end() - 1;
                news = true;
            } else if (e.has_name && it->name.empty()) {
                it->name = e.name;
                news = true;
            }
            if (news) {
                cJSON *ev = cJSON_CreateObject();
                cJSON_AddStringToObject(ev, "event", "found");
                cJSON_AddNumberToObject(ev, "request", req.p.id);
                cJSON_AddItemToObject(ev, "device", device_json(*it, r));
                control::send_event(req.p.conn, ev);
            }
        }
    }
    // Known radios first
    std::stable_sort(found.begin(), found.end(), [](const Found &a, const Found &b) {
        return model_for_name(a.name, a.ble) && !model_for_name(b.name, b.ble);
    });
    cJSON *list = cJSON_CreateArray();
    for (const Found &f : found) {
        cJSON_AddItemToArray(list, device_json(f, r));
    }
    busy_now = false;
    control::reply_ok(req.p, list);
}

void pairing_event(const control::Pending &p, const char *step, const char *key = nullptr,
                   const std::string &value = "")
{
    if (!p.valid()) {
        return;
    }
    cJSON *e = cJSON_CreateObject();
    cJSON_AddStringToObject(e, "event", "pairing");
    cJSON_AddNumberToObject(e, "request", p.id);
    cJSON_AddStringToObject(e, "step", step);
    if (key) {
        cJSON_AddStringToObject(e, key, value.c_str());
    }
    control::send_event(p.conn, e);
}

// The button found a TD-H9: it becomes the radio. It isn't paired: it
// takes a connection from anything, so the bridge only remembers its
// address. A Classic radio paired before is forgotten.
void select_ble(const Found &target)
{
    Record old = record();
    Record r;
    r.present = true;
    r.addr = target.addr;
    r.addr_type = target.addr_type;
    r.name = target.name;
    r.model = model_for_name(target.name, true)->name;
    if (save_record(r)) {
        if (old.present && !old.ble() && bonded(old.addr)) {
            esp_bt_gap_remove_bond_device(old.addr.raw());
        }
        ESP_LOGI(TAG, "selected %s (%s), model %s", r.name.c_str(), r.addr.str().c_str(), r.model.c_str());
        ui::post(ui::Event::pairing_succeeded);
    } else {
        ui::post(ui::Event::pairing_failed);
    }
}

// Pairs with a radio and makes it the radio, keeping the old one until the
// new one has paired. From the button (search, confirm at once) or from
// bluetooth.pair (a given address, the management app confirms). The
// button selects a TD-H9 instead if it finds one first.
void do_pair(const Request &req)
{
    bool button = req.type == ReqType::button_pair;
    const control::Pending &p = req.p;
    pairing_now = true;
    busy_now = true;
    restart = true; // whatever happens, start the link over afterwards
    drop_link();
    set_port(PortState::idle, "pairing a radio");

    Found target;
    const char *failure = nullptr;
    bool cancelled = false;
    set_pair_stage(PairStage::searching, pairing_seconds);
    if (button) {
        ESP_LOGI(TAG, "pairing: searching for a TH-D74 or TH-D75 in pairing mode, or a TD-H9");
        switch (search(xTaskGetTickCount() + pdMS_TO_TICKS(pairing_seconds * 1000), target)) {
        case SearchResult::found:
            ESP_LOGI(TAG, "pairing: found %s, %s", target.name.c_str(), target.addr.str().c_str());
            if (target.ble) {
                select_ble(target);
                set_pair_stage(PairStage::none, 0);
                pairing_now = false;
                busy_now = false;
                return;
            }
            break;
        case SearchResult::cancelled:
            cancelled = true;
            break;
        case SearchResult::none:
            failure = "no TH-D74 or TH-D75 in pairing mode, and no TD-H9, was found";
            break;
        }
    } else {
        Addr::parse(req.addr, target.addr);
        pairing_event(p, "searching");
        target.name = read_name(target.addr);
        if (target.name.empty()) {
            failure = "the device didn't answer: is it on, nearby and in pairing mode?";
        }
    }

    bool authed = false;
    bool opened = false;
    // A radio we're bonded with (pairing the radio again) is tried with
    // that bond first, so cancelling leaves it as it was. If the radio
    // has forgotten us, Bluedroid usually pairs afresh by itself on the
    // same connection; if that attempt fails instead, the bond goes and
    // the pairing starts once more from scratch.
    bool keep_bond = !failure && !cancelled && bonded(target.addr);
    if (keep_bond) {
        ESP_LOGI(TAG, "pairing: already bonded with %s, trying that bond first", target.addr.str().c_str());
    }
    for (bool again = !failure && !cancelled; again;) {
        again = false;
        bool auth_failed = false;
        pairing_event(p, "pairing", "name", target.name);
        xQueueReset(gap_events);
        xEventGroupClearBits(events, EV_OPEN | EV_CLOSED | EV_FAILED);
        connect_addr = target.addr;
        if (esp_spp_start_discovery(connect_addr.raw()) != ESP_OK) {
            failure = "couldn't start pairing";
        }
        TickType_t deadline = xTaskGetTickCount() + pair_timeout;
        bool confirm_pending = false;
        TickType_t confirm_deadline = 0;
        std::string number;
        TickType_t confirm_at = 0;
        // The radio gives about as long as confirm_timeout from showing
        // the number.
        auto confirm_left_s = [&]() -> uint32_t {
            TickType_t gone = xTaskGetTickCount() - confirm_at;
            return gone < confirm_timeout ? pdTICKS_TO_MS(confirm_timeout - gone) / 1000 : 0;
        };
        set_pair_stage(PairStage::pairing, pdTICKS_TO_MS(pair_timeout) / 1000, target.name);
        while (!failure && !cancelled && xTaskGetTickCount() < deadline) {
            Request q;
            while (xQueueReceive(requests, &q, 0) == pdTRUE) {
                if (button && q.type == ReqType::button_cancel) {
                    cancelled = true;
                } else if (button && q.type == ReqType::button_answer) {
                    if (confirm_pending) {
                        esp_bt_gap_ssp_confirm_reply(target.addr.raw(), q.flag);
                        confirm_pending = false;
                        if (q.flag) {
                            ESP_LOGI(TAG, "pairing: the number is the same: press OK on the radio");
                            set_pair_stage(PairStage::press_ok, confirm_left_s(), target.name, number);
                        } else {
                            ESP_LOGI(TAG, "pairing: the radio shows another number");
                            cancelled = true;
                        }
                    }
                } else if (!button && q.type == ReqType::pair_reply && q.ref == p.id) {
                    if (confirm_pending) {
                        esp_bt_gap_ssp_confirm_reply(target.addr.raw(), q.flag);
                        confirm_pending = false;
                        cancelled = !q.flag;
                        control::reply_ok(q.p, cJSON_CreateObject());
                    } else {
                        control::reply_error(q.p, "failed", "the pairing isn't waiting for a reply");
                    }
                } else if (q.type == ReqType::pair_reply) {
                    control::reply_error(q.p, "not_found", "no pairing with that request id");
                } else {
                    refuse_busy(q);
                }
            }
            if (cancelled) {
                break;
            }
            if (confirm_pending && xTaskGetTickCount() > confirm_deadline) {
                esp_bt_gap_ssp_confirm_reply(target.addr.raw(), false);
                confirm_pending = false;
                failure = "no reply to the confirmation";
                break;
            }
            GapEvent e;
            if (xQueueReceive(gap_events, &e, pdMS_TO_TICKS(100)) == pdTRUE && e.addr == target.addr) {
                if (e.type == GapEvent::confirm) {
                    char num[8];
                    snprintf(num, sizeof(num), "%06" PRIu32, e.number);
                    number = num;
                    confirm_at = xTaskGetTickCount();
                    if (button && !board::screen) {
                        ESP_LOGI(TAG, "pairing: the radio shows %s: press OK on it", num);
                        esp_bt_gap_ssp_confirm_reply(target.addr.raw(), true);
                        set_pair_stage(PairStage::press_ok, confirm_left_s(), target.name, number);
                    } else if (button) {
                        ESP_LOGI(TAG, "pairing: does the radio show %s?", num);
                        confirm_pending = true;
                        confirm_deadline = xTaskGetTickCount() + confirm_timeout;
                        set_pair_stage(PairStage::compare, confirm_left_s(), target.name, number);
                    } else {
                        pairing_event(p, "confirm", "passkey", num);
                        confirm_pending = true;
                        confirm_deadline = xTaskGetTickCount() + confirm_timeout;
                    }
                } else if (e.type == GapEvent::auth) {
                    if (e.ok) {
                        authed = true;
                        if (target.name.empty() && e.has_name) {
                            target.name = e.name;
                        }
                    } else {
                        auth_failed = true;
                        failure = "the pairing failed: press OK on the radio within 30 s of the number "
                                  "appearing, and dismiss any old prompt before trying again";
                    }
                }
            }
            EventBits_t bits = xEventGroupGetBits(events);
            if (bits & EV_OPEN) {
                opened = true;
                if (keep_bond && !authed) {
                    // The link opens only once authenticated: the bond
                    // still works, with no new pairing to report.
                    ESP_LOGI(TAG, "pairing: the radio still knows us");
                    authed = true;
                }
            }
            if (authed && (opened || (bits & (EV_FAILED | EV_CLOSED)))) {
                break; // paired; the link is open or won't be
            }
            if (!authed && (bits & (EV_FAILED | EV_CLOSED))) {
                failure = "the radio didn't accept the connection: is it in pairing mode?";
            }
        }
        if (spp_handle != 0) {
            drop_link(); // the task connects afresh
        }
        if (keep_bond && auth_failed && !authed && !cancelled) {
            // Only when the radio turned the bond down: a link that just
            // failed is no reason to lose it.
            ESP_LOGI(TAG, "pairing: the radio turned the bond down, pairing afresh");
            keep_bond = false;
            failure = nullptr;
            if (bonded(target.addr)) {
                esp_bt_gap_remove_bond_device(target.addr.raw());
            }
            vTaskDelay(pdMS_TO_TICKS(500)); // let the failed link go
            again = true;
            continue;
        }
        if (!authed && !failure && !cancelled) {
            failure = "the pairing timed out";
        }
    }

    if (authed && !cancelled) {
        Record old = record();
        Record r;
        r.present = true;
        r.addr = target.addr;
        r.name = target.name;
        const Model *m = model_for_name(target.name, false);
        r.model = m ? m->name : (old.present && old.addr == target.addr ? old.model : "generic");
        if (save_record(r)) {
            if (old.present && old.addr != r.addr) {
                esp_bt_gap_remove_bond_device(old.addr.raw());
            }
            ESP_LOGI(TAG, "pairing: paired with %s (%s), model %s", r.name.c_str(), r.addr.str().c_str(),
                     r.model.c_str());
            ui::post(ui::Event::pairing_succeeded);
            cJSON *res = cJSON_CreateObject();
            cJSON_AddStringToObject(res, "address", r.addr.str().c_str());
            cJSON_AddStringToObject(res, "adapter", "builtin");
            cJSON_AddBoolToObject(res, "paired", true);
            control::reply_ok(p, res);
        } else {
            esp_bt_gap_remove_bond_device(target.addr.raw());
            ui::post(ui::Event::pairing_failed);
            control::reply_error(p, "internal", "paired, but couldn't save the radio");
        }
    } else {
        Record current = record();
        if (authed && !(current.present && current.addr == target.addr)) {
            // Cancelled after the radio's OK: forget the new radio, but
            // never the radio we have.
            esp_bt_gap_remove_bond_device(target.addr.raw());
        }
        const char *why = cancelled ? "cancelled" : failure;
        ESP_LOGI(TAG, "pairing: %s", why);
        ui::post(ui::Event::pairing_failed);
        control::reply_error(p, "failed", why);
    }
    set_pair_stage(PairStage::none, 0);
    pairing_now = false;
    busy_now = false;
}

void do_unpair(const Request &req)
{
    Addr a;
    Record r = record();
    if (req.addr[0] == '\0') {
        if (!r.present) {
            control::reply_error(req.p, "not_found", "no radio is paired");
            return;
        }
        a = r.addr;
    } else {
        Addr::parse(req.addr, a);
    }
    if (r.present && r.addr == a) {
        restart = true;
        drop_link();
        save_record(Record{});
    }
    if (bonded(a)) {
        esp_bt_gap_remove_bond_device(a.raw());
    }
    ESP_LOGI(TAG, "unpaired %s", a.str().c_str());
    control::reply_ok(req.p, cJSON_CreateObject());
}

void do_update(const Request &req)
{
    Record r = record();
    Record next = r;
    if (req.model[0]) {
        next.model = req.model;
    }
    if (req.addr[0]) {
        Addr a;
        Addr::parse(req.addr, a);
        if (!r.present || a != r.addr) {
            next.addr = a;
            next.present = true;
            next.name.clear();
        }
    }
    if (next.ble()) {
        next.addr_type = le_type(next.addr);
    }
    bool changed = next.present != r.present || next.addr != r.addr || next.model != r.model;
    bool link_up = port_state == PortState::connected;
    bool disruptive = changed && link_up && phones() > 0;

    cJSON *res = cJSON_CreateObject();
    cJSON_AddStringToObject(res, "id", "radio");
    if (disruptive && req.flag) {
        cJSON_Delete(res);
        control::reply_error(req.p, "reload_would_disrupt",
                             "the change drops the radio link while phones are using it");
        return;
    }
    if (changed && !save_record(next)) {
        cJSON_Delete(res);
        control::reply_error(req.p, "internal", "couldn't save the radio");
        return;
    }
    if (changed) {
        restart = true;
        drop_link();
    }
    cJSON_AddBoolToObject(res, "applied", true);
    cJSON *plan = cJSON_AddObjectToObject(res, "plan");
    cJSON_AddArrayToObject(plan, "global");
    cJSON_AddArrayToObject(plan, "bridges");
    cJSON_AddBoolToObject(res, "disruptive", disruptive);
    cJSON *disruptions = cJSON_AddArrayToObject(res, "disruptions");
    if (disruptive) {
        cJSON_AddItemToArray(disruptions, cJSON_CreateString("drops the radio link in use by ktbr0"));
    }
    cJSON_AddArrayToObject(res, "warnings");
    control::reply_ok(req.p, res);
}

void handle(const Request &r)
{
    switch (r.type) {
    case ReqType::button_pair:
    case ReqType::pair:
        do_pair(r);
        break;
    case ReqType::button_cancel:
    case ReqType::button_answer:
        break; // nothing to cancel or answer
    case ReqType::scan:
        do_scan(r);
        break;
    case ReqType::scan_stop:
        control::reply_error(r.p, "not_found", "no scan with that request id");
        break;
    case ReqType::pair_reply:
        control::reply_error(r.p, "not_found", "no pairing with that request id");
        break;
    case ReqType::unpair:
        do_unpair(r);
        break;
    case ReqType::update:
        do_update(r);
        break;
    case ReqType::hold:
        if (hold_state.exchange(r.hold) != r.hold) {
            restart = true;
            if (r.hold == Hold::down) {
                drop_link();
            }
        }
        control::reply_ok(r.p, cJSON_CreateObject());
        break;
    }
}

// ---- The task ----

bool started(const char *what, esp_err_t err)
{
    if (err == ESP_OK) {
        return true;
    }
    errors::set(errors::Error::bluetooth,
                std::string("Bluetooth didn't start: ") + what + " failed (" + esp_err_to_name(err) +
                    "). Nothing is bridged. Unplug the device and plug it in; if it happens "
                    "again, reinstall the firmware");
    return false;
}

void radio_task(void *)
{
    xEventGroupWaitBits(events, EV_SPP_INIT, pdFALSE, pdFALSE, portMAX_DELAY);
    if (!(xEventGroupWaitBits(events, EV_GATTC_REG, pdFALSE, pdFALSE, pdMS_TO_TICKS(5000)) & EV_GATTC_REG)) {
        ESP_LOGE(TAG, "the GATT client didn't register: a TD-H9 can't connect");
    }
    for (;;) {
        restart = false;
        xQueueReset(gap_events); // only pairing and scans read these
        Record r = record();
        if (!r.present) {
            set_port(PortState::unpaired, board::screen
                                              ? "no radio: pair a TH-D74 or TH-D75, or select a TD-H9, on the TNC page"
                                              : "no radio: double press the button to pair a TH-D74 or TH-D75, "
                                                "or select a TD-H9");
            serve_until(portMAX_DELAY);
            continue;
        }
        const Model &m = model_of(r.model);
        if (!m.ble && !bonded(r.addr)) {
            // Bluedroid removes a bond when the radio refuses its key: the
            // radio has forgotten this device. Retrying can't help.
            set_port(PortState::unpaired, r.addr.str() +
                                              " isn't paired any more (the radio may have forgotten this "
                                              "device): " +
                                              (board::screen ? "pair it again on the TNC page"
                                                             : "double press the button to pair it again"));
            serve_until(portMAX_DELAY);
            continue;
        }
        if (hold_state == Hold::down) {
            set_port(PortState::idle, "held down by port.disconnect");
            serve_until(portMAX_DELAY);
            continue;
        }
        set_port(PortState::connecting, r.addr.str());
        if (m.ble) {
            // A TD-H9: the connect waits for it to advertise, so a radio
            // that's off or away is caught within a second of coming back
            // (spikes/atom-lite-ble-tnc).
            BleOpen res = ble_connect(r);
            if (res == BleOpen::opened) {
                counters.tnc_connects++;
                set_port(PortState::connected);
                bridge();
                ble_close();
                continue;
            }
            if (res == BleOpen::interrupted) {
                serve_now();
                continue;
            }
            if (res == BleOpen::waited || restart) {
                continue;
            }
            set_port(PortState::backoff, "retrying in 15 s");
            serve_until(retry_delay);
            continue;
        }
        if (connect_to(r.addr, connect_timeout)) {
            counters.tnc_connects++;
            if (kiss_init(m)) {
                set_port(PortState::connected);
                bridge();
            } else {
                ESP_LOGE(TAG, "KISS init failed: closing the link");
                drop_link();
            }
        }
        if (restart) {
            continue;
        }
        set_port(PortState::backoff, "retrying in 15 s");
        serve_until(retry_delay);
    }
}

} // namespace

bool start()
{
    load_record();

    events = xEventGroupCreate();
    gap_events = xQueueCreate(16, sizeof(GapEvent));
    requests = xQueueCreate(8, sizeof(Request));
    from_radio = xStreamBufferCreate(4096, 1);
    write_results = xQueueCreate(1, sizeof(WriteResult));
    write_mutex = xSemaphoreCreateMutex();
    to_radio = xMessageBufferCreate(4096);

    esp_bt_controller_config_t bt_cfg = BT_CONTROLLER_INIT_CONFIG_DEFAULT();
    esp_bluedroid_config_t bd_cfg = BT_BLUEDROID_INIT_CONFIG_DEFAULT();
    if (!started("the controller", esp_bt_controller_init(&bt_cfg)) ||
        !started("the controller", esp_bt_controller_enable(ESP_BT_MODE_BTDM)) ||
        !started("Bluedroid", esp_bluedroid_init_with_cfg(&bd_cfg)) ||
        !started("Bluedroid", esp_bluedroid_enable()) ||
        !started("GAP", esp_bt_gap_register_callback(gap_cb))) {
        set_port(PortState::idle, "Bluetooth didn't start");
        return false;
    }
    // DisplayYesNo: the TH-D75 pairs by numeric comparison, and refuses
    // Just Works (spikes/FINDINGS.md, Pairing).
    esp_bt_io_cap_t iocap = ESP_BT_IO_CAP_IO;
    esp_bt_gap_set_security_param(ESP_BT_SP_IOCAP_MODE, &iocap, sizeof(iocap));
    esp_bt_pin_code_t pin = {};
    esp_bt_gap_set_pin(ESP_BT_PIN_TYPE_VARIABLE, 0, pin);
    esp_bt_gap_set_device_name(settings::public_name().c_str());
    // The device only connects out; nothing connects to it over Classic.
    esp_bt_gap_set_scan_mode(ESP_BT_NON_CONNECTABLE, ESP_BT_NON_DISCOVERABLE);

    if (!started("the GATT client", esp_ble_gattc_register_callback(gattc_cb)) ||
        !started("the GATT client", esp_ble_gattc_app_register(GATTC_APP_ID))) {
        set_port(PortState::idle, "Bluetooth didn't start");
        return false;
    }

    esp_spp_cfg_t cfg = {};
    cfg.mode = ESP_SPP_MODE_CB;
    cfg.enable_l2cap_ertm = true;
    if (!started("SPP", esp_spp_register_callback(spp_cb)) ||
        !started("SPP", esp_spp_enhanced_init(&cfg))) {
        set_port(PortState::idle, "Bluetooth didn't start");
        return false;
    }

    Record r = record();
    set_port(r.present ? PortState::connecting : PortState::unpaired);
    ESP_LOGI(TAG, "%s", r.present ? ("radio " + r.name + " " + r.addr.str() + ", model " + r.model).c_str()
                                  : "no radio paired");
    xTaskCreate(radio_task, "radio", 6144, nullptr, 5, nullptr);
    xTaskCreate(writer_task, "radio_tx", 4096, nullptr, 5, nullptr);
    running = true;
    return true;
}

Link link()
{
    if (override_on) {
        return override_link;
    }
    switch (port_state.load()) {
    case PortState::connected: return Link::connected;
    case PortState::unpaired: return Link::none;
    default: return Link::down;
    }
}

bool pairing() { return pairing_now; }
int phones()
{
    int d = debug_phones;
    return d >= 0 ? d : phones::sessions();
}

View view()
{
    Record r = record();
    View v;
    v.present = r.present;
    v.ble = r.ble();
    v.paired = r.present && !v.ble && bonded(r.addr);
    v.name = r.name.empty() ? r.model : r.name;
    const Model *m = find_model(r.model);
    v.make = m && m->radio ? m->radio : "";
    v.address = r.present ? r.addr.str() : "";
    {
        std::lock_guard<std::mutex> g(state_lock);
        v.detail = port_detail;
    }
    int64_t since = state_since_us;
    uint32_t elapsed = static_cast<uint32_t>((esp_timer_get_time() - since) / 1000000);
    v.state_s = elapsed;
    v.down_s = static_cast<uint32_t>((esp_timer_get_time() - link_down_us) / 1000000);
    switch (port_state.load()) {
    case PortState::unpaired: v.state = State::none; break;
    case PortState::idle: v.state = State::idle; break;
    case PortState::connecting: v.state = State::connecting; break;
    case PortState::connected:
        v.state = State::connected;
        v.rx = counters.frames_down - link_start_down;
        v.tx = counters.frames_up - link_start_up;
        break;
    case PortState::backoff: {
        v.state = State::retrying;
        uint32_t total = pdTICKS_TO_MS(retry_delay) / 1000;
        v.retry_in_s = elapsed < total ? total - elapsed : 0;
        break;
    }
    }
    return v;
}

void send_frame(const uint8_t *frame, size_t len, const std::string &session)
{
    uint8_t msg[1 + 64 + kiss::Splitter::max_frame];
    size_t slen = std::min<size_t>(session.size(), 64);
    if (len > kiss::Splitter::max_frame) {
        counters.dropped++;
        return;
    }
    msg[0] = static_cast<uint8_t>(slen);
    memcpy(msg + 1, session.data(), slen);
    memcpy(msg + 1 + slen, frame, len);
    if (xMessageBufferSend(to_radio, msg, 1 + slen + len, 0) == 0) {
        counters.dropped++; // the radio is slower than the phones
    }
}

void start_pairing()
{
    if (pairing_now || busy_now) {
        ui::post(ui::Event::pairing_failed);
        return;
    }
    pairing_now = true; // the LED shows it at once
    Request r{ReqType::button_pair, {}};
    post(r);
}

void cancel_pairing()
{
    Request r{ReqType::button_cancel, {}};
    post(r);
}

void answer_pairing(bool same)
{
    Request r{ReqType::button_answer, {}};
    r.flag = same;
    post(r);
}

PairView pairing_view()
{
    std::lock_guard<std::mutex> g(pair_view_lock);
    PairView v = pair_view_now;
    int64_t left = pair_view_until - esp_timer_get_time();
    v.left_s = left > 0 ? static_cast<uint32_t>((left + 999999) / 1000000) : 0;
    return v;
}

void scan(const control::Pending &p, uint32_t seconds, bool le)
{
    if (busy_now) {
        control::reply_error(p, "busy", "a Bluetooth scan or pairing is running; one runs at a time");
        return;
    }
    Request r{ReqType::scan, p};
    r.seconds = seconds;
    r.flag = le;
    post(r);
}

void scan_stop(const control::Pending &p, double scan_request)
{
    Request r{ReqType::scan_stop, p};
    r.ref = scan_request;
    post(r);
}

void pair(const control::Pending &p, const std::string &address)
{
    Request r{ReqType::pair, p};
    std::string a = address;
    if (a.empty()) {
        Record rc = record();
        if (!rc.present) {
            control::reply_error(p, "not_found", "no radio address to pair with: give an address");
            return;
        }
        a = rc.addr.str();
    }
    Addr parsed;
    if (!Addr::parse(a, parsed)) {
        control::reply_error(p, "bad_request", "address must look like 40:79:12:E4:95:9D");
        return;
    }
    if (busy_now) {
        control::reply_error(p, "busy", "a Bluetooth scan or pairing is running; one runs at a time");
        return;
    }
    strlcpy(r.addr, parsed.str().c_str(), sizeof(r.addr));
    post(r);
}

void pair_reply(const control::Pending &p, double pair_request, bool accept)
{
    Request r{ReqType::pair_reply, p};
    r.ref = pair_request;
    r.flag = accept;
    post(r);
}

void unpair(const control::Pending &p, const std::string &address)
{
    Request r{ReqType::unpair, p};
    if (!address.empty()) {
        Addr a;
        if (!Addr::parse(address, a)) {
            control::reply_error(p, "bad_request", "address must look like 40:79:12:E4:95:9D");
            return;
        }
        strlcpy(r.addr, a.str().c_str(), sizeof(r.addr));
    }
    post(r);
}

void update(const control::Pending &p, const std::string &model, const std::string &address, bool address_ble,
            bool non_disruptive)
{
    Request r{ReqType::update, p};
    if (!model.empty()) {
        if (!find_model(model)) {
            control::reply_error(p, "bad_request", "unknown model " + model + " (see tnc.models)");
            return;
        }
        strlcpy(r.model, model.c_str(), sizeof(r.model));
    }
    // The model decides the transport: spp for a Classic radio, ble for a
    // TD-H9. Moving between them needs the new transport's address.
    Record cur = record();
    bool ble = model.empty() ? cur.ble() : model_of(model).ble;
    const char *key = ble ? "ble.address" : "spp.address";
    if (!address.empty() && address_ble != ble) {
        control::reply_error(p, "bad_request", std::string("model ") + (model.empty() ? cur.model : model) +
                                                   " takes " + key);
        return;
    }
    if (address.empty() && cur.present && ble != cur.ble()) {
        control::reply_error(p, "bad_request", std::string("changing to model ") + model + " needs " + key);
        return;
    }
    if (!address.empty()) {
        Addr a;
        if (!Addr::parse(address, a)) {
            control::reply_error(p, "bad_request", std::string(key) + " must look like 40:79:12:E4:95:9D");
            return;
        }
        strlcpy(r.addr, a.str().c_str(), sizeof(r.addr));
    }
    r.flag = non_disruptive;
    post(r);
}

void hold(const control::Pending &p, Hold h)
{
    Request r{ReqType::hold, p};
    r.hold = h;
    post(r);
}

cJSON *status_json()
{
    Record r = record();
    std::string detail;
    {
        std::lock_guard<std::mutex> g(state_lock);
        detail = port_detail;
    }
    bool ble = r.ble();
    bool paired = r.present && !ble && bonded(r.addr);
    const uint8_t *own = esp_bt_dev_get_address();
    char mac[18] = "";
    if (own) {
        snprintf(mac, sizeof(mac), "%02X:%02X:%02X:%02X:%02X:%02X", own[0], own[1], own[2], own[3], own[4],
                 own[5]);
    }

    cJSON *s = cJSON_CreateObject();
    cJSON_AddStringToObject(s, "version", esp_app_get_description()->version);

    cJSON *adapters = cJSON_AddArrayToObject(s, "adapters");
    cJSON *ad = cJSON_CreateObject();
    cJSON_AddStringToObject(ad, "id", "builtin");
    cJSON_AddStringToObject(ad, "mac", mac);
    cJSON_AddBoolToObject(ad, "present", true);
    cJSON_AddBoolToObject(ad, "powered", true);
    cJSON *devices = cJSON_AddArrayToObject(ad, "devices");
    if (paired) {
        Found f{r.addr, r.name};
        cJSON *d = device_json(f, r);
        cJSON_AddItemToArray(devices, d);
    }
    cJSON_AddItemToArray(adapters, ad);

    cJSON *tncs = cJSON_AddArrayToObject(s, "tncs");
    cJSON *t = cJSON_CreateObject();
    cJSON_AddStringToObject(t, "id", "radio");
    cJSON_AddStringToObject(t, "name", r.name.empty() ? "radio" : r.name.c_str());
    cJSON_AddStringToObject(t, "model", r.model.c_str());
    if (r.present && ble) {
        // Not paired: a BLE TNC takes any central, like a phone.
        cJSON *b = cJSON_AddObjectToObject(t, "ble");
        cJSON_AddStringToObject(b, "address", r.addr.str().c_str());
        cJSON_AddStringToObject(b, "adapter", "builtin");
    } else if (r.present) {
        cJSON *spp = cJSON_AddObjectToObject(t, "spp");
        cJSON_AddStringToObject(spp, "address", r.addr.str().c_str());
        cJSON_AddStringToObject(spp, "adapter", "builtin");
        cJSON_AddBoolToObject(spp, "paired", paired);
    }
    cJSON_AddStringToObject(t, "in_use_by", "ktbr0");
    cJSON_AddBoolToObject(t, "editable", true);
    cJSON_AddItemToArray(tncs, t);

    std::string name = settings::public_name();
    cJSON *bridges = cJSON_AddArrayToObject(s, "bridges");
    cJSON *b = cJSON_CreateObject();
    cJSON_AddStringToObject(b, "id", "ktbr0");
    cJSON_AddStringToObject(b, "name", name.c_str());
    cJSON_AddBoolToObject(b, "enabled", true);
    cJSON *pn = cJSON_AddObjectToObject(b, "public_name");
    cJSON_AddStringToObject(pn, "configured", name.c_str());
    cJSON_AddStringToObject(pn, "effective", name.c_str());
    cJSON *ports = cJSON_AddArrayToObject(b, "ports");

    cJSON *rp = cJSON_CreateObject();
    cJSON_AddStringToObject(rp, "id", "radio");
    cJSON_AddStringToObject(rp, "type", ble ? "ble-client" : "spp-client");
    cJSON_AddStringToObject(rp, "role", "tnc");
    cJSON_AddStringToObject(rp, "state", port_state_name(port_state));
    std::string d = detail;
    Hold h = hold_state;
    if (h == Hold::up) {
        d += d.empty() ? "held up by port.connect" : "; held up by port.connect";
    }
    if (!d.empty()) {
        cJSON_AddStringToObject(rp, "detail", d.c_str());
    }
    cJSON_AddArrayToObject(rp, "sessions");
    cJSON_AddItemToArray(ports, rp);

    cJSON_AddItemToArray(ports, phones::port_json());

    cJSON *c = cJSON_AddObjectToObject(b, "counters");
    cJSON_AddNumberToObject(c, "frames_up", counters.frames_up);
    cJSON_AddNumberToObject(c, "frames_down", counters.frames_down);
    cJSON_AddNumberToObject(c, "dropped", counters.dropped);
    cJSON_AddNumberToObject(c, "tnc_connects", counters.tnc_connects);
    cJSON_AddItemToArray(bridges, b);
    return s;
}

cJSON *models_json()
{
    cJSON *list = cJSON_CreateArray();
    cJSON *generic = cJSON_CreateObject();
    cJSON *gn = cJSON_AddArrayToObject(generic, "names");
    cJSON_AddItemToArray(gn, cJSON_CreateString("generic"));
    cJSON_AddStringToObject(generic, "summary", "any SPP TNC that's always in KISS mode; nothing is sent at connect");
    cJSON_AddItemToArray(list, generic);

    cJSON *d7x = cJSON_CreateObject();
    cJSON *names = cJSON_AddArrayToObject(d7x, "names");
    cJSON_AddItemToArray(names, cJSON_CreateString("th-d74"));
    cJSON_AddItemToArray(names, cJSON_CreateString("th-d75"));
    cJSON_AddStringToObject(d7x, "summary",
                            "Kenwood TH-D74/D75: switched to KISS mode at every connect, on the band its TNC is set to");
    cJSON *radios = cJSON_AddArrayToObject(d7x, "radios");
    for (const Model &m : models) {
        if (!m.radio || m.ble) {
            continue;
        }
        cJSON *radio = cJSON_CreateObject();
        cJSON_AddStringToObject(radio, "name", m.radio);
        cJSON_AddStringToObject(radio, "model", m.name);
        cJSON *bt = cJSON_AddObjectToObject(radio, "bluetooth");
        cJSON_AddStringToObject(bt, "name_prefix", m.name_prefix);
        cJSON_AddStringToObject(bt, "setup", "On the radio, set Menu > Configuration > Interface > KISS to Bluetooth");
        cJSON_AddStringToObject(bt, "pairing_mode", "Menu > Bluetooth > Pairing Mode");
        cJSON_AddStringToObject(bt, "pairing",
                                "The radio shows a six-digit number: press OK on the radio within 30 seconds");
        cJSON_AddItemToArray(radios, radio);
    }
    cJSON_AddItemToArray(list, d7x);

    cJSON *h9 = cJSON_CreateObject();
    cJSON *h9n = cJSON_AddArrayToObject(h9, "names");
    cJSON_AddItemToArray(h9n, cJSON_CreateString("td-h9"));
    cJSON_AddStringToObject(h9, "summary",
                            "TIDRADIO TD-H9: KISS over BLE on TIDRADIO's own GATT service (af00) instead of the "
                            "BLE KISS API's; otherwise like generic");
    cJSON *h9r = cJSON_AddArrayToObject(h9, "radios");
    for (const Model &m : models) {
        if (!m.radio || !m.ble) {
            continue;
        }
        cJSON *radio = cJSON_CreateObject();
        cJSON_AddStringToObject(radio, "name", m.radio);
        cJSON_AddStringToObject(radio, "model", m.name);
        cJSON *b = cJSON_AddObjectToObject(radio, "ble");
        cJSON_AddStringToObject(b, "name_prefix", m.name_prefix);
        cJSON_AddStringToObject(b, "setup",
                                "On the radio, set Menu > Bluetooth > BT On/Off to On and BT Mode to Receiver, "
                                "Menu > APRS > APRS Switch to On (with it off, the radio transmits but passes "
                                "nothing it receives to the bridge), and the APRS channel's bandwidth to wide "
                                "(on narrow it decodes nothing). Disconnect any phone app from it: while "
                                "something is connected, it doesn't advertise.");
        cJSON_AddItemToArray(h9r, radio);
    }
    cJSON_AddItemToArray(list, h9);
    return list;
}

void name_changed()
{
    esp_bt_gap_set_device_name(settings::public_name().c_str());
}

void ble_gap_event(esp_gap_ble_cb_event_t event, esp_ble_gap_cb_param_t *param)
{
    if (!gap_events) {
        return;
    }
    GapEvent e;
    switch (event) {
    case ESP_GAP_BLE_SCAN_PARAM_SET_COMPLETE_EVT:
        if (uint32_t seconds = ble_scan_seconds.exchange(0)) {
            esp_ble_gap_start_scanning(seconds);
        }
        return;
    case ESP_GAP_BLE_SCAN_START_COMPLETE_EVT:
        if (param->scan_start_cmpl.status == ESP_BT_STATUS_SUCCESS) {
            return;
        }
        ESP_LOGW(TAG, "BLE scan didn't start, status %d", param->scan_start_cmpl.status);
        e.type = GapEvent::ble_scan_done;
        break;
    case ESP_GAP_BLE_SCAN_RESULT_EVT: {
        auto &r = param->scan_rst;
        if (r.search_evt == ESP_GAP_SEARCH_INQ_CMPL_EVT) {
            e.type = GapEvent::ble_scan_done;
            break;
        }
        if (r.search_evt != ESP_GAP_SEARCH_INQ_RES_EVT) {
            return;
        }
        // Only named devices, each once: a scan lists what someone could
        // pick, and a TD-H9 is known by its name. A known radio is passed on
        // every time (a TD-H9 advertises about five times a second), as the
        // search can miss one while it waits for something else.
        std::string name = adv_name(r);
        if (name.empty()) {
            return;
        }
        e.addr = Addr::of(r.bda);
        if (!model_for_name(name, true)) {
            std::lock_guard<std::mutex> g(scan_lock);
            if (std::find(scan_seen.begin(), scan_seen.end(), e.addr) != scan_seen.end()) {
                return;
            }
            scan_seen.push_back(e.addr);
        }
        e.type = GapEvent::ble_found;
        e.addr_type = r.ble_addr_type;
        copy_name(e.name, reinterpret_cast<const uint8_t *>(name.data()), name.size());
        e.has_name = true;
        e.rssi = r.rssi;
        e.has_rssi = true;
        break;
    }
    case ESP_GAP_BLE_UPDATE_CONN_PARAMS_EVT:
        if (ble_active && Addr::of(param->update_conn_params.bda) == ble_target) {
            ESP_LOGI(TAG, "BLE link: interval %.2f ms, latency %u, timeout %u ms",
                     param->update_conn_params.conn_int * 1.25, param->update_conn_params.latency,
                     param->update_conn_params.timeout * 10);
        }
        return;
    default:
        return;
    }
    if (xQueueSend(gap_events, &e, 0) != pdTRUE) {
        ESP_LOGW(TAG, "GAP event queue full");
    }
}

void remove_all_bonds()
{
    if (esp_bluedroid_get_status() != ESP_BLUEDROID_STATUS_ENABLED) {
        return;
    }
    int n = esp_bt_gap_get_bond_device_num();
    if (n <= 0) {
        return;
    }
    std::vector<esp_bd_addr_t> list(n);
    esp_bt_gap_get_bond_device_list(&n, list.data());
    for (int i = 0; i < n; i++) {
        esp_bt_gap_remove_bond_device(list[i]);
    }
}

#if CONFIG_KTB_DEBUG_COMMANDS
void debug_override_link(bool on, Link l)
{
    override_link = l;
    override_on = on;
}
void debug_set_phones(int n) { debug_phones = n; }
#endif

} // namespace ktb::radio
