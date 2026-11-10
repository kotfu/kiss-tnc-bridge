#include "kiss.h"

#include <vector>

namespace ktb::kiss {

void Splitter::feed(const uint8_t *data, size_t len, const Handler &on_frame)
{
    for (size_t i = 0; i < len; i++) {
        uint8_t b = data[i];
        if (b == FEND) {
            if (len_ > 0 && !overflow_) {
                on_frame(buf_, len_);
            }
            len_ = 0;
            overflow_ = false;
        } else if (len_ < max_frame) {
            buf_[len_++] = b;
        } else {
            overflow_ = true;
        }
    }
}

std::string hex(const uint8_t *frame, size_t len)
{
    static const char digits[] = "0123456789abcdef";
    std::string out;
    out.reserve(len * 2);
    for (size_t i = 0; i < len; i++) {
        out += digits[frame[i] >> 4];
        out += digits[frame[i] & 0xf];
    }
    return out;
}

namespace {

std::vector<uint8_t> unstuff(const uint8_t *frame, size_t len)
{
    std::vector<uint8_t> out;
    out.reserve(len);
    for (size_t i = 0; i < len; i++) {
        if (frame[i] == FESC && i + 1 < len) {
            i++;
            out.push_back(frame[i] == TFEND ? FEND : frame[i] == TFESC ? FESC : frame[i]);
        } else {
            out.push_back(frame[i]);
        }
    }
    return out;
}

// One 7-byte AX.25 address: "W1AW-9".
std::string address(const uint8_t *a)
{
    std::string call;
    for (int i = 0; i < 6; i++) {
        char c = static_cast<char>(a[i] >> 1);
        if (c != ' ') {
            call += c;
        }
    }
    int ssid = (a[6] >> 1) & 0x0f;
    if (ssid) {
        call += "-" + std::to_string(ssid);
    }
    return call;
}

} // namespace

cJSON *decode(const uint8_t *frame, size_t len)
{
    if (len < 1) {
        return nullptr;
    }
    std::vector<uint8_t> f = unstuff(frame, len);
    cJSON *d = cJSON_CreateObject();
    cJSON_AddNumberToObject(d, "channel", f[0] >> 4);
    cJSON_AddNumberToObject(d, "command", f[0] & 0x0f);
    if ((f[0] & 0x0f) != 0) {
        return d; // not a data frame
    }

    // Addresses: destination, source, then up to 8 digipeaters. The last
    // address has bit 0 of its SSID byte set.
    const uint8_t *p = f.data() + 1;
    size_t n = f.size() - 1;
    if (n < 14) {
        return d;
    }
    size_t addrs = 0;
    while (addrs < 10 && (addrs + 1) * 7 <= n) {
        addrs++;
        if (p[addrs * 7 - 1] & 0x01) {
            break;
        }
    }
    if (addrs < 2 || !(p[addrs * 7 - 1] & 0x01)) {
        return d; // no end to the address field
    }
    std::string dest = address(p);
    std::string src = address(p + 7);
    cJSON *ax25 = cJSON_AddObjectToObject(d, "ax25");
    cJSON_AddStringToObject(ax25, "source", src.c_str());
    cJSON_AddStringToObject(ax25, "destination", dest.c_str());
    cJSON *path = cJSON_AddArrayToObject(ax25, "path");
    std::string summary = src + ">" + dest;
    for (size_t i = 2; i < addrs; i++) {
        std::string digi = address(p + i * 7);
        if (p[i * 7 + 6] & 0x80) {
            digi += "*"; // has been repeated
        }
        cJSON_AddItemToArray(path, cJSON_CreateString(digi.c_str()));
        summary += "," + digi;
    }

    // Control and PID, then the information field for UI frames.
    size_t info = addrs * 7 + 2;
    if (n >= info && (p[addrs * 7] & 0xEF) == 0x03) {
        summary += ":";
        for (size_t i = info; i < n && summary.size() < 256; i++) {
            char c = static_cast<char>(p[i]);
            summary += (c >= 0x20 && c < 0x7f) ? c : '.';
        }
    }
    cJSON_AddStringToObject(d, "summary", summary.c_str());
    return d;
}

} // namespace ktb::kiss
