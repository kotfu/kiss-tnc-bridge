// The phone side: the BLE KISS API service and a stand-in management
// service on one GATT server, advertising, and per-connection state.

#include <cinttypes>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <vector>

#include "esp_bt_defs.h"
#include "esp_gap_ble_api.h"
#include "esp_gatt_common_api.h"
#include "esp_gatts_api.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_timer.h"

#include "spike.h"

namespace spike {
namespace {

constexpr const char *TAG = "ble";

// BLE KISS API (https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md),
// the same UUIDs as the daemon's ble-server ports.
constexpr const char *KISS_SERVICE = "00000001-ba2a-46c9-ae49-01b0961f68bb";
constexpr const char *KISS_TX = "00000002-ba2a-46c9-ae49-01b0961f68bb";  // phone writes
constexpr const char *KISS_RX = "00000003-ba2a-46c9-ae49-01b0961f68bb";  // phone subscribes

// Placeholder management service. Same shape as the planned one (write
// characteristic, notify characteristic, length-prefixed messages in
// MTU-sized chunks), made-up UUIDs.
constexpr const char *MGMT_SERVICE = "4b544201-9b3e-4a51-8d36-5c3a1f7e0001";
constexpr const char *MGMT_TX = "4b544201-9b3e-4a51-8d36-5c3a1f7e0002";
constexpr const char *MGMT_RX = "4b544201-9b3e-4a51-8d36-5c3a1f7e0003";

constexpr uint16_t APP_ID = 0x55;
constexpr uint16_t LOCAL_MTU = 517;
constexpr int MAX_CONNS = CONFIG_BTDM_CTRL_BLE_MAX_CONN;

enum Idx { IDX_SVC, IDX_TX_DECL, IDX_TX_VAL, IDX_RX_DECL, IDX_RX_VAL, IDX_RX_CCC, IDX_NB };

struct ServiceDef {
    uint8_t svc_uuid[16];
    uint8_t tx_uuid[16];
    uint8_t rx_uuid[16];
    uint16_t handles[IDX_NB];
    esp_gatts_attr_db_t db[IDX_NB];
};

ServiceDef kiss_svc;
ServiceDef mgmt_svc;

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
    uint16_t mtu = 23;
    bool kiss_notify = false;
    bool mgmt_notify = false;
    std::vector<uint8_t> mgmt_rx;
};

std::mutex conns_mutex;
Conn conns[MAX_CONNS];
esp_gatt_if_t gatts_if_saved = ESP_GATT_IF_NONE;
int tables_created = 0;
bool adv_data_set = false;
bool scan_rsp_set = false;

esp_ble_adv_params_t adv_params = {};

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

void build_service(ServiceDef &s, const char *svc, const char *tx, const char *rx)
{
    parse_uuid(svc, s.svc_uuid);
    parse_uuid(tx, s.tx_uuid);
    parse_uuid(rx, s.rx_uuid);
    s.db[IDX_SVC] = attr(ESP_UUID_LEN_16, &primary_service_uuid, ESP_GATT_PERM_READ, 16, 16, s.svc_uuid);
    s.db[IDX_TX_DECL] = attr(ESP_UUID_LEN_16, &char_decl_uuid, ESP_GATT_PERM_READ, 1, 1, &prop_write);
    s.db[IDX_TX_VAL] = attr(ESP_UUID_LEN_128, s.tx_uuid, ESP_GATT_PERM_WRITE, 512, sizeof(tx_init), tx_init);
    s.db[IDX_RX_DECL] = attr(ESP_UUID_LEN_16, &char_decl_uuid, ESP_GATT_PERM_READ, 1, 1, &prop_notify);
    // Notifications don't go through the stored value, so keep it tiny.
    s.db[IDX_RX_VAL] = attr(ESP_UUID_LEN_128, s.rx_uuid, ESP_GATT_PERM_READ, sizeof(rx_init), sizeof(rx_init),
                            rx_init);
    s.db[IDX_RX_CCC] = attr(ESP_UUID_LEN_16, &ccc_uuid, ESP_GATT_PERM_READ | ESP_GATT_PERM_WRITE, 2, 2, ccc_init);
}

Conn *find_conn(uint16_t conn_id)
{
    for (auto &c : conns) {
        if (c.used && c.conn_id == conn_id) {
            return &c;
        }
    }
    return nullptr;
}

// Sends len bytes as notifications of at most MTU-3 bytes each.
void notify_chunked(const Conn &c, uint16_t handle, const uint8_t *data, size_t len)
{
    size_t chunk = c.mtu > 3 ? c.mtu - 3 : 20;
    while (len > 0) {
        size_t n = len < chunk ? len : chunk;
        if (esp_ble_gatts_send_indicate(gatts_if_saved, c.conn_id, handle, n, const_cast<uint8_t *>(data),
                                        false) != ESP_OK) {
            stats.notify_failures++;
            return;
        }
        data += n;
        len -= n;
    }
}

void start_advertising()
{
    esp_ble_gap_start_advertising(&adv_params);
}

void configure_advertising()
{
    // Advertising data: flags and the KISS service UUID (3 + 18 bytes).
    uint8_t adv[31];
    size_t n = 0;
    adv[n++] = 2;
    adv[n++] = ESP_BLE_AD_TYPE_FLAG;
    adv[n++] = ESP_BLE_ADV_FLAG_GEN_DISC | ESP_BLE_ADV_FLAG_BREDR_NOT_SPT;
    adv[n++] = 17;
    adv[n++] = ESP_BLE_AD_TYPE_128SRV_CMPL;
    memcpy(&adv[n], kiss_svc.svc_uuid, 16);
    n += 16;
    esp_ble_gap_config_adv_data_raw(adv, n);

    // Scan response: the name.
    uint8_t rsp[31];
    const char *name = CONFIG_SPIKE_DEVICE_NAME;
    size_t name_len = strlen(name);
    if (name_len > 29) {
        name_len = 29;
    }
    rsp[0] = static_cast<uint8_t>(name_len + 1);
    rsp[1] = ESP_BLE_AD_TYPE_NAME_CMPL;
    memcpy(&rsp[2], name, name_len);
    esp_ble_gap_config_scan_rsp_data_raw(rsp, name_len + 2);
}

void gap_cb(esp_gap_ble_cb_event_t event, esp_ble_gap_cb_param_t *param)
{
    switch (event) {
    case ESP_GAP_BLE_ADV_DATA_RAW_SET_COMPLETE_EVT:
        adv_data_set = true;
        if (scan_rsp_set) {
            start_advertising();
        }
        break;
    case ESP_GAP_BLE_SCAN_RSP_DATA_RAW_SET_COMPLETE_EVT:
        scan_rsp_set = true;
        if (adv_data_set) {
            start_advertising();
        }
        break;
    case ESP_GAP_BLE_ADV_START_COMPLETE_EVT:
        if (param->adv_start_cmpl.status != ESP_BT_STATUS_SUCCESS) {
            ESP_LOGE(TAG, "advertising failed to start, status %d", param->adv_start_cmpl.status);
        } else {
            ESP_LOGI(TAG, "advertising as '%s'", CONFIG_SPIKE_DEVICE_NAME);
        }
        break;
    case ESP_GAP_BLE_SEC_REQ_EVT:
        // A phone asked to pair. Accept; nothing here requires it.
        esp_ble_gap_security_rsp(param->ble_security.ble_req.bd_addr, true);
        break;
    default:
        break;
    }
}

// A complete management message: answer with the heap numbers, framed the
// same way (4-byte big-endian length, then the body).
void handle_mgmt_message(Conn &c, const uint8_t *body, size_t len)
{
    stats.mgmt_messages++;
    multi_heap_info_t info;
    heap_caps_get_info(&info, MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    char json[200];
    int n = snprintf(json, sizeof(json),
                     "{\"echo_len\":%u,\"heap_free\":%u,\"heap_min\":%u,\"heap_largest\":%u,\"uptime_s\":%" PRIu64 "}",
                     (unsigned)len, (unsigned)info.total_free_bytes, (unsigned)info.minimum_free_bytes,
                     (unsigned)info.largest_free_block, esp_timer_get_time() / 1000000);
    std::vector<uint8_t> out(4 + n);
    out[0] = (n >> 24) & 0xFF;
    out[1] = (n >> 16) & 0xFF;
    out[2] = (n >> 8) & 0xFF;
    out[3] = n & 0xFF;
    memcpy(&out[4], json, n);
    if (c.mgmt_notify) {
        notify_chunked(c, mgmt_svc.handles[IDX_RX_VAL], out.data(), out.size());
    }
}

void on_mgmt_write(Conn &c, const uint8_t *data, size_t len)
{
    c.mgmt_rx.insert(c.mgmt_rx.end(), data, data + len);
    while (c.mgmt_rx.size() >= 4) {
        uint32_t body_len = (uint32_t(c.mgmt_rx[0]) << 24) | (uint32_t(c.mgmt_rx[1]) << 16) |
                            (uint32_t(c.mgmt_rx[2]) << 8) | c.mgmt_rx[3];
        if (body_len > CONFIG_SPIKE_MGMT_MAX_MESSAGE) {
            ESP_LOGW(TAG, "mgmt: message of %" PRIu32 " bytes is over the limit, discarding", body_len);
            c.mgmt_rx.clear();
            return;
        }
        if (c.mgmt_rx.size() < 4 + body_len) {
            return;
        }
        handle_mgmt_message(c, c.mgmt_rx.data() + 4, body_len);
        c.mgmt_rx.erase(c.mgmt_rx.begin(), c.mgmt_rx.begin() + 4 + body_len);
    }
}

void update_subscriber_count()
{
    int n = 0;
    for (auto &c : conns) {
        if (c.used && c.kiss_notify) {
            n++;
        }
    }
    stats.kiss_subscribers = n;
}

void on_write(esp_ble_gatts_cb_param_t *param)
{
    auto &w = param->write;
    if (w.is_prep) {
        // Long writes: phones send write-without-response chunks instead.
        ESP_LOGW(TAG, "prepared write ignored (handle %u)", w.handle);
        if (w.need_rsp) {
            esp_ble_gatts_send_response(gatts_if_saved, w.conn_id, w.trans_id, ESP_GATT_REQ_NOT_SUPPORTED, nullptr);
        }
        return;
    }
    std::lock_guard<std::mutex> lock(conns_mutex);
    Conn *c = find_conn(w.conn_id);
    if (!c) {
        return;
    }
    if (w.handle == kiss_svc.handles[IDX_TX_VAL]) {
        size_t sent = xStreamBufferSend(to_radio, w.value, w.len, 0);
        if (sent < w.len) {
            stats.dropped_to_radio += w.len - sent;
        }
    } else if (w.handle == kiss_svc.handles[IDX_RX_CCC] && w.len == 2) {
        c->kiss_notify = w.value[0] & 0x01;
        ESP_LOGI(TAG, "conn %u %s KISS notifications", w.conn_id, c->kiss_notify ? "enabled" : "disabled");
        update_subscriber_count();
        heap_mark("kiss subscribed");
    } else if (w.handle == mgmt_svc.handles[IDX_TX_VAL]) {
        on_mgmt_write(*c, w.value, w.len);
    } else if (w.handle == mgmt_svc.handles[IDX_RX_CCC] && w.len == 2) {
        c->mgmt_notify = w.value[0] & 0x01;
        ESP_LOGI(TAG, "conn %u %s management notifications", w.conn_id, c->mgmt_notify ? "enabled" : "disabled");
    }
}

void gatts_cb(esp_gatts_cb_event_t event, esp_gatt_if_t gatts_if, esp_ble_gatts_cb_param_t *param)
{
    switch (event) {
    case ESP_GATTS_REG_EVT:
        if (param->reg.status != ESP_GATT_OK) {
            ESP_LOGE(TAG, "app register failed, status %d", param->reg.status);
            return;
        }
        gatts_if_saved = gatts_if;
        esp_ble_gap_set_device_name(CONFIG_SPIKE_DEVICE_NAME);
        esp_ble_gatts_create_attr_tab(kiss_svc.db, gatts_if, IDX_NB, 0);
        break;
    case ESP_GATTS_CREAT_ATTR_TAB_EVT: {
        if (param->add_attr_tab.status != ESP_GATT_OK || param->add_attr_tab.num_handle != IDX_NB) {
            ESP_LOGE(TAG, "attribute table failed, status 0x%x, %u handles", param->add_attr_tab.status,
                     param->add_attr_tab.num_handle);
            return;
        }
        ServiceDef &s = tables_created == 0 ? kiss_svc : mgmt_svc;
        memcpy(s.handles, param->add_attr_tab.handles, sizeof(s.handles));
        esp_ble_gatts_start_service(s.handles[IDX_SVC]);
        if (++tables_created == 1) {
            esp_ble_gatts_create_attr_tab(mgmt_svc.db, gatts_if, IDX_NB, 1);
        } else {
            heap_mark("gatt services");
            configure_advertising();
        }
        break;
    }
    case ESP_GATTS_CONNECT_EVT: {
        std::lock_guard<std::mutex> lock(conns_mutex);
        for (auto &c : conns) {
            if (!c.used) {
                c = Conn{};
                c.used = true;
                c.conn_id = param->connect.conn_id;
                // Reserve the management reassembly buffer up front, as the
                // real service would, so the heap numbers include it.
                c.mgmt_rx.reserve(CONFIG_SPIKE_MGMT_MAX_MESSAGE + 4);
                break;
            }
        }
        int n = ++stats.ble_connections;
        ESP_LOGI(TAG, "conn %u connected, %d of %d", param->connect.conn_id, n, MAX_CONNS);
        heap_mark("ble connected");
        if (n < MAX_CONNS) {
            start_advertising();
        }
        break;
    }
    case ESP_GATTS_DISCONNECT_EVT: {
        std::lock_guard<std::mutex> lock(conns_mutex);
        if (Conn *c = find_conn(param->disconnect.conn_id)) {
            *c = Conn{};
        }
        update_subscriber_count();
        int n = --stats.ble_connections;
        ESP_LOGI(TAG, "conn %u disconnected, reason 0x%x, %d left", param->disconnect.conn_id,
                 param->disconnect.reason, n);
        heap_mark("ble disconnected");
        start_advertising();
        break;
    }
    case ESP_GATTS_MTU_EVT: {
        std::lock_guard<std::mutex> lock(conns_mutex);
        if (Conn *c = find_conn(param->mtu.conn_id)) {
            c->mtu = param->mtu.mtu;
        }
        ESP_LOGI(TAG, "conn %u MTU %u", param->mtu.conn_id, param->mtu.mtu);
        break;
    }
    case ESP_GATTS_WRITE_EVT:
        on_write(param);
        break;
    case ESP_GATTS_CONGEST_EVT:
        ESP_LOGW(TAG, "conn %u congested %d", param->congest.conn_id, param->congest.congested);
        break;
    default:
        break;
    }
}

} // namespace

void ble_send_kiss(const uint8_t *data, size_t len)
{
    std::lock_guard<std::mutex> lock(conns_mutex);
    for (auto &c : conns) {
        if (c.used && c.kiss_notify) {
            notify_chunked(c, kiss_svc.handles[IDX_RX_VAL], data, len);
        }
    }
}

void ble_start()
{
    build_service(kiss_svc, KISS_SERVICE, KISS_TX, KISS_RX);
    build_service(mgmt_svc, MGMT_SERVICE, MGMT_TX, MGMT_RX);

    adv_params.adv_int_min = 0xA0;  // 100 ms
    adv_params.adv_int_max = 0xF0;  // 150 ms
    adv_params.adv_type = ADV_TYPE_IND;
    adv_params.own_addr_type = BLE_ADDR_TYPE_PUBLIC;
    adv_params.channel_map = ADV_CHNL_ALL;
    adv_params.adv_filter_policy = ADV_FILTER_ALLOW_SCAN_ANY_CON_ANY;

    // Phones pair Just Works if they ask at all; the real firmware
    // authenticates inside the management protocol instead.
    esp_ble_io_cap_t iocap = ESP_IO_CAP_NONE;
    esp_ble_gap_set_security_param(ESP_BLE_SM_IOCAP_MODE, &iocap, sizeof(iocap));

    ESP_ERROR_CHECK(esp_ble_gap_register_callback(gap_cb));
    ESP_ERROR_CHECK(esp_ble_gatts_register_callback(gatts_cb));
    ESP_ERROR_CHECK(esp_ble_gatts_app_register(APP_ID));
    ESP_ERROR_CHECK(esp_ble_gatt_set_local_mtu(LOCAL_MTU));
}

} // namespace spike
