// Firmware updates over BLE: the management commands, the image data path,
// and marking a new image valid on its first boot.
//
// Image bytes arrive as writes without response on the data characteristic,
// each one a 4-byte little-endian offset and then the bytes. The Bluetooth
// task only checks the offset and queues the bytes in a ring buffer; this
// module's task writes them to flash and acknowledges every few KB, so a
// client keeps at most a window of unacknowledged data in flight and the
// buffer can't overflow while flash is busy erasing. A write at an
// unexpected offset (or one that doesn't fit) is dropped and answered with a
// "nak" naming the offset to resend from.

#include <atomic>
#include <cinttypes>
#include <cstdarg>
#include <cstdio>
#include <cstring>

#include "cJSON.h"
#include "esp_app_desc.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/ringbuf.h"
#include "freertos/task.h"

#include "spike.h"

namespace spike {
namespace {

constexpr const char *TAG = "update";

// Writes to the data characteristic carry at most 512 bytes (Bluedroid's
// attribute length limit), 4 of which are the offset.
constexpr uint32_t CHUNK_MAX = 512 - 4;
// What a client may have in flight: the ring buffer less its per-item
// headers (8 bytes per chunk, rounded up) and a margin.
constexpr uint32_t WINDOW = CONFIG_SPIKE_UPDATE_BUFFER - CONFIG_SPIKE_UPDATE_BUFFER / 32 - 1024;

enum class State : int { Idle, Erasing, Receiving, Finishing };

enum class CmdType { Info, Begin, Finish, Abort, Reboot, Disconnected };

struct Cmd {
    CmdType type;
    uint16_t conn_id;
    uint32_t size;
    bool erase_all;
    bool stop_radio;
};

// Shared with the Bluetooth task.
std::atomic<State> state{State::Idle};
std::atomic<int> update_conn{-1};
std::atomic<uint32_t> expected{0};
std::atomic<bool> nak_sent{false};
std::atomic<uint32_t> gaps{0};
std::atomic<uint32_t> overflows{0};
std::atomic<int64_t> first_data_us{0};
std::atomic<int64_t> last_data_us{0};

QueueHandle_t cmds;
RingbufHandle_t ring;

// The update task's own.
const esp_partition_t *target = nullptr;
esp_ota_handle_t ota = 0;
uint32_t image_size = 0;
uint32_t written = 0;
uint32_t last_ack = 0;
bool paused_radio = false;
int64_t erase_us = 0;
int64_t flash_us = 0;
uint32_t heap_low = 0;

const char *state_name(esp_ota_img_states_t s)
{
    switch (s) {
    case ESP_OTA_IMG_NEW: return "new";
    case ESP_OTA_IMG_PENDING_VERIFY: return "pending_verify";
    case ESP_OTA_IMG_VALID: return "valid";
    case ESP_OTA_IMG_INVALID: return "invalid";
    case ESP_OTA_IMG_ABORTED: return "aborted";
    case ESP_OTA_IMG_UNDEFINED: return "undefined";
    }
    return "?";
}

const char *partition_state(const esp_partition_t *p)
{
    esp_ota_img_states_t s;
    if (!p || esp_ota_get_state_partition(p, &s) != ESP_OK) {
        return "none";
    }
    return state_name(s);
}

void reply(uint16_t conn_id, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
void reply(uint16_t conn_id, const char *fmt, ...)
{
    char json[400];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(json, sizeof(json), fmt, ap);
    va_end(ap);
    ESP_LOGI(TAG, "-> %s", json);
    ble_mgmt_send(conn_id, json);
}

void error(uint16_t conn_id, const char *stage, const char *what)
{
    reply(conn_id, "{\"event\":\"error\",\"stage\":\"%s\",\"error\":\"%s\"}", stage, what);
}

void resume_radio()
{
    if (paused_radio) {
        radio_pause(false);
        paused_radio = false;
    }
}

void info(uint16_t conn_id)
{
    const esp_partition_t *running = esp_ota_get_running_partition();
    const esp_partition_t *other = esp_ota_get_next_update_partition(nullptr);
    const esp_partition_t *invalid = esp_ota_get_last_invalid_partition();
    const esp_app_desc_t *me = esp_app_get_description();
    esp_app_desc_t other_desc;
    bool other_ok = other && esp_ota_get_partition_description(other, &other_desc) == ESP_OK;
    reply(conn_id,
          "{\"event\":\"info\",\"version\":\"%s\",\"built\":\"%s %s\",\"running\":\"%s\",\"state\":\"%s\","
          "\"other\":\"%s\",\"other_version\":\"%s\",\"other_state\":\"%s\",\"last_invalid\":\"%s\","
          "\"heap_free\":%u,\"heap_min\":%u,\"radio\":\"%s\"}",
          me->version, me->date, me->time, running->label, partition_state(running), other ? other->label : "",
          other_ok ? other_desc.version : "", partition_state(other), invalid ? invalid->label : "",
          (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL),
          (unsigned)heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL),
          stats.radio_bridging ? "bridging" : "down");
}

void abort_update(const char *why)
{
    if (state == State::Receiving || state == State::Finishing) {
        esp_ota_abort(ota);
    }
    // Drop whatever is still queued.
    size_t len;
    while (void *item = xRingbufferReceive(ring, &len, 0)) {
        vRingbufferReturnItem(ring, item);
    }
    state = State::Idle;
    update_conn = -1;
    resume_radio();
    ESP_LOGW(TAG, "update aborted: %s", why);
}

void begin(const Cmd &cmd)
{
    if (state != State::Idle) {
        error(cmd.conn_id, "begin", "busy");
        return;
    }
    target = esp_ota_get_next_update_partition(nullptr);
    if (!target) {
        error(cmd.conn_id, "begin", "no update partition");
        return;
    }
    if (cmd.size == 0 || cmd.size > target->size) {
        error(cmd.conn_id, "begin", "bad size");
        return;
    }
    if (cmd.stop_radio && !paused_radio) {
        radio_pause(true);
        paused_radio = true;
    }
    state = State::Erasing;
    update_conn = cmd.conn_id;
    heap_mark("update begin");
    int64_t t0 = esp_timer_get_time();
    esp_err_t err = esp_ota_begin(target, cmd.erase_all ? cmd.size : OTA_WITH_SEQUENTIAL_WRITES, &ota);
    erase_us = esp_timer_get_time() - t0;
    if (err != ESP_OK) {
        state = State::Idle;
        update_conn = -1;
        resume_radio();
        error(cmd.conn_id, "begin", esp_err_to_name(err));
        return;
    }
    image_size = cmd.size;
    written = 0;
    last_ack = 0;
    flash_us = 0;
    heap_low = heap_caps_get_free_size(MALLOC_CAP_INTERNAL);
    gaps = 0;
    overflows = 0;
    first_data_us = 0;
    last_data_us = 0;
    expected = 0;
    nak_sent = false;
    state = State::Receiving;
    ESP_LOGI(TAG, "receiving %" PRIu32 " bytes into %s (erase %s, %lld ms)", image_size, target->label,
             cmd.erase_all ? "all" : "as it goes", erase_us / 1000);
    reply(cmd.conn_id,
          "{\"event\":\"ready\",\"partition\":\"%s\",\"erase_ms\":%lld,\"window\":%" PRIu32
          ",\"chunk_max\":%" PRIu32 ",\"ack_every\":%d,\"radio\":\"%s\"}",
          target->label, erase_us / 1000, WINDOW, CHUNK_MAX, CONFIG_SPIKE_UPDATE_ACK_EVERY,
          stats.radio_bridging ? "bridging" : "down");
}

void finish(const Cmd &cmd)
{
    if (state != State::Receiving || written != image_size) {
        char what[64];
        snprintf(what, sizeof(what), "incomplete: %" PRIu32 " of %" PRIu32, written, image_size);
        error(cmd.conn_id, "finish", what);
        return;
    }
    state = State::Finishing;
    int64_t t0 = esp_timer_get_time();
    // Checks the image: its header, its SHA-256 and, with signed apps on,
    // the ECDSA signature against the key built into this app.
    esp_err_t err = esp_ota_end(ota);
    int64_t verify_us = esp_timer_get_time() - t0;
    if (err != ESP_OK) {
        state = State::Idle;
        update_conn = -1;
        resume_radio();
        ESP_LOGE(TAG, "image rejected: %s", esp_err_to_name(err));
        error(cmd.conn_id, "verify", esp_err_to_name(err));
        return;
    }
    err = esp_ota_set_boot_partition(target);
    state = State::Idle;
    update_conn = -1;
    resume_radio();
    if (err != ESP_OK) {
        error(cmd.conn_id, "set_boot", esp_err_to_name(err));
        return;
    }
    esp_app_desc_t desc = {};
    esp_ota_get_partition_description(target, &desc);
    ESP_LOGI(TAG, "%s (%s) is set to boot next", target->label, desc.version);
    heap_mark("update done");
    reply(cmd.conn_id,
          "{\"event\":\"finished\",\"partition\":\"%s\",\"version\":\"%s\",\"bytes\":%" PRIu32
          ",\"erase_ms\":%lld,\"receive_ms\":%lld,\"flash_ms\":%lld,\"verify_ms\":%lld,\"gaps\":%" PRIu32
          ",\"overflows\":%" PRIu32 ",\"heap_low\":%" PRIu32 "}",
          target->label, desc.version, written, erase_us / 1000, (last_data_us - first_data_us) / 1000,
          flash_us / 1000, verify_us / 1000, gaps.load(), overflows.load(), heap_low);
}

void write_chunk(const uint8_t *data, size_t len)
{
    int64_t t0 = esp_timer_get_time();
    esp_err_t err = esp_ota_write(ota, data, len);
    flash_us += esp_timer_get_time() - t0;
    if (err != ESP_OK) {
        int conn = update_conn;
        abort_update(esp_err_to_name(err));
        if (conn >= 0) {
            error(conn, "write", esp_err_to_name(err));
        }
        return;
    }
    written += len;
    if (written - last_ack >= CONFIG_SPIKE_UPDATE_ACK_EVERY || written == image_size) {
        last_ack = written;
        uint32_t free_now = heap_caps_get_free_size(MALLOC_CAP_INTERNAL);
        if (free_now < heap_low) {
            heap_low = free_now;
        }
        char json[48];
        snprintf(json, sizeof(json), "{\"event\":\"ack\",\"written\":%" PRIu32 "}", written);
        ble_mgmt_send(update_conn, json);
    }
}

void update_task(void *)
{
    for (;;) {
        bool receiving = state == State::Receiving;
        Cmd cmd;
        if (xQueueReceive(cmds, &cmd, receiving ? 0 : pdMS_TO_TICKS(100)) == pdTRUE) {
            switch (cmd.type) {
            case CmdType::Info: info(cmd.conn_id); break;
            case CmdType::Begin: begin(cmd); break;
            case CmdType::Finish: finish(cmd); break;
            case CmdType::Abort:
                abort_update("asked to");
                reply(cmd.conn_id, "{\"event\":\"aborted\"}");
                break;
            case CmdType::Disconnected:
                if (state != State::Idle && update_conn == cmd.conn_id) {
                    abort_update("the client disconnected");
                }
                break;
            case CmdType::Reboot:
                reply(cmd.conn_id, "{\"event\":\"rebooting\"}");
                vTaskDelay(pdMS_TO_TICKS(500));
                esp_restart();
                break;
            }
            continue;
        }
        if (receiving) {
            size_t len;
            auto *item = static_cast<uint8_t *>(xRingbufferReceive(ring, &len, pdMS_TO_TICKS(20)));
            if (item) {
                write_chunk(item, len);
                vRingbufferReturnItem(ring, item);
            }
        }
    }
}

void post(const Cmd &cmd)
{
    if (xQueueSend(cmds, &cmd, 0) != pdTRUE) {
        ESP_LOGW(TAG, "command queue full");
    }
}

void nak(uint16_t conn_id, uint32_t offset)
{
    char json[48];
    snprintf(json, sizeof(json), "{\"event\":\"nak\",\"expected\":%" PRIu32 "}", offset);
    ble_mgmt_send(conn_id, json);
}

} // namespace

void update_start()
{
    cmds = xQueueCreate(4, sizeof(Cmd));
    ring = xRingbufferCreate(CONFIG_SPIKE_UPDATE_BUFFER, RINGBUF_TYPE_NOSPLIT);
    xTaskCreate(update_task, "update", 4096, nullptr, 4, nullptr);

    const esp_partition_t *running = esp_ota_get_running_partition();
    const esp_app_desc_t *me = esp_app_get_description();
    ESP_LOGI(TAG, "running %s, version %s, state %s", running->label, me->version, partition_state(running));
    if (const esp_partition_t *invalid = esp_ota_get_last_invalid_partition()) {
        esp_app_desc_t desc = {};
        esp_ota_get_partition_description(invalid, &desc);
        ESP_LOGW(TAG, "rolled back: %s (%s) is marked %s", invalid->label, desc.version, partition_state(invalid));
    }
}

void update_self_test_passed()
{
    const esp_partition_t *running = esp_ota_get_running_partition();
    esp_ota_img_states_t s;
    if (esp_ota_get_state_partition(running, &s) != ESP_OK || s != ESP_OTA_IMG_PENDING_VERIFY) {
        return;
    }
#if CONFIG_SPIKE_BREAK_SELF_TEST
    ESP_LOGE(TAG, "first boot of %s: failing the self-test on purpose", running->label);
    vTaskDelay(pdMS_TO_TICKS(1000));
    abort();
#else
    esp_ota_mark_app_valid_cancel_rollback();
    ESP_LOGI(TAG, "first boot of %s: self-test passed, marked valid", running->label);
#endif
}

void update_command(uint16_t conn_id, const char *json, size_t len)
{
    cJSON *msg = cJSON_ParseWithLength(json, len);
    const cJSON *c = cJSON_GetObjectItemCaseSensitive(msg, "cmd");
    if (!cJSON_IsString(c)) {
        error(conn_id, "parse", "no cmd");
        cJSON_Delete(msg);
        return;
    }
    ESP_LOGI(TAG, "<- %.*s", (int)len, json);
    Cmd cmd = {};
    cmd.conn_id = conn_id;
    const char *name = c->valuestring;
    if (strcmp(name, "info") == 0) {
        cmd.type = CmdType::Info;
    } else if (strcmp(name, "update.begin") == 0) {
        cmd.type = CmdType::Begin;
        const cJSON *size = cJSON_GetObjectItemCaseSensitive(msg, "size");
        const cJSON *erase = cJSON_GetObjectItemCaseSensitive(msg, "erase");
        const cJSON *stop = cJSON_GetObjectItemCaseSensitive(msg, "stop_radio");
        const cJSON *interval = cJSON_GetObjectItemCaseSensitive(msg, "interval");
        cmd.size = cJSON_IsNumber(size) ? static_cast<uint32_t>(size->valuedouble) : 0;
        cmd.erase_all = !cJSON_IsString(erase) || strcmp(erase->valuestring, "all") == 0;
        cmd.stop_radio = cJSON_IsTrue(stop);
        if (cJSON_IsArray(interval) && cJSON_GetArraySize(interval) == 2) {
            ble_request_interval(conn_id, cJSON_GetArrayItem(interval, 0)->valueint,
                                 cJSON_GetArrayItem(interval, 1)->valueint);
        }
    } else if (strcmp(name, "update.finish") == 0) {
        cmd.type = CmdType::Finish;
    } else if (strcmp(name, "update.abort") == 0) {
        cmd.type = CmdType::Abort;
    } else if (strcmp(name, "reboot") == 0) {
        cmd.type = CmdType::Reboot;
    } else {
        error(conn_id, "parse", "unknown cmd");
        cJSON_Delete(msg);
        return;
    }
    cJSON_Delete(msg);
    post(cmd);
}

void update_data(uint16_t conn_id, const uint8_t *data, size_t len)
{
    if (len < 5 || state != State::Receiving || update_conn != conn_id) {
        return;
    }
    uint32_t offset = data[0] | (uint32_t(data[1]) << 8) | (uint32_t(data[2]) << 16) | (uint32_t(data[3]) << 24);
    uint32_t want = expected;
    if (offset != want) {
        // Writes after a dropped one: ignore them until the client rewinds.
        gaps++;
        if (!nak_sent) {
            nak_sent = true;
            nak(conn_id, want);
        }
        return;
    }
    if (xRingbufferSend(ring, data + 4, len - 4, 0) != pdTRUE) {
        // Over the window. Ask again from here (also when a resend after a
        // nak doesn't fit, or the client would wait forever).
        overflows++;
        nak_sent = true;
        nak(conn_id, want);
        return;
    }
    nak_sent = false;
    expected = want + (len - 4);
    int64_t now = esp_timer_get_time();
    if (first_data_us == 0) {
        first_data_us = now;
    }
    last_data_us = now;
}

void update_disconnected(uint16_t conn_id)
{
    Cmd cmd = {};
    cmd.type = CmdType::Disconnected;
    cmd.conn_id = conn_id;
    post(cmd);
}

} // namespace spike
