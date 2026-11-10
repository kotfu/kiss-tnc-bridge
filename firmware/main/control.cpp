#include "control.h"

#include <cstring>
#include <functional>
#include <string>
#include <vector>

#include "auth.h"
#include "board.h"
#include "cJSON.h"
#include "console.h"
#include "esp_log.h"
#include "esp_mac.h"
#include "esp_system.h"
#include "firmware.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "radio.h"
#include "settings.h"
#include "storage.h"
#include "ui.h"

namespace ktb::control {

namespace {

constexpr const char *TAG = "control";
constexpr int protocol_version = 1;

// What a command produces: a result, or an error code and message. `after`
// runs once the response is on its way (a reboot, say).
struct Reply {
    cJSON *result = nullptr;
    const char *code = nullptr;
    std::string message;
    std::function<void()> after;

    static Reply ok(cJSON *r) { return Reply{r, nullptr, {}, {}}; }
    static Reply empty() { return ok(cJSON_CreateObject()); }
    static Reply error(const char *code, std::string message)
    {
        return Reply{nullptr, code, std::move(message), {}};
    }
};

using Handler = Reply (*)(const cJSON *req);

void send(cJSON *msg)
{
    char *text = cJSON_PrintUnformatted(msg);
    if (text) {
        console::write_line(text, strlen(text));
        cJSON_free(text);
    }
    cJSON_Delete(msg);
}

void send_reply(const cJSON *id, Reply &r)
{
    cJSON *msg = cJSON_CreateObject();
    cJSON_AddNumberToObject(msg, "v", protocol_version);
    if (id) {
        cJSON_AddItemToObject(msg, "id", cJSON_Duplicate(id, true));
    } else {
        cJSON_AddNullToObject(msg, "id");
    }
    if (r.code) {
        cJSON_AddFalseToObject(msg, "ok");
        cJSON *err = cJSON_AddObjectToObject(msg, "error");
        cJSON_AddStringToObject(err, "code", r.code);
        cJSON_AddStringToObject(err, "message", r.message.c_str());
        ESP_LOGI(TAG, "error %s: %s", r.code, r.message.c_str());
    } else {
        cJSON_AddTrueToObject(msg, "ok");
        cJSON_AddItemToObject(msg, "result", r.result ? r.result : cJSON_CreateObject());
        r.result = nullptr;
    }
    send(msg);
}

void reboot_soon()
{
    console::flush();
    vTaskDelay(pdMS_TO_TICKS(100));
    esp_restart();
}

std::string mac_string(esp_mac_type_t type)
{
    uint8_t mac[6] = {};
    esp_read_mac(mac, type);
    char buf[18];
    snprintf(buf, sizeof(buf), "%02X:%02X:%02X:%02X:%02X:%02X", mac[0], mac[1], mac[2], mac[3],
             mac[4], mac[5]);
    return buf;
}

cJSON *window_json()
{
    cJSON *o = cJSON_CreateObject();
    uint32_t left = auth::window_seconds_left();
    cJSON_AddBoolToObject(o, "open", left > 0);
    cJSON_AddNumberToObject(o, "seconds_left", left);
    return o;
}

// device.*

Reply device_info(const cJSON *)
{
    cJSON *o = cJSON_CreateObject();
    cJSON_AddStringToObject(o, "board", board::name);
    cJSON_AddItemToObject(o, "firmware", firmware::info_json());
    cJSON_AddNumberToObject(o, "protocol", protocol_version);
    // Capabilities arrive with the features: spp-client with the radio,
    // ble-server with the BLE KISS service.
    cJSON_AddArrayToObject(o, "capabilities");
    cJSON_AddNumberToObject(o, "max_message", console::max_message);
    cJSON_AddStringToObject(o, "fingerprint", auth::fingerprint().c_str());
    cJSON *bt = cJSON_AddObjectToObject(o, "bluetooth");
    cJSON_AddStringToObject(bt, "address", mac_string(ESP_MAC_BT).c_str());
    return Reply::ok(o);
}

Reply settings_get(const cJSON *)
{
    cJSON *o = cJSON_CreateObject();
    cJSON_AddItemToObject(o, "settings", settings::to_json());
    return Reply::ok(o);
}

Reply settings_set(const cJSON *req)
{
    std::string error;
    std::vector<std::string> reboot_required;
    if (!settings::apply(cJSON_GetObjectItemCaseSensitive(req, "settings"), error,
                         reboot_required)) {
        return Reply::error("bad_request", error);
    }
    cJSON *o = cJSON_CreateObject();
    cJSON_AddItemToObject(o, "settings", settings::to_json());
    cJSON *rr = cJSON_AddArrayToObject(o, "reboot_required");
    for (const auto &k : reboot_required) {
        cJSON_AddItemToArray(rr, cJSON_CreateString(k.c_str()));
    }
    return Reply::ok(o);
}

Reply device_reboot(const cJSON *)
{
    ESP_LOGI(TAG, "rebooting");
    Reply r = Reply::empty();
    r.after = reboot_soon;
    return r;
}

Reply device_reset(const cJSON *)
{
    Reply r = Reply::empty();
    r.after = [] {
        storage::reset();
        ui::post(ui::Event::reset_done);
        ui::wait_idle(3000);
        reboot_soon();
    };
    return r;
}

Reply device_identify(const cJSON *)
{
    ui::post(ui::Event::identify);
    return Reply::empty();
}

// auth.*

Reply auth_info(const cJSON *)
{
    cJSON *o = cJSON_CreateObject();
    cJSON_AddStringToObject(o, "fingerprint", auth::fingerprint().c_str());
    cJSON_AddStringToObject(o, "public_key", auth::public_key_base64().c_str());
    cJSON_AddItemToObject(o, "window", window_json());
    cJSON_AddNumberToObject(o, "clients", auth::client_count());
    return Reply::ok(o);
}

Reply code_reply()
{
    cJSON *o = cJSON_CreateObject();
    cJSON_AddStringToObject(o, "code", auth::code().c_str());
    cJSON_AddStringToObject(o, "uri", auth::uri().c_str());
    return Reply::ok(o);
}

Reply auth_code(const cJSON *) { return code_reply(); }

Reply auth_rotate_code(const cJSON *)
{
    if (!auth::rotate_code()) {
        return Reply::error("internal", "couldn't save a new setup code; the old one still works");
    }
    return code_reply();
}

Reply auth_window(const cJSON *req)
{
    const cJSON *s = cJSON_GetObjectItemCaseSensitive(req, "seconds");
    uint32_t seconds = auth::window_default_seconds;
    if (s) {
        if (!cJSON_IsNumber(s) || s->valuedouble < 0 || s->valuedouble > auth::window_max_seconds ||
            s->valuedouble != static_cast<uint32_t>(s->valuedouble)) {
            return Reply::error("bad_request", "seconds must be a whole number from 0 to 3600");
        }
        seconds = static_cast<uint32_t>(s->valuedouble);
    }
    auth::window_open(seconds); // 0 closes it
    return Reply::ok(window_json());
}

Reply auth_clients(const cJSON *)
{
    cJSON *o = cJSON_CreateObject();
    cJSON_AddArrayToObject(o, "clients"); // enrollment comes in Phase E
    return Reply::ok(o);
}

#if CONFIG_KTB_DEBUG_COMMANDS
// debug.* fake what milestone 1 doesn't have yet, to try the LED.

Reply debug_set(const cJSON *req)
{
    if (const cJSON *r = cJSON_GetObjectItemCaseSensitive(req, "radio")) {
        const char *s = cJSON_GetStringValue(r);
        if (s && strcmp(s, "none") == 0) {
            radio::debug_set_link(radio::Link::none);
        } else if (s && strcmp(s, "down") == 0) {
            radio::debug_set_link(radio::Link::down);
        } else if (s && strcmp(s, "connected") == 0) {
            radio::debug_set_link(radio::Link::connected);
        } else {
            return Reply::error("bad_request", "radio must be \"none\", \"down\" or \"connected\"");
        }
    }
    if (const cJSON *p = cJSON_GetObjectItemCaseSensitive(req, "phones")) {
        if (!cJSON_IsNumber(p) || p->valuedouble < 0 || p->valuedouble > 8) {
            return Reply::error("bad_request", "phones must be 0 to 8");
        }
        radio::debug_set_phones(static_cast<int>(p->valuedouble));
    }
    if (const cJSON *e = cJSON_GetObjectItemCaseSensitive(req, "error")) {
        ui::set_error(cJSON_IsTrue(e));
    }
    if (const cJSON *u = cJSON_GetObjectItemCaseSensitive(req, "updating")) {
        ui::set_updating(cJSON_IsTrue(u));
    }
    return Reply::empty();
}

Reply debug_frame(const cJSON *req)
{
    const char *dir = cJSON_GetStringValue(cJSON_GetObjectItemCaseSensitive(req, "direction"));
    if (dir && strcmp(dir, "to_phone") == 0) {
        ui::post(ui::Event::frame_to_phone);
    } else if (dir && strcmp(dir, "to_radio") == 0) {
        ui::post(ui::Event::frame_to_radio);
    } else {
        return Reply::error("bad_request", "direction must be \"to_phone\" or \"to_radio\"");
    }
    return Reply::empty();
}

Reply debug_pairing_result(const cJSON *req)
{
    if (!radio::pairing()) {
        return Reply::error("failed", "not pairing");
    }
    radio::debug_finish_pairing(cJSON_IsTrue(cJSON_GetObjectItemCaseSensitive(req, "succeeded")));
    return Reply::empty();
}
#endif

struct Command {
    const char *name;
    Handler handler;
};

const Command commands[] = {
    {"device.info", device_info},
    {"device.settings.get", settings_get},
    {"device.settings.set", settings_set},
    {"device.reboot", device_reboot},
    {"device.reset", device_reset},
    {"device.identify", device_identify},
    {"auth.info", auth_info},
    {"auth.code", auth_code},
    {"auth.rotate_code", auth_rotate_code},
    {"auth.window", auth_window},
    {"auth.clients", auth_clients},
#if CONFIG_KTB_DEBUG_COMMANDS
    {"debug.set", debug_set},
    {"debug.frame", debug_frame},
    {"debug.pairing_result", debug_pairing_result},
#endif
};

// The daemon's commands that a device's fixed bridge can't do
// (docs/control-api.md, Devices).
bool daemon_only(const char *cmd)
{
    static const char *const exact[] = {
        "reload", "config.check", "config.get", "config.put", "port.add", "port.update",
        "port.remove", "tnc.create", "tnc.delete", "serial.list",
    };
    for (const char *c : exact) {
        if (strcmp(cmd, c) == 0) {
            return true;
        }
    }
    return strncmp(cmd, "bridge.", 7) == 0 || strncmp(cmd, "adapter.", 8) == 0;
}

Reply dispatch(const cJSON *req, const char *cmd)
{
    for (const Command &c : commands) {
        if (strcmp(cmd, c.name) == 0) {
            return c.handler(req);
        }
    }
    if (daemon_only(cmd)) {
        return Reply::error("not_supported",
                            std::string(cmd) + " isn't available on a device: its bridge is fixed");
    }
    if (strncmp(cmd, "firmware.update.", 16) == 0) {
        return Reply::error("not_supported",
                            "firmware updates go over BLE; over USB, flash the device instead");
    }
    std::string known;
    for (const Command &c : commands) {
        known += known.empty() ? "" : ", ";
        known += c.name;
    }
    return Reply::error("unknown_command", std::string("no command ") + cmd + "; known: " + known);
}

} // namespace

void handle_line(char *line, size_t len)
{
    if (!line) {
        Reply r = Reply::error("bad_request", "request longer than 4096 bytes");
        send_reply(nullptr, r);
        return;
    }
    if (line[0] != '{') {
        return; // not a request: a stray line, or an echo of the log
    }
    cJSON *req = cJSON_ParseWithLength(line, len);
    if (!cJSON_IsObject(req)) {
        cJSON_Delete(req);
        Reply r = Reply::error("bad_request", "not a JSON object");
        send_reply(nullptr, r);
        return;
    }

    const cJSON *id = cJSON_GetObjectItemCaseSensitive(req, "id");
    const cJSON *v = cJSON_GetObjectItemCaseSensitive(req, "v");
    const char *cmd = cJSON_GetStringValue(cJSON_GetObjectItemCaseSensitive(req, "cmd"));
    Reply r;
    if (!cJSON_IsNumber(id)) {
        id = nullptr;
        r = Reply::error("bad_request", "a request needs a numeric id");
    } else if (!cJSON_IsNumber(v)) {
        r = Reply::error("bad_request", "a request needs \"v\": 1");
    } else if (v->valuedouble != protocol_version) {
        r = Reply::error("unsupported_version", "this device speaks version 1");
    } else if (!cmd) {
        r = Reply::error("bad_request", "a request needs a cmd");
    } else {
        ESP_LOGI(TAG, "%s", cmd);
        r = dispatch(req, cmd);
    }
    send_reply(id, r);
    auto after = std::move(r.after);
    cJSON_Delete(req);
    if (after) {
        after();
    }
}

} // namespace ktb::control
