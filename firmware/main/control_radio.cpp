// The control API's radio commands: status, subscribe, tnc.*, bluetooth.*
// and port.* (docs/control-api.md, Devices).

#include <cstring>
#include <string>

#include "control_internal.h"
#include "phones.h"
#include "radio.h"

namespace ktb::control {

namespace {

const char *str(const cJSON *req, const char *key)
{
    return cJSON_GetStringValue(cJSON_GetObjectItemCaseSensitive(req, key));
}

// The device's one adapter answers to its id; anything else isn't here.
bool adapter_ok(const cJSON *req, Reply &err)
{
    const char *a = str(req, "adapter");
    if (a && strcmp(a, "builtin") != 0) {
        err = Reply::error("not_found", std::string("no adapter ") + a + "; this device has one, builtin");
        return false;
    }
    return true;
}

// The device's one TNC is "radio".
bool tnc_ok(const char *tnc, Reply &err)
{
    if (tnc && strcmp(tnc, "radio") != 0) {
        err = Reply::error("not_found", std::string("no TNC ") + tnc + "; this device has one, radio");
        return false;
    }
    return true;
}

Reply status(const cJSON *, const Pending &)
{
    return Reply::ok(radio::status_json());
}

// The topics a request names, as bits. all_if_missing: no topics means
// every topic (unsubscribe).
bool parse_topics(const cJSON *req, bool all_if_missing, uint32_t &out, Reply &err)
{
    const cJSON *topics = cJSON_GetObjectItemCaseSensitive(req, "topics");
    if (!topics && all_if_missing) {
        out = topic_state | topic_frames | topic_activity;
        return true;
    }
    if (!cJSON_IsArray(topics)) {
        err = Reply::error("bad_request", "topics must be a list");
        return false;
    }
    if (all_if_missing && cJSON_GetArraySize(topics) == 0) {
        out = topic_state | topic_frames | topic_activity;
        return true;
    }
    out = 0;
    const cJSON *t;
    cJSON_ArrayForEach(t, topics)
    {
        const char *name = cJSON_GetStringValue(t);
        if (name && strcmp(name, "state") == 0) {
            out |= topic_state;
        } else if (name && strcmp(name, "frames") == 0) {
            out |= topic_frames;
        } else if (name && strcmp(name, "activity") == 0) {
            out |= topic_activity;
        } else {
            err = Reply::error("bad_request", std::string("unknown topic ") + (name ? name : "?") +
                                                  "; the topics are state, activity and frames");
            return false;
        }
    }
    return true;
}

cJSON *topics_json(uint32_t now)
{
    cJSON *o = cJSON_CreateObject();
    cJSON *list = cJSON_AddArrayToObject(o, "topics");
    if (now & topic_state) cJSON_AddItemToArray(list, cJSON_CreateString("state"));
    if (now & topic_activity) cJSON_AddItemToArray(list, cJSON_CreateString("activity"));
    if (now & topic_frames) cJSON_AddItemToArray(list, cJSON_CreateString("frames"));
    return o;
}

Reply subscribe_cmd(const cJSON *req, const Pending &p)
{
    uint32_t add;
    Reply err;
    if (!parse_topics(req, false, add, err)) {
        return err;
    }
    return Reply::ok(topics_json(subscribe(p.conn, add)));
}

// Over USB a subscription can't end with its connection, so it ends here
// or at a reboot.
Reply unsubscribe_cmd(const cJSON *req, const Pending &p)
{
    uint32_t remove;
    Reply err;
    if (!parse_topics(req, true, remove, err)) {
        return err;
    }
    return Reply::ok(topics_json(unsubscribe(p.conn, remove)));
}

Reply tnc_models(const cJSON *, const Pending &)
{
    return Reply::ok(radio::models_json());
}

Reply tnc_update(const cJSON *req, const Pending &p)
{
    Reply err;
    const char *tnc = str(req, "tnc");
    if (!tnc) {
        return Reply::error("bad_request", "tnc.update needs tnc");
    }
    if (!tnc_ok(tnc, err)) {
        return err;
    }
    const cJSON *def = cJSON_GetObjectItemCaseSensitive(req, "definition");
    if (!cJSON_IsObject(def)) {
        return Reply::error("bad_request", "tnc.update needs a definition");
    }
    std::string model;
    std::string address;
    for (const cJSON *item = def->child; item; item = item->next) {
        const char *key = item->string;
        if (strcmp(key, "id") == 0) {
            if (!cJSON_IsString(item) || strcmp(item->valuestring, "radio") != 0) {
                return Reply::error("bad_request", "a device's TNC is always radio; it can't be renamed");
            }
        } else if (strcmp(key, "model") == 0 && cJSON_IsString(item)) {
            model = item->valuestring;
        } else if (strcmp(key, "spp") == 0 && cJSON_IsObject(item)) {
            for (const cJSON *s = item->child; s; s = s->next) {
                if (strcmp(s->string, "address") == 0 && cJSON_IsString(s)) {
                    address = s->valuestring;
                } else if (strcmp(s->string, "adapter") == 0 && cJSON_IsString(s) &&
                           strcmp(s->valuestring, "builtin") == 0) {
                    // the only adapter
                } else {
                    return Reply::error("bad_request", std::string("spp.") + s->string +
                                                           " can't be set on a device; it takes address");
                }
            }
        } else {
            return Reply::error("bad_request", std::string(key) +
                                                   " can't be set on a device's TNC; it takes model and "
                                                   "spp.address");
        }
    }
    radio::update(p, model, address, cJSON_IsTrue(cJSON_GetObjectItemCaseSensitive(req, "non_disruptive")));
    return Reply::later();
}

Reply bt_scan(const cJSON *req, const Pending &p)
{
    Reply err;
    if (!adapter_ok(req, err)) {
        return err;
    }
    const char *transport = str(req, "transport");
    if (transport && strcmp(transport, "classic") != 0) {
        return Reply::error("not_supported", "a device scans for Classic radios only");
    }
    uint32_t seconds = 10;
    if (const cJSON *s = cJSON_GetObjectItemCaseSensitive(req, "seconds")) {
        if (!cJSON_IsNumber(s) || s->valuedouble < 1 || s->valuedouble > 60) {
            return Reply::error("bad_request", "seconds must be from 1 to 60");
        }
        seconds = static_cast<uint32_t>(s->valuedouble);
    }
    radio::scan(p, seconds);
    return Reply::later();
}

Reply bt_scan_stop(const cJSON *req, const Pending &p)
{
    const cJSON *r = cJSON_GetObjectItemCaseSensitive(req, "request");
    if (!cJSON_IsNumber(r)) {
        return Reply::error("bad_request", "bluetooth.scan_stop needs request, the scan's id");
    }
    radio::scan_stop(p, r->valuedouble);
    return Reply::later();
}

Reply bt_pair(const cJSON *req, const Pending &p)
{
    Reply err;
    if (!adapter_ok(req, err) || !tnc_ok(str(req, "tnc"), err)) {
        return err;
    }
    const char *address = str(req, "address");
    radio::pair(p, address ? address : "");
    return Reply::later();
}

Reply bt_pair_reply(const cJSON *req, const Pending &p)
{
    const cJSON *r = cJSON_GetObjectItemCaseSensitive(req, "request");
    const cJSON *accept = cJSON_GetObjectItemCaseSensitive(req, "accept");
    if (!cJSON_IsNumber(r) || !cJSON_IsBool(accept)) {
        return Reply::error("bad_request",
                            "bluetooth.pair_reply needs request (the pairing's id) and accept; the radio "
                            "only asks to confirm a number, so pin and passkey aren't used");
    }
    radio::pair_reply(p, r->valuedouble, cJSON_IsTrue(accept));
    return Reply::later();
}

Reply bt_unpair(const cJSON *req, const Pending &p)
{
    Reply err;
    if (!adapter_ok(req, err) || !tnc_ok(str(req, "tnc"), err)) {
        return err;
    }
    const char *address = str(req, "address");
    radio::unpair(p, address ? address : "");
    return Reply::later();
}

Reply port_hold(const cJSON *req, const Pending &p, radio::Hold h)
{
    const char *bridge = str(req, "bridge");
    const char *port = str(req, "port");
    if (!bridge || !port) {
        return Reply::error("bad_request", "needs bridge and port");
    }
    if (strcmp(bridge, "ktbr0") != 0) {
        return Reply::error("not_found", std::string("no bridge ") + bridge + "; this device has ktbr0");
    }
    if (strcmp(port, "radio") != 0) {
        return Reply::error(strcmp(port, "phones") == 0 ? "bad_request" : "not_found",
                            std::string(port) + " isn't a TNC port; ktbr0's TNC port is radio");
    }
    radio::hold(p, h);
    return Reply::later();
}

Reply session_kick(const cJSON *req, const Pending &p)
{
    const char *session = str(req, "session");
    if (!session) {
        return Reply::error("bad_request", "session.kick needs session");
    }
    phones::kick(p, session);
    return Reply::later();
}

Reply port_connect(const cJSON *req, const Pending &p) { return port_hold(req, p, radio::Hold::up); }
Reply port_disconnect(const cJSON *req, const Pending &p) { return port_hold(req, p, radio::Hold::down); }
Reply port_release(const cJSON *req, const Pending &p) { return port_hold(req, p, radio::Hold::none); }

} // namespace

const Command radio_commands[] = {
    {"status", status},
    {"subscribe", subscribe_cmd},
    {"unsubscribe", unsubscribe_cmd},
    {"tnc.models", tnc_models},
    {"tnc.update", tnc_update},
    {"bluetooth.scan", bt_scan},
    {"bluetooth.scan_stop", bt_scan_stop},
    {"bluetooth.pair", bt_pair},
    {"bluetooth.pair_reply", bt_pair_reply},
    {"bluetooth.unpair", bt_unpair},
    {"port.connect", port_connect},
    {"port.disconnect", port_disconnect},
    {"port.release", port_release},
    {"session.kick", session_kick},
};
const size_t radio_command_count = sizeof(radio_commands) / sizeof(radio_commands[0]);

} // namespace ktb::control
