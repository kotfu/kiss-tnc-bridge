// The device's key, its setup code and the management window
// (docs/control-api.md, Authentication). The key and code are made on the
// first boot and kept through a reset; only erasing the flash replaces
// them. Enrollment over BLE comes in Phase E: until then the window opens
// and closes, but nothing can enroll through it.
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

// The setup code, grouped as "7KQ2-M9XD-4TPA-W3HN".
std::string code();
// The setup URI: https://www.k0tfu.org/ktb#code=…&fp=…
std::string uri();
// Makes and saves a new setup code. Returns false if it couldn't be saved.
bool rotate_code();

// The management window
constexpr uint32_t window_default_seconds = 300; // provisional
constexpr uint32_t window_max_seconds = 3600;
void window_open(uint32_t seconds);
void window_close();
bool window_is_open();
uint32_t window_seconds_left();

// How many clients are trusted. None until Phase E.
int client_count();

// NVS namespaces: the key and code (kept by reset), and the trusted
// clients (cleared by reset).
constexpr const char *identity_namespace = "identity";
constexpr const char *clients_namespace = "clients";

} // namespace ktb::auth
