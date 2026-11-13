// The StickC Plus SE's screen as the ATOM Lite's LED: led::show() fills
// the whole screen with the color, so the StickC runs the ATOM's UI
// (ui.cpp, docs/atom-lite-ui.md) unchanged until it has its own. Built
// instead of led.cpp on the StickC.
//
// led_brightness sets the backlight (the AXP192's LDO2), and the level
// (breathing, dim, the frame dip) scales the pixels. Off switches the
// backlight off rather than drawing black, which still glows and draws
// current. Pins and panel settings are the ones tried in
// spikes/stickc-plus-se.

#include <cmath>

#include "axp192.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "driver/spi_master.h"
#include "esp_heap_caps.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_lcd_panel_st7789.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "led.h"

namespace ktb::led {

namespace {

constexpr const char *TAG = "screen";

// docs.m5stack.com, StickC Plus SE; tried in spikes/stickc-plus-se
constexpr gpio_num_t pin_sda = GPIO_NUM_21;
constexpr gpio_num_t pin_scl = GPIO_NUM_22;
constexpr gpio_num_t pin_mosi = GPIO_NUM_15;
constexpr gpio_num_t pin_sclk = GPIO_NUM_13;
constexpr gpio_num_t pin_dc = GPIO_NUM_23;
constexpr gpio_num_t pin_rst = GPIO_NUM_18;
constexpr gpio_num_t pin_cs = GPIO_NUM_5;
constexpr gpio_num_t pin_red_led = GPIO_NUM_10; // active low
constexpr spi_host_device_t lcd_host = SPI2_HOST;

// The panel is 135x240 in the ST7789's 240x320 memory.
constexpr int width = 135;
constexpr int height = 240;
constexpr int x_gap = 52;
constexpr int y_gap = 40;

// One strip of 20 rows (5.4 KB of DMA-capable RAM), sent 12 times: the
// whole screen is one color, so every strip is the same.
constexpr int strip_rows = 20;
constexpr int strips = height / strip_rows;
static_assert(height % strip_rows == 0);

esp_lcd_panel_handle_t panel = nullptr;
SemaphoreHandle_t sent = nullptr;
uint16_t *strip = nullptr;
bool ready = false;
int32_t drawn = -1; // the color on the screen, -1 for none yet

bool on_sent(esp_lcd_panel_io_handle_t, esp_lcd_panel_io_event_data_t *, void *)
{
    BaseType_t woken = pdFALSE;
    xSemaphoreGiveFromISR(sent, &woken);
    return woken == pdTRUE;
}

// RGB565, byte-swapped: the panel takes it big-endian.
uint16_t rgb565(uint8_t r, uint8_t g, uint8_t b)
{
    uint16_t c = static_cast<uint16_t>(((r & 0xf8) << 8) | ((g & 0xfc) << 3) | (b >> 3));
    return static_cast<uint16_t>((c >> 8) | (c << 8));
}

uint8_t scale(uint8_t c, float k)
{
    float v = c * k + 0.5f;
    return v > 255 ? 255 : static_cast<uint8_t>(v);
}

void red_led_off()
{
    gpio_config_t cfg = {};
    cfg.pin_bit_mask = 1ULL << pin_red_led;
    cfg.mode = GPIO_MODE_OUTPUT;
    gpio_config(&cfg);
    gpio_set_level(pin_red_led, 1);
}

bool start_power()
{
    i2c_master_bus_config_t cfg = {};
    cfg.i2c_port = I2C_NUM_0;
    cfg.sda_io_num = pin_sda;
    cfg.scl_io_num = pin_scl;
    cfg.clk_source = I2C_CLK_SRC_DEFAULT;
    cfg.glitch_ignore_cnt = 7;
    cfg.flags.enable_internal_pullup = true;
    i2c_master_bus_handle_t bus = nullptr;
    if (i2c_new_master_bus(&cfg, &bus) != ESP_OK) {
        ESP_LOGE(TAG, "I2C bus didn't start");
        return false;
    }
    return axp192::init(bus);
}

bool start_panel()
{
    sent = xSemaphoreCreateCounting(strips, 0);
    strip = static_cast<uint16_t *>(heap_caps_malloc(width * strip_rows * 2, MALLOC_CAP_DMA));
    if (!sent || !strip) {
        ESP_LOGE(TAG, "no memory for a strip");
        return false;
    }
    spi_bus_config_t bus = {};
    bus.mosi_io_num = pin_mosi;
    bus.miso_io_num = -1;
    bus.sclk_io_num = pin_sclk;
    bus.quadwp_io_num = -1;
    bus.quadhd_io_num = -1;
    bus.max_transfer_sz = width * strip_rows * 2;
    if (spi_bus_initialize(lcd_host, &bus, SPI_DMA_CH_AUTO) != ESP_OK) {
        ESP_LOGE(TAG, "SPI bus didn't start");
        return false;
    }
    esp_lcd_panel_io_spi_config_t io_cfg = {};
    io_cfg.dc_gpio_num = pin_dc;
    io_cfg.cs_gpio_num = pin_cs;
    io_cfg.pclk_hz = 40 * 1000 * 1000;
    io_cfg.lcd_cmd_bits = 8;
    io_cfg.lcd_param_bits = 8;
    io_cfg.spi_mode = 0;
    io_cfg.trans_queue_depth = strips;
    io_cfg.on_color_trans_done = on_sent;
    esp_lcd_panel_io_handle_t io = nullptr;
    if (esp_lcd_new_panel_io_spi(static_cast<esp_lcd_spi_bus_handle_t>(lcd_host), &io_cfg, &io) != ESP_OK) {
        ESP_LOGE(TAG, "screen IO didn't start");
        return false;
    }
    esp_lcd_panel_dev_config_t panel_cfg = {};
    panel_cfg.reset_gpio_num = pin_rst;
    panel_cfg.rgb_ele_order = LCD_RGB_ELEMENT_ORDER_RGB;
    panel_cfg.bits_per_pixel = 16;
    if (esp_lcd_new_panel_st7789(io, &panel_cfg, &panel) != ESP_OK) {
        ESP_LOGE(TAG, "screen driver didn't start");
        return false;
    }
    esp_lcd_panel_reset(panel);
    esp_lcd_panel_init(panel);
    esp_lcd_panel_invert_color(panel, true);
    esp_lcd_panel_set_gap(panel, x_gap, y_gap);
    esp_lcd_panel_disp_on_off(panel, true);
    return true;
}

// Every strip queued at once, then waited for: about 13 ms at 40 MHz.
void fill(uint16_t c)
{
    for (int i = 0; i < width * strip_rows; i++) {
        strip[i] = c;
    }
    int queued = 0;
    for (int s = 0; s < strips; s++) {
        if (esp_lcd_panel_draw_bitmap(panel, 0, s * strip_rows, width, (s + 1) * strip_rows, strip) == ESP_OK) {
            queued++;
        }
    }
    for (int i = 0; i < queued; i++) {
        xSemaphoreTake(sent, pdMS_TO_TICKS(100));
    }
}

} // namespace

void init()
{
    if (ready) {
        return;
    }
    red_led_off();
    if (!start_power() || !start_panel()) {
        ESP_LOGE(TAG, "the screen didn't start: the device runs without it");
        return;
    }
    // Black underneath, so whatever the panel held never shows.
    fill(rgb565(0, 0, 0));
    drawn = 0;
    ready = true;
    ESP_LOGI(TAG, "ready");
}

void show(Rgb color, uint8_t brightness, float level)
{
    if (!ready) {
        return;
    }
    if (color == off || brightness == 0 || level <= 0) {
        axp192::backlight(0);
        return;
    }
    // The level is light, as on the LED; the panel's values aren't linear
    // in light (a gamma of about 2.2), so they're scaled by its root.
    float k = level >= 1 ? 1.0f : powf(level, 1 / 2.2f);
    uint16_t c = rgb565(scale(color.r, k), scale(color.g, k), scale(color.b, k));
    if (c != drawn) {
        fill(c);
        drawn = c;
    }
    // Drawn first, then lit, so the backlight never shows the last color.
    axp192::backlight(brightness);
}

} // namespace ktb::led
