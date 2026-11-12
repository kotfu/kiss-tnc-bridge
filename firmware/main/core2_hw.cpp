#include "core2_hw.h"

#include "axp192.h"
#include "driver/i2c_master.h"
#include "driver/spi_master.h"
#include "esp_heap_caps.h"
#include "esp_lcd_ili9341.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "esp_lvgl_port.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

namespace ktb::core2 {

namespace {

constexpr const char *TAG = "core2";

// Pins (docs.m5stack.com, Core2 v1.3)
constexpr gpio_num_t pin_sda = GPIO_NUM_21;
constexpr gpio_num_t pin_scl = GPIO_NUM_22;
constexpr gpio_num_t pin_mosi = GPIO_NUM_23;
constexpr gpio_num_t pin_miso = GPIO_NUM_38; // shared with the SD card
constexpr gpio_num_t pin_sclk = GPIO_NUM_18;
constexpr gpio_num_t pin_lcd_cs = GPIO_NUM_5;
constexpr gpio_num_t pin_lcd_dc = GPIO_NUM_15;
constexpr spi_host_device_t lcd_host = SPI2_HOST;

// LVGL draws a strip at a time into internal RAM, which SPI DMA can read
// (PSRAM it can't, on the original ESP32): two strips of 24 rows, 15 KB
// each.
constexpr int strip_rows = 24;

i2c_master_bus_handle_t i2c_bus = nullptr;
i2c_master_dev_handle_t touch = nullptr;
lv_display_t *disp = nullptr;

bool start_i2c()
{
    i2c_master_bus_config_t cfg = {};
    cfg.i2c_port = I2C_NUM_0;
    cfg.sda_io_num = pin_sda;
    cfg.scl_io_num = pin_scl;
    cfg.clk_source = I2C_CLK_SRC_DEFAULT;
    cfg.glitch_ignore_cnt = 7;
    cfg.flags.enable_internal_pullup = true;
    return i2c_new_master_bus(&cfg, &i2c_bus) == ESP_OK;
}

bool start_panel(esp_lcd_panel_io_handle_t &io, esp_lcd_panel_handle_t &panel)
{
    spi_bus_config_t bus = {};
    bus.mosi_io_num = pin_mosi;
    bus.miso_io_num = pin_miso;
    bus.sclk_io_num = pin_sclk;
    bus.quadwp_io_num = -1;
    bus.quadhd_io_num = -1;
    bus.max_transfer_sz = width * strip_rows * 2;
    if (spi_bus_initialize(lcd_host, &bus, SPI_DMA_CH_AUTO) != ESP_OK) {
        ESP_LOGE(TAG, "SPI bus didn't start");
        return false;
    }
    esp_lcd_panel_io_spi_config_t io_cfg = {};
    io_cfg.dc_gpio_num = pin_lcd_dc;
    io_cfg.cs_gpio_num = pin_lcd_cs;
    io_cfg.pclk_hz = 40 * 1000 * 1000;
    io_cfg.lcd_cmd_bits = 8;
    io_cfg.lcd_param_bits = 8;
    io_cfg.spi_mode = 0;
    io_cfg.trans_queue_depth = 10;
    if (esp_lcd_new_panel_io_spi(static_cast<esp_lcd_spi_bus_handle_t>(lcd_host), &io_cfg, &io) != ESP_OK) {
        ESP_LOGE(TAG, "screen IO didn't start");
        return false;
    }
    esp_lcd_panel_dev_config_t panel_cfg = {};
    panel_cfg.reset_gpio_num = GPIO_NUM_NC; // the AXP192's GPIO4, reset in axp192::init
    panel_cfg.rgb_ele_order = LCD_RGB_ELEMENT_ORDER_BGR;
    panel_cfg.bits_per_pixel = 16;
    if (esp_lcd_new_panel_ili9341(io, &panel_cfg, &panel) != ESP_OK) {
        ESP_LOGE(TAG, "screen driver didn't start");
        return false;
    }
    esp_lcd_panel_reset(panel);
    esp_lcd_panel_init(panel);
    esp_lcd_panel_invert_color(panel, true); // the ILI9342 in the Core2 is inverted
    esp_lcd_panel_disp_on_off(panel, true);
    return true;
}

// The touch controller, an FT6336U, read directly. Espressif's FT5x06
// driver writes its own thresholds and timings into it (some into
// registers the FT6336U doesn't have), over the panel maker's tuning;
// with those, touch stopped as soon as the Core2 came off USB. M5Stack's
// code leaves the tuning alone, and so does this.
constexpr uint8_t touch_address = 0x38;
constexpr uint8_t FT_TD_STATUS = 0x02; // points touched, then 6 bytes a point
constexpr uint8_t FT_TH_GROUP = 0x80;  // touch threshold
constexpr uint8_t FT_CTRL = 0x86;      // 1: low-power scanning when idle
constexpr uint8_t FT_G_MODE = 0xA4;
constexpr uint8_t FT_CHIP_ID = 0xA3;
constexpr uint8_t FT_FIRMWARE = 0xA6;

bool touch_read(uint8_t reg, uint8_t *buf, size_t len)
{
    return i2c_master_transmit_receive(touch, &reg, 1, buf, len, 50) == ESP_OK;
}

bool start_touch()
{
    i2c_device_config_t cfg = {};
    cfg.dev_addr_length = I2C_ADDR_BIT_LEN_7;
    cfg.device_address = touch_address;
    cfg.scl_speed_hz = 400000;
    if (i2c_master_bus_add_device(i2c_bus, &cfg, &touch) != ESP_OK) {
        ESP_LOGE(TAG, "touch device couldn't be added");
        return false;
    }
    // It takes a moment after LDO2 comes on.
    uint8_t id = 0;
    for (int i = 0; i < 20 && !touch_read(FT_CHIP_ID, &id, 1); i++) {
        vTaskDelay(pdMS_TO_TICKS(25));
    }
    uint8_t th = 0, ctrl = 0, mode = 0, fw = 0;
    if (!touch_read(FT_CHIP_ID, &id, 1)) {
        ESP_LOGE(TAG, "touch didn't answer at 0x%02x", touch_address);
        return false;
    }
    touch_read(FT_TH_GROUP, &th, 1);
    touch_read(FT_CTRL, &ctrl, 1);
    touch_read(FT_G_MODE, &mode, 1);
    touch_read(FT_FIRMWARE, &fw, 1);
    ESP_LOGI(TAG, "touch: chip 0x%02x, firmware 0x%02x, threshold %u, ctrl %u, mode %u", id, fw, th, ctrl, mode);
    return true;
}

} // namespace

bool start()
{
    if (!start_i2c()) {
        ESP_LOGE(TAG, "I2C didn't start");
        return false;
    }
    if (!axp192::init(i2c_bus)) {
        return false;
    }
    esp_lcd_panel_io_handle_t io = nullptr;
    esp_lcd_panel_handle_t panel = nullptr;
    if (!start_panel(io, panel)) {
        return false;
    }
    bool touch_ok = start_touch();

    lvgl_port_cfg_t port = ESP_LVGL_PORT_INIT_CONFIG();
    port.task_affinity = 1; // away from the Bluetooth controller on core 0
    if (lvgl_port_init(&port) != ESP_OK) {
        ESP_LOGE(TAG, "LVGL didn't start");
        return false;
    }
    lvgl_port_display_cfg_t dcfg = {};
    dcfg.io_handle = io;
    dcfg.panel_handle = panel;
    dcfg.buffer_size = width * strip_rows;
    dcfg.double_buffer = true;
    dcfg.hres = width;
    dcfg.vres = height;
    dcfg.color_format = LV_COLOR_FORMAT_RGB565;
    dcfg.flags.buff_dma = 1;
    dcfg.flags.swap_bytes = 1;
    disp = lvgl_port_add_disp(&dcfg);
    if (!disp) {
        ESP_LOGE(TAG, "LVGL display didn't start");
        return false;
    }
    ESP_LOGI(TAG, "screen up%s", touch_ok ? ", touch up" : ", no touch");
    return touch_ok;
}

int read_touch(TouchPoint *points, int max)
{
    if (!touch) {
        return 0;
    }
    uint8_t b[13]; // TD_STATUS, then two points of 6 bytes
    if (!touch_read(FT_TD_STATUS, b, sizeof(b))) {
        // Logged at most every 5 s: touch that stops answering shows here.
        static int64_t logged = -5000;
        static uint32_t failures = 0;
        failures++;
        int64_t now = esp_timer_get_time() / 1000;
        if (now - logged >= 5000) {
            ESP_LOGW(TAG, "touch didn't answer, %lu times", (unsigned long)failures);
            logged = now;
        }
        return 0;
    }
    int n = b[0] & 0x0f;
    if (n > 2) {
        n = 0; // 0x0f and the like: nothing valid
    }
    int out = 0;
    for (int i = 0; i < n && out < max; i++) {
        const uint8_t *p = b + 1 + i * 6;
        uint8_t event = p[0] >> 6; // 0 down, 1 up, 2 contact, 3 none
        if (event == 1 || event == 3) {
            continue;
        }
        uint16_t x = static_cast<uint16_t>(((p[0] & 0x0f) << 8) | p[1]);
        uint16_t y = static_cast<uint16_t>(((p[2] & 0x0f) << 8) | p[3]);
        points[out++] = {x, y};
    }
    return out;
}

lv_display_t *display() { return disp; }

bool lock(uint32_t timeout_ms) { return lvgl_port_lock(timeout_ms); }
void unlock() { lvgl_port_unlock(); }

} // namespace ktb::core2

// LVGL's memory, in PSRAM (CONFIG_LV_USE_CUSTOM_MALLOC): internal RAM is
// for Bluetooth and the draw strips.
extern "C" {

void lv_mem_init(void) {}
void lv_mem_deinit(void) {}
lv_mem_pool_t lv_mem_add_pool(void *, size_t) { return nullptr; }
void lv_mem_remove_pool(lv_mem_pool_t) {}

void *lv_malloc_core(size_t size)
{
    void *p = heap_caps_malloc(size, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    return p ? p : heap_caps_malloc(size, MALLOC_CAP_8BIT);
}

void *lv_realloc_core(void *p, size_t new_size)
{
    void *q = heap_caps_realloc(p, new_size, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    return q ? q : heap_caps_realloc(p, new_size, MALLOC_CAP_8BIT);
}

void lv_free_core(void *p) { heap_caps_free(p); }

void lv_mem_monitor_core(lv_mem_monitor_t *mon) { lv_memzero(mon, sizeof(*mon)); }

lv_result_t lv_mem_test_core(void) { return LV_RESULT_OK; }

} // extern "C"
