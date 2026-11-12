#include "axp192.h"

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

namespace ktb::axp192 {

namespace {

constexpr const char *TAG = "axp192";
constexpr uint8_t address = 0x34;
constexpr int timeout_ms = 100;

// Registers (AXP192 datasheet)
constexpr uint8_t REG_POWER_STATUS = 0x00; // bit 7 ACIN, bit 5 VBUS, bit 2 charging direction
constexpr uint8_t REG_CHARGE_STATUS = 0x01; // bit 6 charging, bit 5 battery present
constexpr uint8_t REG_OUTPUTS = 0x12;      // bit 0 DCDC1, 1 DCDC3, 2 LDO2, 3 LDO3
constexpr uint8_t REG_DCDC1_V = 0x26;      // the ESP32's 3.3 V
constexpr uint8_t REG_DCDC3_V = 0x27;      // the backlight
constexpr uint8_t REG_LDO23_V = 0x28;      // LDO2 (high nibble): the screen's logic and the SD card
constexpr uint8_t REG_VBUS_IPSOUT = 0x30;
constexpr uint8_t REG_SHUTDOWN = 0x32;     // bit 7 powers off
constexpr uint8_t REG_CHARGE_1 = 0x33;     // the charge current, low nibble
constexpr uint8_t REG_BACKUP_CHARGE = 0x35;
constexpr uint8_t REG_PEK = 0x36;          // the power button's timings
constexpr uint8_t REG_IRQ_EN_3 = 0x42;     // bits 1, 0: the button's short and long press
constexpr uint8_t REG_IRQ_STATUS_3 = 0x46;
constexpr uint8_t REG_BATT_V = 0x78;       // 12 bits, 1.1 mV
constexpr uint8_t REG_CHARGE_I = 0x7A;     // 13 bits, 0.5 mA
constexpr uint8_t REG_DISCHARGE_I = 0x7C;  // 13 bits, 0.5 mA
constexpr uint8_t REG_ADC_EN_1 = 0x82;
constexpr uint8_t REG_GPIO1_CTRL = 0x92;   // the green power LED
constexpr uint8_t REG_GPIO2_CTRL = 0x93;   // the speaker amplifier's enable
constexpr uint8_t REG_GPIO012_DATA = 0x94;
constexpr uint8_t REG_GPIO34_CTRL = 0x95;  // GPIO4: the screen's reset
constexpr uint8_t REG_GPIO34_DATA = 0x96;

// DCDC3 in 25 mV steps from 700 mV. Full is 3.3 V, as M5Stack sets it;
// dim is provisional: 2.7 V looked too bright on a device.
constexpr uint8_t backlight_full = (3300 - 700) / 25;
constexpr uint8_t backlight_dim = (2500 - 700) / 25; // M5GFX's lowest

i2c_master_dev_handle_t dev = nullptr;

bool read(uint8_t reg, uint8_t *out, size_t n = 1)
{
    return dev && i2c_master_transmit_receive(dev, &reg, 1, out, n, timeout_ms) == ESP_OK;
}

uint8_t read8(uint8_t reg)
{
    uint8_t v = 0;
    read(reg, &v);
    return v;
}

bool write8(uint8_t reg, uint8_t v)
{
    uint8_t buf[2] = {reg, v};
    return dev && i2c_master_transmit(dev, buf, sizeof(buf), timeout_ms) == ESP_OK;
}

// Read, change, write: if the read fails, nothing is written, so a bad
// read can't switch rails off.
bool update(uint8_t reg, uint8_t keep_mask, uint8_t set)
{
    uint8_t v;
    if (!read(reg, &v)) {
        ESP_LOGW(TAG, "couldn't read 0x%02x: left as it was", reg);
        return false;
    }
    return write8(reg, static_cast<uint8_t>((v & keep_mask) | set));
}

// A rough state of charge from the voltage at rest (a typical Li-ion
// curve). It reads high while charging and low under load; the coulomb
// counter can do better (roadmap, Core2 firmware).
int percent_from(float v)
{
    static const struct {
        float v;
        int pct;
    } curve[] = {{4.15f, 100}, {4.05f, 90}, {3.97f, 80}, {3.90f, 70}, {3.84f, 60}, {3.79f, 50},
                 {3.75f, 40},   {3.71f, 30}, {3.67f, 20}, {3.61f, 10}, {3.50f, 5},  {3.30f, 0}};
    if (v >= curve[0].v) {
        return 100;
    }
    for (size_t i = 1; i < sizeof(curve) / sizeof(curve[0]); i++) {
        if (v >= curve[i].v) {
            float f = (v - curve[i].v) / (curve[i - 1].v - curve[i].v);
            return curve[i].pct + static_cast<int>(f * (curve[i - 1].pct - curve[i].pct) + 0.5f);
        }
    }
    return 0;
}

} // namespace

bool init(i2c_master_bus_handle_t bus)
{
    i2c_device_config_t cfg = {};
    cfg.dev_addr_length = I2C_ADDR_BIT_LEN_7;
    cfg.device_address = address;
    cfg.scl_speed_hz = 400000;
    if (i2c_master_bus_add_device(bus, &cfg, &dev) != ESP_OK) {
        ESP_LOGE(TAG, "couldn't add the device");
        return false;
    }
    uint8_t outputs;
    if (!read(REG_OUTPUTS, &outputs)) {
        ESP_LOGE(TAG, "no answer at 0x%02x", address);
        return false;
    }
    // M5Stack's own Core2 set-up (M5Unified, Power_Class), which the
    // factory firmware runs. Espressif's board package leaves out the 5 V
    // bus, battery monitoring and VOFF, and without the bus output touch
    // stopped answering on battery.
    static const uint8_t regs[][2] = {
        {REG_DCDC1_V, 0x6a},       // the ESP32 at 3.35 V
        {REG_DCDC3_V, backlight_full},
        {REG_VBUS_IPSOUT, 0x02},   // VBUS current limit at 500 mA
        {0x31, 0x04},              // VOFF: shut down at 3.0 V
        {REG_SHUTDOWN, 0x42},      // battery monitoring on; the charge LED follows charging
        {REG_CHARGE_1, 0xc3},      // charge to 4.2 V at 360 mA (M5Unified: 390 mA, the nearest step)
        {REG_BACKUP_CHARGE, 0xa2}, // charge the clock's backup battery
        // The power button: on after 512 ms, a long press at 1 s, and off
        // after 6 s held, in the chip.
        {REG_PEK, 0x4d},
        {0x40, 0x00},              // no IRQs but the power button's
        {0x41, 0x00},
        {REG_IRQ_EN_3, 0x03},
        {0x43, 0x00},
        {0x44, 0x00},
        {REG_IRQ_STATUS_3, 0x03},  // forget presses from before
        {REG_ADC_EN_1, 0xff},      // every ADC: battery voltage and currents
        {0x83, 0x80},              // and the chip's temperature
        {0x84, 0x32},              // at 25 Hz
        {0x91, 0xa0},              // GPIO0 as an LDO at 2.8 V
        {0x90, 0x02},              // GPIO0 on: with EXTEN below, the 5 V bus output
        {REG_GPIO1_CTRL, 0x02},    // GPIO1 (the green LED) as PWM,
        {0x98, 0x00},
        {0x99, 0xff},
        {0x9a, 0xff},              // off
        {REG_GPIO2_CTRL, 0x00},    // GPIO2: an output,
    };
    for (auto &r : regs) {
        write8(r[0], r[1]);
    }
    update(REG_GPIO012_DATA, 0xfb, 0x00);  // and low: the speaker amplifier off
    update(REG_LDO23_V, 0x0f, 0xf0);       // LDO2 at 3.3 V
    // LDO2 off for a moment first: a reset (the ESP32's EN) doesn't reach
    // the touch controller, so this is what starts it afresh, with the
    // panel maker's tuning and a new baseline.
    write8(REG_OUTPUTS, static_cast<uint8_t>((outputs | 0x41) & ~0x0e));
    vTaskDelay(pdMS_TO_TICKS(100));
    // DCDC1 and LDO2 (the screen and touch) on, EXTEN on (the 5 V bus),
    // LDO3 (the vibration motor) off, and DCDC3 off: the backlight comes on
    // once the splash is drawn.
    write8(REG_OUTPUTS, static_cast<uint8_t>(((outputs | 0x45) & ~0x0a)));
    update(REG_GPIO34_CTRL, 0x72, 0x84);   // GPIO4: output, for the screen's reset

    // Reset the screen.
    update(REG_GPIO34_DATA, static_cast<uint8_t>(~0x02), 0x00);
    vTaskDelay(pdMS_TO_TICKS(20));
    update(REG_GPIO34_DATA, 0xff, 0x02);
    vTaskDelay(pdMS_TO_TICKS(20));
    ESP_LOGI(TAG, "rails set: power 0x%02x, charge 0x%02x, outputs 0x%02x, GPIO0 0x%02x", read8(REG_POWER_STATUS),
             read8(REG_CHARGE_STATUS), read8(REG_OUTPUTS), read8(0x90));
    return true;
}

void backlight(Backlight b)
{
    if (b == Backlight::off) {
        update(REG_OUTPUTS, static_cast<uint8_t>(~0x02), 0x00);
        return;
    }
    write8(REG_DCDC3_V, b == Backlight::dim ? backlight_dim : backlight_full);
    update(REG_OUTPUTS, 0xff, 0x02);
}

Power read()
{
    Power p;
    uint8_t status = read8(REG_POWER_STATUS);
    uint8_t charge = read8(REG_CHARGE_STATUS);
    // USB comes in on ACIN. VBUS is the 5 V bus, which EXTEN powers from
    // the battery (init), so it's always there: it isn't external power.
    p.external = status & 0x80;
    p.battery = charge & 0x20;
    p.charging = (charge & 0x40) && p.external;
    uint8_t b[2];
    if (read(REG_BATT_V, b, 2)) {
        p.battery_v = ((b[0] << 4) | (b[1] & 0x0f)) * 1.1f / 1000.0f;
    }
    if (read(REG_CHARGE_I, b, 2)) {
        p.charge_ma = ((b[0] << 5) | (b[1] & 0x1f)) * 0.5f;
    }
    if (read(REG_DISCHARGE_I, b, 2)) {
        p.discharge_ma = ((b[0] << 5) | (b[1] & 0x1f)) * 0.5f;
    }
    p.percent = p.battery ? percent_from(p.battery_v) : 0;
    // The voltage reaches the top of the curve while the charger is still
    // topping up (until under 10% of its current): not full until it stops.
    if (p.charging && p.percent > 99) {
        p.percent = 99;
    }
    return p;
}

int short_presses()
{
    uint8_t irq = read8(REG_IRQ_STATUS_3);
    if (irq & 0x03) {
        write8(REG_IRQ_STATUS_3, irq & 0x03);
    }
    return (irq & 0x02) ? 1 : 0;
}

void power_off()
{
    ESP_LOGW(TAG, "powering off");
    update(REG_SHUTDOWN, 0xff, 0x80);
    vTaskDelay(pdMS_TO_TICKS(1000));
}

} // namespace ktb::axp192
