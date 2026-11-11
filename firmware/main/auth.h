// The device's key, its authorization code and authorization mode
// (docs/control-api.md, Authentication). The key and code are made on the
// first boot and kept through a reset; only erasing the flash replaces
// them. Authorizing management apps over BLE comes in Phase E: until then
// authorization mode starts and ends, but no app can be authorized in it.
#pragma once

#include <cstdint>
#include <string>

namespace ktb::auth {

// Loads the key and code from NVS, making them on the first boot.
// Returns false if there's no key and one couldn't be made.
bool load();

// The X25519 public key, 32 bytes, and as base64.
const uint8_t *public_key();
std::string public_key_base64();

// The first 10 bytes of the SHA-256 of the public key, as 16 Crockford
// base32 characters.
std::string fingerprint();

// The authorization code, grouped as "7KQ2-M9XD-4TPA-W3HN".
std::string code();
// The authorization link: https://www.k0tfu.org/ktb#code=…&fp=…
std::string uri();
// Makes and saves a new authorization code. Returns false if it couldn't be saved.
bool rotate_code();

// Authorization mode
constexpr uint32_t mode_default_seconds = 300; // provisional
constexpr uint32_t mode_max_seconds = 3600;
void mode_start(uint32_t seconds);
void mode_end();
bool mode_is_on();
uint32_t mode_seconds_left();

// How many management apps are authorized. None until Phase E.
int app_count();

// NVS namespaces: the key and code (kept by reset), and the authorized
// management apps (cleared by reset).
constexpr const char *identity_namespace = "identity";
constexpr const char *apps_namespace = "apps";

} // namespace ktb::auth
