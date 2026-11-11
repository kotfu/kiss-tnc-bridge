#include "phones.h"

#include <algorithm>
#include <atomic>
#include <cstdio>
#include <cstring>
#include <mutex>

#include "activity.h"
#include "errors.h"
#include "esp_bt_defs.h"
#include "esp_gap_ble_api.h"
#include "esp_gap_bt_api.h"
#include "esp_gatt_common_api.h"
#include "esp_gatts_api.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "kiss.h"
#include "radio.h"
#include "settings.h"

namespace ktb::phones {

namespace {

constexpr const char *TAG = "phones";

// BLE KISS API UUIDs, the same as the daemon's ble-server ports.
constexpr const char *KISS_SERVICE = "00000001-ba2a-46c9-ae49-01b0961f68bb";
constexpr const char *KISS_TX = "00000002-ba2a-46c9-ae49-01b0961f68bb"; // phone writes
constexpr const char *KISS_RX = "00000003-ba2a-46c9-ae49-01b0961f68bb"; // phone subscribes
// The management service (docs/control-api.md, BLE management). It isn't
// served until Phase E, but its UUID goes in the scan response now, so the
// advertising layout can be tried with KISS TNC apps (roadmap, Phase C).
constexpr const char *MGMT_SERVICE = "e3370001-156d-4ba6-90dd-14075e7ca557";

constexpr uint16_t APP_ID = 0x4b;
constexpr uint16_t LOCAL_MTU = 517;
constexpr int MAX_CONNS = CONFIG_BTDM_CTRL_BLE_MAX_CONN;
// The most of the name that fits in the scan response beside the
// management UUID: 31 bytes, less 18 for the UUID and 2 for the name's
// header.
constexpr size_t ADV_NAME_MAX = 11;

enum Idx { IDX_SVC, IDX_TX_DECL, IDX_TX_VAL, IDX_RX_DECL, IDX_RX_VAL, IDX_RX_CCC, IDX_NB };

uint8_t svc_uuid[16];
uint8_t tx_uuid[16];
uint8_t rx_uuid[16];
uint8_t mgmt_uuid[16];
uint16_t handles[IDX_NB];
esp_gatts_attr_db_t db[IDX_NB];

const uint16_t primary_service_uuid = ESP_GATT_UUID_PRI_SERVICE;
const uint16_t char_decl_uuid = ESP_GATT_UUID_CHAR_DECLARE;
const uint16_t ccc_uuid = ESP_GATT_UUID_CHAR_CLIENT_CONFIG;
const uint8_t prop_write = ESP_GATT_CHAR_PROP_BIT_WRITE | ESP_GATT_CHAR_PROP_BIT_WRITE_NR;
const uint8_t prop_notify = ESP_GATT_CHAR_PROP_BIT_NOTIFY;
uint8_t ccc_init[2] = {0, 0};
uint8_t tx_init[1] = {0};
uint8_t rx_init[1] = {0};

struct Conn {
    bool used = false;
    uint16_t conn_id = 0;
    esp_bd_addr_t bda{};
    uint16_t mtu = 23;
    bool subscribed = false; // a session
    std::string session;     // ktbr0/phones/<address>
    kiss::Splitter splitter;
};

std::mutex lock;
Conn conns[MAX_CONNS];
esp_gatt_if_t gatts_if_saved = ESP_GATT_IF_NONE;
esp_ble_adv_params_t adv_params = {};
bool adv_data_set = false;
bool scan_rsp_set = false;
bool advertising = false;
bool failed = false;
std::string failure;

// The service or advertising failed: the phones error, until advertising
// starts.
void fail(const char *why)
{
    failed = true;
    failure = why;
    errors::set(errors::Error::phones, std::string(why) +
                                           ": phones can't find the device. Unplug the device and plug it in");
}

EventGroupHandle_t events;
constexpr EventBits_t EV_ADVERTISING = 1 << 0;

// "00000001-ba2a-..." to the little-endian bytes Bluedroid wants.
void parse_uuid(const char *text, uint8_t out[16])
{
    uint8_t be[16];
    int n = 0;
    for (const char *p = text; *p && n < 16; p++) {
        if (*p == '-') {
            continue;
        }
        unsigned byte;
        sscanf(p, "%2x", &byte);
        be[n++] = static_cast<uint8_t>(byte);
        p++;
    }
    for (int i = 0; i < 16; i++) {
        out[i] = be[15 - i];
    }
}

esp_gatts_attr_db_t attr(uint16_t uuid_len, const void *uuid, uint16_t perm, uint16_t max_len, uint16_t len,
                         const void *value)
{
    esp_gatts_attr_db_t a = {};
    a.attr_control.auto_rsp = ESP_GATT_AUTO_RSP;
    a.att_desc.uuid_length = uuid_len;
    a.att_desc.uuid_p = static_cast<uint8_t *>(const_cast<void *>(uuid));
    a.att_desc.perm = perm;
    a.att_desc.max_length = max_len;
    a.att_desc.length = len;
    a.att_desc.value = static_cast<uint8_t *>(const_cast<void *>(value));
    return a;
}

std::string addr_str(const esp_bd_addr_t a)
{
    char s[18];
    snprintf(s, sizeof(s), "%02X:%02X:%02X:%02X:%02X:%02X", a[0], a[1], a[2], a[3], a[4], a[5]);
    return s;
}

Conn *find(uint16_t conn_id)
{
    for (auto &c : conns) {
        if (c.used && c.conn_id == conn_id) {
            return &c;
        }
    }
    return nullptr;
}

// Connected phones, kept apart from the lock: advertising decisions are
// made from callbacks that don't hold it.
std::atomic<int> conn_count{0};
int connected() { return conn_count; }

void session_event(const std::string &session, const char *state)
{
    ESP_LOGI(TAG, "session %s %s", session.c_str(), state);
    if (control::subscribed(control::topic_state)) {
        cJSON *e = cJSON_CreateObject();
        cJSON_AddStringToObject(e, "event", "state");
        cJSON_AddStringToObject(e, "what", "session");
        cJSON_AddStringToObject(e, "object", session.c_str());
        cJSON_AddStringToObject(e, "state", state);
        control::publish(control::topic_state, e);
    }
}

// The flags and the KISS service in the advertisement, so KISS TNC apps find
// the device as before; the management service and as much of the name as
// fits in the scan response. The whole name is the GAP device name.
void configure_advertising()
{
    std::string name = settings::public_name();
    esp_ble_gap_set_device_name(name.c_str());

    uint8_t adv[31];
    size_t n = 0;
    adv[n++] = 2;
    adv[n++] = ESP_BLE_AD_TYPE_FLAG;
    adv[n++] = ESP_BLE_ADV_FLAG_GEN_DISC | ESP_BLE_ADV_FLAG_BREDR_NOT_SPT;
    adv[n++] = 17;
    adv[n++] = ESP_BLE_AD_TYPE_128SRV_CMPL;
    memcpy(&adv[n], svc_uuid, 16);
    n += 16;
    adv_data_set = false;
    esp_ble_gap_config_adv_data_raw(adv, n);

    uint8_t rsp[31];
    n = 0;
    rsp[n++] = 17;
    rsp[n++] = ESP_BLE_AD_TYPE_128SRV_CMPL;
    memcpy(&rsp[n], mgmt_uuid, 16);
    n += 16;
    size_t len = name.size();
    uint8_t type = ESP_BLE_AD_TYPE_NAME_CMPL;
    if (len > ADV_NAME_MAX) {
        // A shortened name, cut at a character boundary.
        len = ADV_NAME_MAX;
        while (len > 0 && (static_cast<uint8_t>(name[len]) & 0xC0) == 0x80) {
            len--;
        }
        type = ESP_BLE_AD_TYPE_NAME_SHORT;
    }
    rsp[n++] = static_cast<uint8_t>(len + 1);
    rsp[n++] = type;
    memcpy(&rsp[n], name.data(), len);
    n += len;
    scan_rsp_set = false;
    esp_ble_gap_config_scan_rsp_data_raw(rsp, n);
}

void start_advertising()
{
    if (adv_data_set && scan_rsp_set && connected() < MAX_CONNS) {
        esp_ble_gap_start_advertising(&adv_params);
    }
}

void gap_cb(esp_gap_ble_cb_event_t event, esp_ble_gap_cb_param_t *param)
{
    switch (event) {
    case ESP_GAP_BLE_ADV_DATA_RAW_SET_COMPLETE_EVT:
        adv_data_set = true;
        start_advertising();
        break;
    case ESP_GAP_BLE_SCAN_RSP_DATA_RAW_SET_COMPLETE_EVT:
        scan_rsp_set = true;
        start_advertising();
        break;
    case ESP_GAP_BLE_ADV_START_COMPLETE_EVT:
        if (param->adv_start_cmpl.status == ESP_BT_STATUS_SUCCESS) {
            if (!advertising) {
                ESP_LOGI(TAG, "advertising as \"%s\"", settings::public_name().c_str());
            }
            advertising = true;
            failed = false;
            errors::clear(errors::Error::phones);
            xEventGroupSetBits(events, EV_ADVERTISING);
        } else {
            ESP_LOGE(TAG, "advertising didn't start, status %d", param->adv_start_cmpl.status);
            fail("advertising didn't start");
        }
        break;
    case ESP_GAP_BLE_ADV_STOP_COMPLETE_EVT:
        advertising = false;
        break;
    case ESP_GAP_BLE_SEC_REQ_EVT:
        // A phone asked for encryption. Grant it, without bonding: the
        // KISS service doesn't need it, and bonds would pile up in NVS.
        esp_ble_gap_security_rsp(param->ble_security.ble_req.bd_addr, true);
        break;
    default:
        break;
    }
}

void on_write(esp_ble_gatts_cb_param_t *param)
{
    auto &w = param->write;
    if (w.is_prep) {
        if (w.need_rsp) {
            esp_ble_gatts_send_response(gatts_if_saved, w.conn_id, w.trans_id, ESP_GATT_REQ_NOT_SUPPORTED, nullptr);
        }
        return;
    }
    std::lock_guard<std::mutex> g(lock);
    Conn *c = find(w.conn_id);
    if (!c) {
        return;
    }
    if (w.handle == handles[IDX_TX_VAL]) {
        std::string session = c->session;
        c->splitter.feed(w.value, w.len, [&session](const uint8_t *frame, size_t len) {
            if (activity::wanted()) {
                activity::add(session, activity::Dir::rx, len);
            }
            radio::send_frame(frame, len, session);
        });
    } else if (w.handle == handles[IDX_RX_CCC] && w.len == 2) {
        bool on = w.value[0] & 0x01;
        if (on != c->subscribed) {
            c->subscribed = on;
            session_event(c->session, on ? "opened" : "closed");
        }
    }
}

void gatts_cb(esp_gatts_cb_event_t event, esp_gatt_if_t gatts_if, esp_ble_gatts_cb_param_t *param)
{
    switch (event) {
    case ESP_GATTS_REG_EVT:
        if (param->reg.status != ESP_GATT_OK) {
            ESP_LOGE(TAG, "GATT app didn't register, status %d", param->reg.status);
            fail("the GATT service didn't register");
            return;
        }
        gatts_if_saved = gatts_if;
        esp_ble_gatts_create_attr_tab(db, gatts_if, IDX_NB, 0);
        break;
    case ESP_GATTS_CREAT_ATTR_TAB_EVT:
        if (param->add_attr_tab.status != ESP_GATT_OK || param->add_attr_tab.num_handle != IDX_NB) {
            ESP_LOGE(TAG, "attribute table failed, status 0x%x", param->add_attr_tab.status);
            fail("the GATT service didn't start");
            return;
        }
        memcpy(handles, param->add_attr_tab.handles, sizeof(handles));
        esp_ble_gatts_start_service(handles[IDX_SVC]);
        configure_advertising();
        break;
    case ESP_GATTS_CONNECT_EVT: {
        std::lock_guard<std::mutex> g(lock);
        for (auto &c : conns) {
            if (!c.used) {
                c.used = true;
                c.conn_id = param->connect.conn_id;
                memcpy(c.bda, param->connect.remote_bda, sizeof(c.bda));
                c.mtu = 23;
                c.subscribed = false;
                c.session = "ktbr0/phones/" + addr_str(c.bda);
                c.splitter.reset();
                conn_count++;
                ESP_LOGI(TAG, "%s connected", addr_str(c.bda).c_str());
                break;
            }
        }
        advertising = false; // a connection ends advertising
        start_advertising();
        break;
    }
    case ESP_GATTS_DISCONNECT_EVT: {
        std::lock_guard<std::mutex> g(lock);
        if (Conn *c = find(param->disconnect.conn_id)) {
            ESP_LOGI(TAG, "%s disconnected, reason 0x%x", addr_str(c->bda).c_str(), param->disconnect.reason);
            if (c->subscribed) {
                session_event(c->session, "closed");
            }
            c->used = false;
            c->subscribed = false;
            conn_count--;
        }
        start_advertising();
        break;
    }
    case ESP_GATTS_MTU_EVT: {
        std::lock_guard<std::mutex> g(lock);
        if (Conn *c = find(param->mtu.conn_id)) {
            c->mtu = param->mtu.mtu;
        }
        break;
    }
    case ESP_GATTS_WRITE_EVT:
        on_write(param);
        break;
    default:
        break;
    }
}

} // namespace

void start()
{
    events = xEventGroupCreate();
    parse_uuid(KISS_SERVICE, svc_uuid);
    parse_uuid(KISS_TX, tx_uuid);
    parse_uuid(KISS_RX, rx_uuid);
    parse_uuid(MGMT_SERVICE, mgmt_uuid);
    db[IDX_SVC] = attr(ESP_UUID_LEN_16, &primary_service_uuid, ESP_GATT_PERM_READ, 16, 16, svc_uuid);
    db[IDX_TX_DECL] = attr(ESP_UUID_LEN_16, &char_decl_uuid, ESP_GATT_PERM_READ, 1, 1, &prop_write);
    db[IDX_TX_VAL] = attr(ESP_UUID_LEN_128, tx_uuid, ESP_GATT_PERM_WRITE, 512, sizeof(tx_init), tx_init);
    db[IDX_RX_DECL] = attr(ESP_UUID_LEN_16, &char_decl_uuid, ESP_GATT_PERM_READ, 1, 1, &prop_notify);
    // Notifications don't go through the stored value, so keep it tiny.
    db[IDX_RX_VAL] = attr(ESP_UUID_LEN_128, rx_uuid, ESP_GATT_PERM_READ, sizeof(rx_init), sizeof(rx_init), rx_init);
    db[IDX_RX_CCC] = attr(ESP_UUID_LEN_16, &ccc_uuid, ESP_GATT_PERM_READ | ESP_GATT_PERM_WRITE, 2, 2, ccc_init);

    adv_params.adv_int_min = 0xA0; // 100 ms
    adv_params.adv_int_max = 0xF0; // 150 ms
    adv_params.adv_type = ADV_TYPE_IND;
    adv_params.own_addr_type = BLE_ADDR_TYPE_PUBLIC;
    adv_params.channel_map = ADV_CHNL_ALL;
    adv_params.adv_filter_policy = ADV_FILTER_ALLOW_SCAN_ANY_CON_ANY;

    // Phones that ask for security get Just Works encryption without a
    // bond; nothing here needs more.
    esp_ble_io_cap_t iocap = ESP_IO_CAP_NONE;
    esp_ble_gap_set_security_param(ESP_BLE_SM_IOCAP_MODE, &iocap, sizeof(iocap));
    esp_ble_auth_req_t auth = ESP_LE_AUTH_NO_BOND;
    esp_ble_gap_set_security_param(ESP_BLE_SM_AUTHEN_REQ_MODE, &auth, sizeof(auth));

    if (esp_ble_gap_register_callback(gap_cb) != ESP_OK || esp_ble_gatts_register_callback(gatts_cb) != ESP_OK ||
        esp_ble_gatts_app_register(APP_ID) != ESP_OK || esp_ble_gatt_set_local_mtu(LOCAL_MTU) != ESP_OK) {
        fail("the BLE KISS service didn't start");
    }
}

bool wait_advertising(uint32_t timeout_ms)
{
    return xEventGroupWaitBits(events, EV_ADVERTISING, pdFALSE, pdFALSE, pdMS_TO_TICKS(timeout_ms)) &
           EV_ADVERTISING;
}

int sessions()
{
    std::lock_guard<std::mutex> g(lock);
    int n = 0;
    for (auto &c : conns) {
        n += (c.used && c.subscribed) ? 1 : 0;
    }
    return n;
}

int send_frame(const uint8_t *frame, size_t len)
{
    // FEND, the frame, FEND, as a phone expects a whole frame.
    uint8_t buf[kiss::Splitter::max_frame + 2];
    if (len > kiss::Splitter::max_frame) {
        return 0;
    }
    buf[0] = kiss::FEND;
    memcpy(buf + 1, frame, len);
    buf[len + 1] = kiss::FEND;
    size_t total = len + 2;

    std::lock_guard<std::mutex> g(lock);
    int delivered = 0;
    for (auto &c : conns) {
        if (!c.used || !c.subscribed) {
            continue;
        }
        size_t chunk = c.mtu > 3 ? c.mtu - 3 : 20;
        bool ok = true;
        for (size_t off = 0; off < total && ok; off += chunk) {
            size_t n = std::min(chunk, total - off);
            ok = esp_ble_gatts_send_indicate(gatts_if_saved, c.conn_id, handles[IDX_RX_VAL], n, buf + off,
                                             false) == ESP_OK;
        }
        if (ok) {
            delivered++;
            if (activity::wanted()) {
                activity::add(c.session, activity::Dir::tx, len);
            }
        } else {
            ESP_LOGW(TAG, "a frame to %s didn't go", c.session.c_str());
        }
    }
    return delivered;
}

void name_changed()
{
    // Advertising stops while its data changes, and starts again once both
    // parts are set.
    esp_ble_gap_stop_advertising();
    configure_advertising();
}

void kick(const control::Pending &p, const std::string &session)
{
    std::lock_guard<std::mutex> g(lock);
    for (auto &c : conns) {
        if (c.used && c.session == session) {
            esp_ble_gap_disconnect(c.bda);
            control::reply_ok(p, cJSON_CreateObject());
            return;
        }
    }
    control::reply_error(p, "not_found", "no session " + session);
}

cJSON *port_json()
{
    std::lock_guard<std::mutex> g(lock);
    cJSON *ph = cJSON_CreateObject();
    cJSON_AddStringToObject(ph, "id", "phones");
    cJSON_AddStringToObject(ph, "type", "ble-server");
    cJSON_AddStringToObject(ph, "role", "app");
    const char *state = failed ? "failed" : connected() >= MAX_CONNS ? "full" : advertising ? "advertising" : "stopped";
    cJSON_AddStringToObject(ph, "state", state);
    if (failed) {
        cJSON_AddStringToObject(ph, "detail", failure.c_str());
    }
    cJSON *s = cJSON_AddObjectToObject(ph, "settings");
    cJSON_AddStringToObject(s, "adapter", "builtin");
    cJSON_AddNumberToObject(s, "max_clients", MAX_CONNS);
    cJSON *sessions = cJSON_AddArrayToObject(ph, "sessions");
    for (auto &c : conns) {
        if (c.used && c.subscribed) {
            cJSON *one = cJSON_CreateObject();
            cJSON_AddStringToObject(one, "id", c.session.c_str());
            cJSON_AddItemToArray(sessions, one);
        }
    }
    return ph;
}

} // namespace ktb::phones
