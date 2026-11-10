#include "auth.h"

#include <cstring>
#include <mutex>

#include "bootloader_random.h"
#include "esp_bt.h"
#include "esp_log.h"
#include "esp_random.h"
#include "esp_timer.h"
#include "nvs.h"
#include "psa/crypto.h"

namespace ktb::auth {

namespace {

constexpr const char *TAG = "auth";

constexpr const char *key_private = "x25519";
constexpr const char *key_code = "setup_code";
constexpr size_t code_bytes = 10; // 80 bits

uint8_t pub[32];
uint8_t setup_code[code_bytes];
std::mutex code_lock;

std::mutex window_lock;
int64_t window_until_us = 0; // 0: closed

// Crockford base32 of 10 bytes: 16 characters, most significant bits first.
std::string base32(const uint8_t *bytes)
{
    static const char alphabet[] = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    std::string out;
    uint32_t acc = 0;
    int bits = 0;
    for (size_t i = 0; i < code_bytes; i++) {
        acc = (acc << 8) | bytes[i];
        bits += 8;
        while (bits >= 5) {
            out += alphabet[(acc >> (bits - 5)) & 0x1f];
            bits -= 5;
        }
    }
    return out;
}

std::string base64(const uint8_t *bytes, size_t len)
{
    static const char alphabet[] =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    std::string out;
    for (size_t i = 0; i < len; i += 3) {
        uint32_t n = bytes[i] << 16;
        if (i + 1 < len) n |= bytes[i + 1] << 8;
        if (i + 2 < len) n |= bytes[i + 2];
        out += alphabet[(n >> 18) & 0x3f];
        out += alphabet[(n >> 12) & 0x3f];
        out += i + 1 < len ? alphabet[(n >> 6) & 0x3f] : '=';
        out += i + 2 < len ? alphabet[n & 0x3f] : '=';
    }
    return out;
}

// Random bytes for keys and codes. The ESP32's generator is only truly
// random while the radio runs, or with the bootloader's entropy source on,
// which mustn't be used while the radio runs.
void secure_random(uint8_t *out, size_t len)
{
    if (esp_bt_controller_get_status() == ESP_BT_CONTROLLER_STATUS_ENABLED) {
        esp_fill_random(out, len);
        return;
    }
    bootloader_random_enable();
    esp_fill_random(out, len);
    bootloader_random_disable();
}

psa_key_attributes_t x25519_attributes()
{
    psa_key_attributes_t attr = PSA_KEY_ATTRIBUTES_INIT;
    psa_set_key_type(&attr, PSA_KEY_TYPE_ECC_KEY_PAIR(PSA_ECC_FAMILY_MONTGOMERY));
    psa_set_key_bits(&attr, 255);
    psa_set_key_usage_flags(&attr, PSA_KEY_USAGE_DERIVE | PSA_KEY_USAGE_EXPORT);
    psa_set_key_algorithm(&attr, PSA_ALG_ECDH);
    return attr;
}

// The public key for a private key.
bool derive_public(const uint8_t *priv)
{
    psa_key_attributes_t attr = x25519_attributes();
    psa_key_id_t id;
    if (psa_import_key(&attr, priv, 32, &id) != PSA_SUCCESS) {
        return false;
    }
    size_t len = 0;
    psa_status_t st = psa_export_public_key(id, pub, sizeof(pub), &len);
    psa_destroy_key(id);
    return st == PSA_SUCCESS && len == sizeof(pub);
}

bool save_code(nvs_handle_t h, const uint8_t *code)
{
    return nvs_set_blob(h, key_code, code, code_bytes) == ESP_OK && nvs_commit(h) == ESP_OK;
}

} // namespace

bool load()
{
    if (psa_crypto_init() != PSA_SUCCESS) {
        ESP_LOGE(TAG, "psa_crypto_init failed");
        return false;
    }
    nvs_handle_t h;
    if (nvs_open(identity_namespace, NVS_READWRITE, &h) != ESP_OK) {
        ESP_LOGE(TAG, "can't open NVS");
        return false;
    }

    uint8_t priv[32];
    size_t len = sizeof(priv);
    bool ok = nvs_get_blob(h, key_private, priv, &len) == ESP_OK && len == sizeof(priv) &&
              derive_public(priv);
    if (!ok) {
        // The first boot after a clean install: make the device's key.
        ESP_LOGI(TAG, "making the device key");
        secure_random(priv, sizeof(priv));
        // X25519 clamping, as the key's owner would apply it anyway
        priv[0] &= 248;
        priv[31] = (priv[31] & 127) | 64;
        ok = derive_public(priv) && nvs_set_blob(h, key_private, priv, sizeof(priv)) == ESP_OK &&
             nvs_commit(h) == ESP_OK;
    }
    memset(priv, 0, sizeof(priv));
    if (!ok) {
        ESP_LOGE(TAG, "no device key");
        nvs_close(h);
        return false;
    }

    len = code_bytes;
    if (nvs_get_blob(h, key_code, setup_code, &len) != ESP_OK || len != code_bytes) {
        ESP_LOGI(TAG, "making the setup code");
        secure_random(setup_code, code_bytes);
        if (!save_code(h, setup_code)) {
            ESP_LOGE(TAG, "couldn't save the setup code");
            nvs_close(h);
            return false;
        }
    }
    nvs_close(h);
    ESP_LOGI(TAG, "fingerprint %s", fingerprint().c_str());
    return true;
}

const uint8_t *public_key() { return pub; }

std::string public_key_base64() { return base64(pub, sizeof(pub)); }

std::string fingerprint()
{
    uint8_t hash[32];
    size_t len = 0;
    psa_hash_compute(PSA_ALG_SHA_256, pub, sizeof(pub), hash, sizeof(hash), &len);
    return base32(hash);
}

std::string code()
{
    std::string plain;
    {
        std::lock_guard<std::mutex> g(code_lock);
        plain = base32(setup_code);
    }
    return plain.substr(0, 4) + "-" + plain.substr(4, 4) + "-" + plain.substr(8, 4) + "-" +
           plain.substr(12, 4);
}

std::string uri()
{
    std::string plain;
    {
        std::lock_guard<std::mutex> g(code_lock);
        plain = base32(setup_code);
    }
    return "https://www.k0tfu.org/ktb#code=" + plain + "&fp=" + fingerprint();
}

bool rotate_code()
{
    uint8_t fresh[code_bytes];
    secure_random(fresh, sizeof(fresh));
    nvs_handle_t h;
    if (nvs_open(identity_namespace, NVS_READWRITE, &h) != ESP_OK) {
        return false;
    }
    bool ok = save_code(h, fresh);
    nvs_close(h);
    if (ok) {
        std::lock_guard<std::mutex> g(code_lock);
        memcpy(setup_code, fresh, code_bytes);
        ESP_LOGI(TAG, "setup code rotated");
    }
    return ok;
}

void window_open(uint32_t seconds)
{
    if (seconds == 0) {
        window_close();
        return;
    }
    if (seconds > window_max_seconds) {
        seconds = window_max_seconds;
    }
    std::lock_guard<std::mutex> g(window_lock);
    window_until_us = esp_timer_get_time() + static_cast<int64_t>(seconds) * 1000000;
    ESP_LOGI(TAG, "management window open for %u s", (unsigned)seconds);
}

void window_close()
{
    std::lock_guard<std::mutex> g(window_lock);
    if (window_until_us != 0) {
        ESP_LOGI(TAG, "management window closed");
    }
    window_until_us = 0;
}

bool window_is_open()
{
    return window_seconds_left() > 0;
}

uint32_t window_seconds_left()
{
    std::lock_guard<std::mutex> g(window_lock);
    if (window_until_us == 0) {
        return 0;
    }
    int64_t left = window_until_us - esp_timer_get_time();
    if (left <= 0) {
        window_until_us = 0;
        ESP_LOGI(TAG, "management window timed out");
        return 0;
    }
    return static_cast<uint32_t>((left + 999999) / 1000000);
}

int client_count() { return 0; }

} // namespace ktb::auth
