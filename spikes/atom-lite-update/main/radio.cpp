// The radio side: SPP client to a TH-D74/D75, its KISS init, and the
// writer that drains BLE writes to the radio.

#include <cinttypes>
#include <cstdio>
#include <cstring>
#include <string>

#include "driver/gpio.h"
#include "esp_bt_device.h"
#include "esp_gap_bt_api.h"
#include "esp_log.h"
#include "esp_spp_api.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

#include "spike.h"

// Bluedroid internals (bta_sys.h isn't a public header). Clearing
// HCI_ENABLE_SNIFF_MODE from a link's policy stops the power manager from
// putting it in sniff mode, and wakes it if it's already there.
extern "C" void bta_sys_clear_policy(uint8_t id, uint8_t policy, uint8_t *peer_addr);
constexpr uint8_t HCI_ENABLE_SNIFF_MODE = 0x04;

namespace spike {
namespace {

constexpr const char *TAG = "radio";

constexpr TickType_t retry_delay = pdMS_TO_TICKS(15000);  // bb-link's reconnect interval
constexpr TickType_t connect_timeout = pdMS_TO_TICKS(45000);  // allows for pressing OK when pairing
constexpr TickType_t reply_timeout = pdMS_TO_TICKS(1000);
constexpr TickType_t write_timeout = pdMS_TO_TICKS(5000);
constexpr gpio_num_t button_gpio = GPIO_NUM_39;  // ATOM Lite button, active low

enum : EventBits_t {
    EV_INIT = 1 << 0,
    EV_OPEN = 1 << 1,
    EV_CLOSED = 1 << 2,
    EV_FAILED = 1 << 3,
    EV_UNCONGESTED = 1 << 4,
    EV_PAIRED = 1 << 5,
};

struct WriteResult {
    esp_spp_status_t status;
    int len;
    bool cong;
};

esp_bd_addr_t radio_addr;
std::atomic<uint32_t> spp_handle{0};
std::atomic<bool> congested{false};
std::atomic<bool> paused{false};
EventGroupHandle_t events;
QueueHandle_t write_results;
SemaphoreHandle_t write_mutex;
FrameCounter rx_frames;
FrameCounter tx_frames;

bool parse_addr(const char *text, esp_bd_addr_t out)
{
    unsigned b[6];
    if (sscanf(text, "%x:%x:%x:%x:%x:%x", &b[0], &b[1], &b[2], &b[3], &b[4], &b[5]) != 6) {
        return false;
    }
    for (int i = 0; i < 6; i++) {
        out[i] = static_cast<uint8_t>(b[i]);
    }
    return true;
}

void gap_cb(esp_bt_gap_cb_event_t event, esp_bt_gap_cb_param_t *param)
{
    switch (event) {
    case ESP_BT_GAP_CFM_REQ_EVT:
        // SSP numeric comparison: the D75 shows the same six digits and the
        // operator presses OK on the radio. Our side confirms at once.
        ESP_LOGW(TAG, "pairing: confirm %06" PRIu32 " -- press OK on the radio", param->cfm_req.num_val);
        esp_bt_gap_ssp_confirm_reply(param->cfm_req.bda, true);
        break;
    case ESP_BT_GAP_KEY_NOTIF_EVT:
        ESP_LOGW(TAG, "pairing: passkey %06" PRIu32, param->key_notif.passkey);
        break;
    case ESP_BT_GAP_PIN_REQ_EVT: {
        ESP_LOGW(TAG, "pairing: legacy PIN requested, answering 0000");
        esp_bt_pin_code_t pin = {'0', '0', '0', '0'};
        esp_bt_gap_pin_reply(param->pin_req.bda, true, 4, pin);
        break;
    }
    case ESP_BT_GAP_AUTH_CMPL_EVT:
        if (param->auth_cmpl.stat == ESP_BT_STATUS_SUCCESS) {
            ESP_LOGI(TAG, "pairing: authenticated with %s", param->auth_cmpl.device_name);
            xEventGroupSetBits(events, EV_PAIRED);
        } else {
            ESP_LOGE(TAG, "pairing: failed, status %d", param->auth_cmpl.stat);
        }
        break;
    case ESP_BT_GAP_MODE_CHG_EVT:
        // Logged so sniff transitions can be matched against what the radio
        // shows (the STA icon, for one).
        ESP_LOGI(TAG, "link mode: %s", param->mode_chg.mode == ESP_BT_PM_MD_ACTIVE  ? "active"
                                       : param->mode_chg.mode == ESP_BT_PM_MD_SNIFF ? "sniff"
                                                                                    : "other");
        break;
    default:
        break;
    }
}

void spp_cb(esp_spp_cb_event_t event, esp_spp_cb_param_t *p)
{
    switch (event) {
    case ESP_SPP_INIT_EVT:
        ESP_LOGI(TAG, "SPP init status %d", p->init.status);
        if (p->init.status == ESP_SPP_SUCCESS) {
            xEventGroupSetBits(events, EV_INIT);
        }
        break;
    case ESP_SPP_DISCOVERY_COMP_EVT:
        if (p->disc_comp.status == ESP_SPP_SUCCESS && p->disc_comp.scn_num > 0) {
            ESP_LOGI(TAG, "SDP found SPP on channel %u (%s)", p->disc_comp.scn[0],
                     p->disc_comp.service_name[0] ? p->disc_comp.service_name[0] : "");
            esp_spp_connect(ESP_SPP_SEC_AUTHENTICATE, ESP_SPP_ROLE_MASTER, p->disc_comp.scn[0], radio_addr);
        } else {
            ESP_LOGW(TAG, "SDP failed, status %d", p->disc_comp.status);
            xEventGroupSetBits(events, EV_FAILED);
        }
        break;
    case ESP_SPP_CL_INIT_EVT:
        if (p->cl_init.status != ESP_SPP_SUCCESS) {
            ESP_LOGW(TAG, "SPP connect failed to start, status %d", p->cl_init.status);
            xEventGroupSetBits(events, EV_FAILED);
        }
        break;
    case ESP_SPP_OPEN_EVT:
        if (p->open.status == ESP_SPP_SUCCESS) {
            spp_handle = p->open.handle;
            congested = false;
#if CONFIG_SPIKE_NO_SNIFF
            bta_sys_clear_policy(0, HCI_ENABLE_SNIFF_MODE, radio_addr);
            ESP_LOGI(TAG, "sniff mode disabled on the radio link");
#endif
            xEventGroupSetBits(events, EV_OPEN | EV_UNCONGESTED);
        } else {
            ESP_LOGW(TAG, "SPP open failed, status %d", p->open.status);
            xEventGroupSetBits(events, EV_FAILED);
        }
        break;
    case ESP_SPP_CLOSE_EVT: {
        ESP_LOGI(TAG, "SPP closed, status %d, by remote %d", p->close.status, p->close.async);
        spp_handle = 0;
        WriteResult r{ESP_SPP_FAILURE, 0, false};
        xQueueOverwrite(write_results, &r);
        xEventGroupSetBits(events, EV_CLOSED | EV_UNCONGESTED);
        break;
    }
    case ESP_SPP_DATA_IND_EVT: {
        size_t len = p->data_ind.len;
        stats.radio_rx_bytes += len;
        size_t sent = xStreamBufferSend(from_radio, p->data_ind.data, len, 0);
        if (sent < len) {
            stats.dropped_from_radio += len - sent;
        }
        break;
    }
    case ESP_SPP_WRITE_EVT: {
        congested = p->write.cong;
        if (p->write.cong) {
            xEventGroupClearBits(events, EV_UNCONGESTED);
        }
        WriteResult r{p->write.status, p->write.len, p->write.cong};
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
        if (congested && !(xEventGroupWaitBits(events, EV_UNCONGESTED, pdFALSE, pdFALSE, write_timeout) &
                           EV_UNCONGESTED)) {
            ESP_LOGW(TAG, "write: still congested after %u ms", (unsigned)pdTICKS_TO_MS(write_timeout));
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
            ESP_LOGW(TAG, "write: no completion event");
            ok = false;
            break;
        }
        if (r.status != ESP_SPP_SUCCESS) {
            // Not sent at all. Retry a couple of times if it was congestion.
            if (spp_handle != 0 && ++failures <= 3) {
                continue;
            }
            ESP_LOGW(TAG, "write: failed, status %d", r.status);
            ok = false;
            break;
        }
        data += r.len;
        len -= r.len;
        stats.radio_tx_bytes += r.len;
    }
    xSemaphoreGive(write_mutex);
    return ok;
}

// ---- KISS init (the bb-link sequence, see spikes/FINDINGS.md) ----

std::string pending;  // bytes read from the radio but not yet consumed

void flush_input()
{
    pending.clear();
    xStreamBufferReset(from_radio);
}

// Reads one CR-terminated reply.
bool read_line(std::string &line, TickType_t timeout)
{
    TickType_t start = xTaskGetTickCount();
    for (;;) {
        size_t cr = pending.find('\r');
        if (cr != std::string::npos) {
            line = pending.substr(0, cr);
            pending.erase(0, cr + 1);
            return true;
        }
        TickType_t elapsed = xTaskGetTickCount() - start;
        if (elapsed >= timeout) {
            return false;
        }
        uint8_t buf[64];
        size_t n = xStreamBufferReceive(from_radio, buf, sizeof(buf), timeout - elapsed);
        pending.append(reinterpret_cast<char *>(buf), n);
    }
}

// Sends a command and waits for a reply that echoes its two letters.
// Retries up to three times, 200 ms apart, on a '?' error reply.
bool command(const std::string &cmd, std::string &reply)
{
    std::string wire = cmd + "\r";
    for (int attempt = 0; attempt < 3; attempt++) {
        if (attempt > 0) {
            vTaskDelay(pdMS_TO_TICKS(200));
        }
        flush_input();
        if (!spp_write_all(reinterpret_cast<const uint8_t *>(wire.data()), wire.size())) {
            return false;
        }
        if (!read_line(reply, reply_timeout)) {
            return false;  // silence: no point retrying
        }
        ESP_LOGI(TAG, "init: %s -> %s", cmd.c_str(), reply.c_str());
        if (reply.compare(0, 2, cmd, 0, 2) == 0) {
            return true;
        }
    }
    return false;
}

bool kiss_init()
{
    std::string reply;
    if (!command("BT", reply)) {
        ESP_LOGI(TAG, "init: no reply to BT, the TNC is in KISS mode; leaving it");
        static const uint8_t kiss_return[] = {0xC0, 0xFF, 0xC0};
        if (!spp_write_all(kiss_return, sizeof(kiss_return))) {
            return false;
        }
        vTaskDelay(pdMS_TO_TICKS(300));
        if (!command("BT", reply)) {
            ESP_LOGE(TAG, "init: still no reply to BT");
            return false;
        }
    }
    if (command("ID", reply)) {
        ESP_LOGI(TAG, "init: radio is %s", reply.c_str());
    }
    if (!command("TN", reply)) {
        return false;
    }
    int mode = 0;
    int band = 0;
    if (sscanf(reply.c_str(), "TN %d,%d", &mode, &band) != 2) {
        ESP_LOGE(TAG, "init: can't parse '%s'", reply.c_str());
        return false;
    }
    if (!command("TN 2," + std::to_string(band), reply)) {
        return false;
    }
    ESP_LOGI(TAG, "init: KISS mode on band %c", band == 0 ? 'A' : 'B');
    return true;
}

void forward_from_radio(const uint8_t *data, size_t len)
{
    stats.radio_rx_frames += rx_frames.feed(data, len);
    ble_send_kiss(data, len);
}

void bridge()
{
    stats.radio_bridging = true;
    heap_mark("radio bridging");
    if (!pending.empty()) {
        forward_from_radio(reinterpret_cast<const uint8_t *>(pending.data()), pending.size());
        pending.clear();
    }
    uint8_t buf[256];
    while (!(xEventGroupGetBits(events) & EV_CLOSED)) {
        size_t n = xStreamBufferReceive(from_radio, buf, sizeof(buf), pdMS_TO_TICKS(200));
        if (n > 0) {
            forward_from_radio(buf, n);
        }
    }
    stats.radio_bridging = false;
}

bool radio_bonded()
{
    int n = esp_bt_gap_get_bond_device_num();
    if (n <= 0) {
        return false;
    }
    esp_bd_addr_t list[n];
    esp_bt_gap_get_bond_device_list(&n, list);
    for (int i = 0; i < n; i++) {
        if (memcmp(list[i], radio_addr, sizeof(esp_bd_addr_t)) == 0) {
            return true;
        }
    }
    return false;
}

// Not paired yet: stay discoverable so the radio's Pairing Mode search can
// find us, and start pairing from our side only when the button is pressed.
// Retrying on a timer piles prompts onto the radio, and a new attempt while
// a stale prompt is up fails (spikes/FINDINGS.md, Pairing).
void wait_for_pairing()
{
    esp_bt_gap_set_scan_mode(ESP_BT_CONNECTABLE, ESP_BT_GENERAL_DISCOVERABLE);
    ESP_LOGW(TAG, "not paired with %s. Put the radio in Pairing Mode, then either pick '%s' on the radio "
                  "or press the ATOM's button to start pairing from here.",
             CONFIG_SPIKE_RADIO_ADDR, CONFIG_SPIKE_DEVICE_NAME);
    xEventGroupClearBits(events, EV_PAIRED);
    for (;;) {
        if (xEventGroupWaitBits(events, EV_PAIRED, pdFALSE, pdFALSE, pdMS_TO_TICKS(50)) & EV_PAIRED) {
            ESP_LOGI(TAG, "paired from the radio's side");
            vTaskDelay(pdMS_TO_TICKS(2000));  // let the radio finish with the pairing link
            break;
        }
        if (gpio_get_level(button_gpio) == 0) {
            ESP_LOGI(TAG, "button pressed, pairing from here");
            break;
        }
    }
    esp_bt_gap_set_scan_mode(ESP_BT_NON_CONNECTABLE, ESP_BT_NON_DISCOVERABLE);
}

void radio_task(void *)
{
    xEventGroupWaitBits(events, EV_INIT, pdFALSE, pdFALSE, portMAX_DELAY);
    for (;;) {
        if (paused) {
            vTaskDelay(pdMS_TO_TICKS(500));
            continue;
        }
        if (!radio_bonded()) {
            wait_for_pairing();
        }
        xEventGroupClearBits(events, EV_OPEN | EV_CLOSED | EV_FAILED);
        ESP_LOGI(TAG, "connecting to %s", CONFIG_SPIKE_RADIO_ADDR);
        esp_spp_start_discovery(radio_addr);
        EventBits_t bits = xEventGroupWaitBits(events, EV_OPEN | EV_FAILED | EV_CLOSED, pdFALSE, pdFALSE,
                                               connect_timeout);
        if (bits & EV_OPEN) {
            stats.radio_connects++;
            heap_mark("radio connected");
            if (kiss_init()) {
                bridge();
            } else {
                ESP_LOGE(TAG, "init failed, disconnecting");
                esp_spp_disconnect(spp_handle);
                xEventGroupWaitBits(events, EV_CLOSED, pdFALSE, pdFALSE, pdMS_TO_TICKS(5000));
            }
            heap_mark("radio disconnected");
            if (paused) {
                continue;
            }
        } else if (!(bits & (EV_FAILED | EV_CLOSED))) {
            ESP_LOGW(TAG, "no connection after %u s", (unsigned)pdTICKS_TO_MS(connect_timeout) / 1000);
        }
        vTaskDelay(retry_delay);
    }
}

// Drains BLE KISS writes to the radio. Writes that arrive while the radio
// isn't bridging are dropped, as the daemon does.
void writer_task(void *)
{
    uint8_t buf[512];
    for (;;) {
        size_t n = xStreamBufferReceive(to_radio, buf, sizeof(buf), portMAX_DELAY);
        if (!stats.radio_bridging) {
            stats.dropped_to_radio += n;
            continue;
        }
        stats.radio_tx_frames += tx_frames.feed(buf, n);
        if (!spp_write_all(buf, n)) {
            stats.dropped_to_radio += n;
        }
    }
}

} // namespace

void radio_pause(bool p)
{
    paused = p;
    ESP_LOGI(TAG, "radio %s", p ? "paused" : "resumed");
    if (p && spp_handle != 0) {
        esp_spp_disconnect(spp_handle);
    }
}

void radio_start()
{
    if (!parse_addr(CONFIG_SPIKE_RADIO_ADDR, radio_addr)) {
        ESP_LOGE(TAG, "bad radio address '%s'", CONFIG_SPIKE_RADIO_ADDR);
        return;
    }
    gpio_config_t button = {};
    button.pin_bit_mask = 1ULL << button_gpio;
    button.mode = GPIO_MODE_INPUT;  // GPIO39 is input-only; the board has the pull-up
    gpio_config(&button);

    events = xEventGroupCreate();
    write_results = xQueueCreate(1, sizeof(WriteResult));
    write_mutex = xSemaphoreCreateMutex();

    ESP_ERROR_CHECK(esp_bt_gap_register_callback(gap_cb));
    esp_bt_io_cap_t iocap = ESP_BT_IO_CAP_IO;  // DisplayYesNo: numeric comparison
    esp_bt_gap_set_security_param(ESP_BT_SP_IOCAP_MODE, &iocap, sizeof(iocap));
    esp_bt_pin_code_t pin = {};
    esp_bt_gap_set_pin(ESP_BT_PIN_TYPE_VARIABLE, 0, pin);
    esp_bt_gap_set_device_name(CONFIG_SPIKE_DEVICE_NAME);
    // We only initiate; nothing connects to us over Classic.
    esp_bt_gap_set_scan_mode(ESP_BT_NON_CONNECTABLE, ESP_BT_NON_DISCOVERABLE);

    ESP_ERROR_CHECK(esp_spp_register_callback(spp_cb));
    esp_spp_cfg_t cfg = {};
    cfg.mode = ESP_SPP_MODE_CB;
    cfg.enable_l2cap_ertm = true;
    cfg.tx_buffer_size = 0;  // VFS mode only
    ESP_ERROR_CHECK(esp_spp_enhanced_init(&cfg));

    const uint8_t *own = esp_bt_dev_get_address();
    ESP_LOGI(TAG, "own address %02x:%02x:%02x:%02x:%02x:%02x", own[0], own[1], own[2], own[3], own[4],
             own[5]);

    xTaskCreate(radio_task, "radio", 4096, nullptr, 5, nullptr);
    xTaskCreate(writer_task, "radio_tx", 3072, nullptr, 5, nullptr);
}

} // namespace spike
