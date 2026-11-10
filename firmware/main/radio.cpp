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
#include "esp_log.h"
#include "esp_spp_api.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "freertos/stream_buffer.h"
#include "freertos/task.h"
#include "kiss.h"
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
};

const Model models[] = {
    {"th-d74", "Kenwood TH-D74", "TH-D74", true},
    {"th-d75", "Kenwood TH-D75", "TH-D75", true},
    {"generic", nullptr, nullptr, false},
};

const Model *find_model(const std::string &name)
{
    for (const Model &m : models) {
        if (name == m.name) {
            return &m;
        }
    }
    return nullptr;
}

// The model a Bluetooth name belongs to, or null.
const Model *model_for_name(const std::string &name)
{
    for (const Model &m : models) {
        if (m.name_prefix && name.compare(0, strlen(m.name_prefix), m.name_prefix) == 0) {
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
    std::string model = "th-d75";
    std::string name;
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
std::atomic<Hold> hold_state{Hold::none};
std::atomic<int> phone_count{0};
std::atomic<bool> override_on{false};
std::atomic<Link> override_link{Link::none};

struct Counters {
    std::atomic<uint32_t> frames_up{0};
    std::atomic<uint32_t> frames_down{0};
    std::atomic<uint32_t> dropped{0};
    std::atomic<uint32_t> tnc_connects{0};
} counters;

void set_port(PortState s, const std::string &detail = "")
{
    PortState was = port_state.exchange(s);
    {
        std::lock_guard<std::mutex> g(state_lock);
        port_detail = detail;
    }
    if (was != s) {
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
};
EventGroupHandle_t events;

struct GapEvent {
    enum Type : uint8_t { found, disc_started, disc_stopped, remote_name, confirm, auth } type;
    Addr addr;
    bool ok = false;
    bool has_name = false;
    char name[32] = {};
    uint32_t cod = 0;
    int8_t rssi = 0;
    bool has_rssi = false;
    uint32_t number = 0;
};
QueueHandle_t gap_events;

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
            ESP_LOGE(TAG, "SPP init failed, status %d", p->init.status);
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

// ---- The link ----

void drop_link()
{
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
    if (phone_count > 0) {
        ui::post(ui::Event::frame_to_phone);
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

// The button's search: the first TH-D74 or TH-D75 in pairing mode, within
// the deadline. Radios report a phone's Class of Device, so it goes by
// name; one with the right class but no name in its inquiry response is
// asked for its name.
SearchResult search(TickType_t deadline, Found &out)
{
    xQueueReset(gap_events);
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
            } else if (e.type == GapEvent::found) {
                if (e.has_name && model_for_name(e.name)) {
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
            if (model_for_name(name)) {
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
    cJSON_AddBoolToObject(d, "paired", bonded(f.addr));
    if (const Model *m = model_for_name(f.name)) {
        cJSON_AddStringToObject(d, "known_tnc", m->radio);
        cJSON_AddStringToObject(d, "model", m->name);
    }
    if (r.present && r.addr == f.addr) {
        cJSON_AddStringToObject(d, "tnc", "radio");
    }
    return d;
}

// bluetooth.scan: everything that answers, with found events as they come.
void do_scan(const Request &req)
{
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
    if (esp_bt_gap_start_discovery(ESP_BT_INQ_MODE_GENERAL_INQUIRY,
                                   inquiry_len(pdMS_TO_TICKS(req.seconds * 1000)), 0) != ESP_OK) {
        busy_now = false;
        control::reply_error(req.p, "failed", "couldn't start the scan");
        return;
    }
    Record r = record();
    bool stopped = false;
    while (!stopped) {
        if (xTaskGetTickCount() > deadline + pdMS_TO_TICKS(2000)) {
            stop_discovery();
            break;
        }
        Request q;
        while (xQueueReceive(requests, &q, 0) == pdTRUE) {
            if (q.type == ReqType::scan_stop && q.ref == req.p.id) {
                stop_discovery();
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
        if (e.type == GapEvent::disc_stopped) {
            stopped = true;
        } else if (e.type == GapEvent::found) {
            auto it = std::find_if(found.begin(), found.end(), [&](const Found &f) { return f.addr == e.addr; });
            bool news = false;
            if (it == found.end()) {
                found.push_back(Found{e.addr, e.has_name ? e.name : "", e.cod, e.rssi, e.has_rssi});
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
        return model_for_name(a.name) && !model_for_name(b.name);
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

// Pairs with a radio and makes it the radio, keeping the old one until the
// new one has paired. From the button (search, confirm at once) or from
// bluetooth.pair (a given address, the client confirms).
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
    if (button) {
        ESP_LOGI(TAG, "pairing: searching for a TH-D74 or TH-D75 in pairing mode");
        switch (search(xTaskGetTickCount() + pdMS_TO_TICKS(pairing_seconds * 1000), target)) {
        case SearchResult::found:
            ESP_LOGI(TAG, "pairing: found %s, %s", target.name.c_str(), target.addr.str().c_str());
            break;
        case SearchResult::cancelled:
            cancelled = true;
            break;
        case SearchResult::none:
            failure = "no TH-D74 or TH-D75 in pairing mode was found";
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
    if (!failure && !cancelled) {
        pairing_event(p, "pairing", "name", target.name);
        if (bonded(target.addr)) {
            // Pair afresh: the radio may have forgotten us.
            esp_bt_gap_remove_bond_device(target.addr.raw());
        }
        xQueueReset(gap_events);
        xEventGroupClearBits(events, EV_OPEN | EV_CLOSED | EV_FAILED);
        connect_addr = target.addr;
        if (esp_spp_start_discovery(connect_addr.raw()) != ESP_OK) {
            failure = "couldn't start pairing";
        }
        TickType_t deadline = xTaskGetTickCount() + pair_timeout;
        bool confirm_pending = false;
        TickType_t confirm_deadline = 0;
        while (!failure && !cancelled && xTaskGetTickCount() < deadline) {
            Request q;
            while (xQueueReceive(requests, &q, 0) == pdTRUE) {
                if (button && q.type == ReqType::button_cancel) {
                    cancelled = true;
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
                    if (button) {
                        ESP_LOGI(TAG, "pairing: the radio shows %s: press OK on it", num);
                        esp_bt_gap_ssp_confirm_reply(target.addr.raw(), true);
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
                        failure = "the pairing failed: press OK on the radio within 30 s of the number "
                                  "appearing, and dismiss any old prompt before trying again";
                    }
                }
            }
            EventBits_t bits = xEventGroupGetBits(events);
            if (bits & EV_OPEN) {
                opened = true;
            }
            if (authed && (opened || (bits & (EV_FAILED | EV_CLOSED)))) {
                break; // paired; the link is open or won't be
            }
            if (!authed && (bits & (EV_FAILED | EV_CLOSED))) {
                failure = "the radio didn't accept the connection: is it in pairing mode?";
            }
        }
        if (!authed && !failure && !cancelled) {
            failure = "the pairing timed out";
        }
        if (spp_handle != 0) {
            drop_link(); // the task connects afresh
        }
    }

    if (authed && !cancelled) {
        Record old = record();
        Record r;
        r.present = true;
        r.addr = target.addr;
        r.name = target.name;
        const Model *m = model_for_name(target.name);
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
        if (authed) {
            // Cancelled after the radio's OK: forget the new radio.
            esp_bt_gap_remove_bond_device(target.addr.raw());
        }
        const char *why = cancelled ? "cancelled" : failure;
        ESP_LOGI(TAG, "pairing: %s", why);
        ui::post(ui::Event::pairing_failed);
        control::reply_error(p, "failed", why);
    }
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
    bool changed = next.present != r.present || next.addr != r.addr || next.model != r.model;
    bool link_up = port_state == PortState::connected;
    bool disruptive = changed && link_up && phone_count > 0;

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
        break; // nothing to cancel
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

void radio_task(void *)
{
    xEventGroupWaitBits(events, EV_SPP_INIT, pdFALSE, pdFALSE, portMAX_DELAY);
    for (;;) {
        restart = false;
        xQueueReset(gap_events); // only pairing and scans read these
        Record r = record();
        if (!r.present) {
            set_port(PortState::unpaired, "no radio paired: double press the button to pair one");
            serve_until(portMAX_DELAY);
            continue;
        }
        if (!bonded(r.addr)) {
            set_port(PortState::unpaired, r.addr.str() + " isn't paired: pair it again");
            serve_until(portMAX_DELAY);
            continue;
        }
        if (hold_state == Hold::down) {
            set_port(PortState::idle, "held down by port.disconnect");
            serve_until(portMAX_DELAY);
            continue;
        }
        set_port(PortState::connecting, r.addr.str());
        if (connect_to(r.addr, connect_timeout)) {
            counters.tnc_connects++;
            const Model *m = find_model(r.model);
            if (kiss_init(m ? *m : models[2])) {
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

void start()
{
    load_record();

    events = xEventGroupCreate();
    gap_events = xQueueCreate(16, sizeof(GapEvent));
    requests = xQueueCreate(8, sizeof(Request));
    from_radio = xStreamBufferCreate(4096, 1);
    write_results = xQueueCreate(1, sizeof(WriteResult));
    write_mutex = xSemaphoreCreateMutex();

    esp_bt_controller_config_t bt_cfg = BT_CONTROLLER_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_bt_controller_init(&bt_cfg));
    ESP_ERROR_CHECK(esp_bt_controller_enable(ESP_BT_MODE_BTDM));
    esp_bluedroid_config_t bd_cfg = BT_BLUEDROID_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_bluedroid_init_with_cfg(&bd_cfg));
    ESP_ERROR_CHECK(esp_bluedroid_enable());

    ESP_ERROR_CHECK(esp_bt_gap_register_callback(gap_cb));
    // DisplayYesNo: the TH-D75 pairs by numeric comparison, and refuses
    // Just Works (spikes/FINDINGS.md, Pairing).
    esp_bt_io_cap_t iocap = ESP_BT_IO_CAP_IO;
    esp_bt_gap_set_security_param(ESP_BT_SP_IOCAP_MODE, &iocap, sizeof(iocap));
    esp_bt_pin_code_t pin = {};
    esp_bt_gap_set_pin(ESP_BT_PIN_TYPE_VARIABLE, 0, pin);
    esp_bt_gap_set_device_name(settings::public_name().c_str());
    // The device only connects out; nothing connects to it over Classic.
    esp_bt_gap_set_scan_mode(ESP_BT_NON_CONNECTABLE, ESP_BT_NON_DISCOVERABLE);

    ESP_ERROR_CHECK(esp_spp_register_callback(spp_cb));
    esp_spp_cfg_t cfg = {};
    cfg.mode = ESP_SPP_MODE_CB;
    cfg.enable_l2cap_ertm = true;
    ESP_ERROR_CHECK(esp_spp_enhanced_init(&cfg));

    Record r = record();
    set_port(r.present ? PortState::connecting : PortState::unpaired);
    ESP_LOGI(TAG, "%s", r.present ? ("radio " + r.name + " " + r.addr.str() + ", model " + r.model).c_str()
                                  : "no radio paired");
    xTaskCreate(radio_task, "radio", 6144, nullptr, 5, nullptr);
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
int phones() { return phone_count; }

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

void scan(const control::Pending &p, uint32_t seconds)
{
    if (busy_now) {
        control::reply_error(p, "busy", "a Bluetooth scan or pairing is running; one runs at a time");
        return;
    }
    Request r{ReqType::scan, p};
    r.seconds = seconds;
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

void update(const control::Pending &p, const std::string &model, const std::string &address,
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
    if (!address.empty()) {
        Addr a;
        if (!Addr::parse(address, a)) {
            control::reply_error(p, "bad_request", "spp.address must look like 40:79:12:E4:95:9D");
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
    bool paired = r.present && bonded(r.addr);
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
    if (r.present) {
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
    cJSON_AddStringToObject(rp, "type", "spp-client");
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

    cJSON *ph = cJSON_CreateObject();
    cJSON_AddStringToObject(ph, "id", "phones");
    cJSON_AddStringToObject(ph, "type", "ble-server");
    cJSON_AddStringToObject(ph, "role", "app");
    cJSON_AddStringToObject(ph, "state", "stopped");
    cJSON_AddStringToObject(ph, "detail", "the BLE KISS service comes in milestone 3");
    cJSON_AddArrayToObject(ph, "sessions");
    cJSON_AddItemToArray(ports, ph);

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
        if (!m.radio) {
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
    return list;
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
void debug_set_phones(int n) { phone_count = n; }
#endif

} // namespace ktb::radio
