// What reset clears (docs/atom-lite-ui.md, Reset): every setting, the
// radio pairing and the trusted clients. The device's key and setup code
// stay.
#pragma once

namespace ktb::storage {

// Opens NVS. Call before anything that reads it.
void init();

// Erases everything reset clears. Returns false if part of it failed.
bool reset();

} // namespace ktb::storage
