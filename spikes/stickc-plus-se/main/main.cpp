// Spike: bring up the M5Stack StickC Plus SE (spikes/stickc-plus-se/README.md).
// Throwaway code.
//
// 1. Scan the internal I2C bus and dump the AXP192 as the factory firmware
//    left it (it keeps its settings through an ESP32 reset). Nothing is
//    written until that's printed.
// 2. Switch on the screen's rails (LDO2, LDO3) only if they're off.
// 3. The ST7789v2: fills and a border, for the offsets and colour order.
// 4. The red LED, then three tones on the passive buzzer.
// 5. Loop: buttons A and B, the power button, and the battery.

#include <cstdio>
#include <cstring>

#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "driver/ledc.h"
#include "driver/spi_master.h"
#include "esp_heap_caps.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_lcd_panel_st7789.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "sdkconfig.h"
#if CONFIG_SPIKE_LVGL
#include "esp_lvgl_port.h"
#endif

namespace {

constexpr const char *TAG = "spike";

// docs.m5stack.com, StickC Plus SE
constexpr gpio_num_t I2C_SDA = GPIO_NUM_21;
constexpr gpio_num_t I2C_SCL = GPIO_NUM_22;
constexpr gpio_num_t LCD_MOSI = GPIO_NUM_15;
constexpr gpio_num_t LCD_CLK = GPIO_NUM_13;
constexpr gpio_num_t LCD_DC = GPIO_NUM_23;
constexpr gpio_num_t LCD_RST = GPIO_NUM_18;
constexpr gpio_num_t LCD_CS = GPIO_NUM_5;
constexpr gpio_num_t LED = GPIO_NUM_10;
constexpr gpio_num_t BUTTON_A = GPIO_NUM_37;
constexpr gpio_num_t BUTTON_B = GPIO_NUM_39;
constexpr gpio_num_t BUZZER = GPIO_NUM_2;

// The panel is 135x240 in the ST7789's 240x320 memory.
constexpr int W = 135;
constexpr int H = 240;
constexpr int X_GAP = 52;
constexpr int Y_GAP = 40;

constexpr uint8_t AXP = 0x34;

i2c_master_bus_handle_t bus = nullptr;
i2c_master_dev_handle_t axp = nullptr;

void heap(const char *when)
{
    ESP_LOGI(TAG, "heap %s: free %u, internal %u, largest %u, min %u", when,
             (unsigned)esp_get_free_heap_size(), (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL),
             (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL),
             (unsigned)esp_get_minimum_free_heap_size());
}

bool axp_read(uint8_t reg, uint8_t *out, size_t n = 1)
{
    return axp && i2c_master_transmit_receive(axp, &reg, 1, out, n, 100) == ESP_OK;
}

uint8_t axp_read8(uint8_t reg)
{
    uint8_t v = 0xee;
    axp_read(reg, &v);
    return v;
}

bool axp_write8(uint8_t reg, uint8_t v)
{
    uint8_t buf[2] = {reg, v};
    return axp && i2c_master_transmit(axp, buf, 2, 100) == ESP_OK;
}

void i2c_scan()
{
    i2c_master_bus_config_t cfg = {};
    cfg.i2c_port = I2C_NUM_0;
    cfg.sda_io_num = I2C_SDA;
    cfg.scl_io_num = I2C_SCL;
    cfg.clk_source = I2C_CLK_SRC_DEFAULT;
    cfg.glitch_ignore_cnt = 7;
    cfg.flags.enable_internal_pullup = true;
    ESP_ERROR_CHECK(i2c_new_master_bus(&cfg, &bus));
    char found[256] = "";
    for (uint16_t a = 0x08; a < 0x78; a++) {
        if (i2c_master_probe(bus, a, 50) == ESP_OK) {
            char one[8];
            snprintf(one, sizeof(one), " 0x%02x", a);
            strncat(found, one, sizeof(found) - strlen(found) - 1);
        }
    }
    ESP_LOGI(TAG, "I2C (SDA %d, SCL %d):%s", I2C_SDA, I2C_SCL, found[0] ? found : " nothing");
    if (i2c_master_probe(bus, AXP, 50) != ESP_OK) {
        ESP_LOGE(TAG, "no AXP192 at 0x%02x", AXP);
        return;
    }
    i2c_device_config_t dev = {};
    dev.dev_addr_length = I2C_ADDR_BIT_LEN_7;
    dev.device_address = AXP;
    dev.scl_speed_hz = 400000;
    ESP_ERROR_CHECK(i2c_master_bus_add_device(bus, &dev, &axp));
}

void axp_dump()
{
    static const struct {
        uint8_t reg;
        const char *what;
    } regs[] = {
        {0x00, "power status (b7 ACIN, b5 VBUS)"},
        {0x01, "charge status (b6 charging, b5 battery)"},
        {0x03, "chip id"},
        {0x10, "EXTEN/DCDC2"},
        {0x12, "outputs (b6 EXTEN, b4 DCDC2, b3 LDO3, b2 LDO2, b1 DCDC3, b0 DCDC1)"},
        {0x26, "DCDC1 V"},
        {0x27, "DCDC3 V"},
        {0x28, "LDO2 (hi) / LDO3 (lo) V"},
        {0x30, "VBUS-IPSOUT"},
        {0x31, "VOFF"},
        {0x32, "shutdown / battery detect"},
        {0x33, "charge 1"},
        {0x35, "backup charge"},
        {0x36, "PEK (power button)"},
        {0x42, "IRQ enable 3"},
        {0x46, "IRQ status 3"},
        {0x82, "ADC enable 1"},
        {0x83, "ADC enable 2"},
        {0x84, "ADC rate"},
        {0x90, "GPIO0 control"},
        {0x91, "GPIO0 LDO V"},
        {0x92, "GPIO1 control"},
        {0x93, "GPIO2 control"},
        {0x94, "GPIO0-2 data"},
        {0x95, "GPIO3-4 control"},
        {0x96, "GPIO3-4 data"},
    };
    for (auto &r : regs) {
        ESP_LOGI(TAG, "AXP 0x%02x = 0x%02x  %s", r.reg, axp_read8(r.reg), r.what);
    }
}

void axp_screen_on()
{
    uint8_t out = axp_read8(0x12);
    if ((out & 0x0c) != 0x0c) {
        ESP_LOGW(TAG, "LDO2/LDO3 were off (0x12 = 0x%02x): switching them on", out);
        if (axp_read8(0x28) == 0x00) {
            axp_write8(0x28, 0xcc); // both at 3.0 V
        }
        axp_write8(0x12, out | 0x0c);
        vTaskDelay(pdMS_TO_TICKS(50));
    }
    // The power button's short and long presses, cleared.
    axp_write8(0x42, axp_read8(0x42) | 0x03);
    axp_write8(0x46, 0x03);
    // Battery voltage and currents.
    axp_write8(0x82, axp_read8(0x82) | 0xc0);
}

// ── Screen ──────────────────────────────────────────────────────────────

esp_lcd_panel_handle_t panel = nullptr;
esp_lcd_panel_io_handle_t io = nullptr;
SemaphoreHandle_t flushed = nullptr;
uint16_t *frame = nullptr;

bool on_flushed(esp_lcd_panel_io_handle_t, esp_lcd_panel_io_event_data_t *, void *)
{
    BaseType_t woken = pdFALSE;
    xSemaphoreGiveFromISR(flushed, &woken);
    return woken == pdTRUE;
}

constexpr uint16_t rgb(uint8_t r, uint8_t g, uint8_t b)
{
    uint16_t c = ((r & 0xf8) << 8) | ((g & 0xfc) << 3) | (b >> 3);
    return static_cast<uint16_t>((c >> 8) | (c << 8)); // the panel takes it big-endian
}

void show()
{
    esp_lcd_panel_draw_bitmap(panel, 0, 0, W, H, frame);
    xSemaphoreTake(flushed, portMAX_DELAY);
}

void fill(uint16_t c)
{
    for (int i = 0; i < W * H; i++) {
        frame[i] = c;
    }
}

void rect(int x, int y, int w, int h, uint16_t c)
{
    for (int j = y; j < y + h && j < H; j++) {
        for (int i = x; i < x + w && i < W; i++) {
            frame[j * W + i] = c;
        }
    }
}

void lcd_init()
{
    flushed = xSemaphoreCreateBinary();
    spi_bus_config_t buscfg = {};
    buscfg.sclk_io_num = LCD_CLK;
    buscfg.mosi_io_num = LCD_MOSI;
    buscfg.miso_io_num = -1;
    buscfg.quadwp_io_num = -1;
    buscfg.quadhd_io_num = -1;
    buscfg.max_transfer_sz = W * H * 2;
    ESP_ERROR_CHECK(spi_bus_initialize(SPI2_HOST, &buscfg, SPI_DMA_CH_AUTO));

    esp_lcd_panel_io_spi_config_t iocfg = {};
    iocfg.dc_gpio_num = LCD_DC;
    iocfg.cs_gpio_num = LCD_CS;
    iocfg.pclk_hz = 40 * 1000 * 1000;
    iocfg.lcd_cmd_bits = 8;
    iocfg.lcd_param_bits = 8;
    iocfg.spi_mode = 0;
    iocfg.trans_queue_depth = 4;
#if !CONFIG_SPIKE_LVGL
    iocfg.on_color_trans_done = on_flushed; // esp_lvgl_port sets its own
#endif
    ESP_ERROR_CHECK(esp_lcd_new_panel_io_spi(static_cast<esp_lcd_spi_bus_handle_t>(SPI2_HOST), &iocfg, &io));

    esp_lcd_panel_dev_config_t pcfg = {};
    pcfg.reset_gpio_num = LCD_RST;
    pcfg.rgb_ele_order = LCD_RGB_ELEMENT_ORDER_RGB;
    pcfg.bits_per_pixel = 16;
    ESP_ERROR_CHECK(esp_lcd_new_panel_st7789(io, &pcfg, &panel));
    ESP_ERROR_CHECK(esp_lcd_panel_reset(panel));
    ESP_ERROR_CHECK(esp_lcd_panel_init(panel));
    ESP_ERROR_CHECK(esp_lcd_panel_invert_color(panel, true));
    ESP_ERROR_CHECK(esp_lcd_panel_set_gap(panel, X_GAP, Y_GAP));
    ESP_ERROR_CHECK(esp_lcd_panel_disp_on_off(panel, true));

#if !CONFIG_SPIKE_LVGL
    frame = static_cast<uint16_t *>(heap_caps_malloc(W * H * 2, MALLOC_CAP_DMA));
    if (!frame) {
        ESP_LOGE(TAG, "no %d bytes of DMA memory for a frame", W * H * 2);
    }
#endif
}

#if CONFIG_SPIKE_LVGL
// A status screen about the size of the real one: a top bar, the radio's
// state in a large font, the phones, counts and two soft keys.
void lvgl_status()
{
    lvgl_port_cfg_t port = ESP_LVGL_PORT_INIT_CONFIG();
    port.task_affinity = 1;
    ESP_ERROR_CHECK(lvgl_port_init(&port));
    heap("after lvgl_port_init");
    lvgl_port_display_cfg_t d = {};
    d.io_handle = io;
    d.panel_handle = panel;
    d.buffer_size = W * 20;
    d.double_buffer = true;
    d.hres = W;
    d.vres = H;
    d.color_format = LV_COLOR_FORMAT_RGB565;
    d.flags.buff_dma = 1;
    d.flags.swap_bytes = 1;
    lv_display_t *disp = lvgl_port_add_disp(&d);
    heap("after the display (two 20-row strips)");
    lvgl_port_lock(0);
    lv_obj_t *scr = lv_display_get_screen_active(disp);
    lv_obj_set_style_bg_color(scr, lv_color_black(), 0);
    lv_obj_t *bar = lv_obj_create(scr);
    lv_obj_set_size(bar, W, 22);
    lv_obj_align(bar, LV_ALIGN_TOP_MID, 0, 0);
    lv_obj_t *name = lv_label_create(bar);
    lv_label_set_text(name, "KTB-F5B4");
    lv_obj_set_style_text_font(name, &lv_font_montserrat_12, 0);
    lv_obj_align(name, LV_ALIGN_LEFT_MID, -8, 0);
    lv_obj_t *batt = lv_label_create(bar);
    lv_label_set_text(batt, "87% " LV_SYMBOL_BATTERY_3);
    lv_obj_set_style_text_font(batt, &lv_font_montserrat_12, 0);
    lv_obj_align(batt, LV_ALIGN_RIGHT_MID, 8, 0);
    lv_obj_t *state = lv_label_create(scr);
    lv_label_set_text(state, "Radio\nconnected");
    lv_obj_set_style_text_font(state, &lv_font_montserrat_20, 0);
    lv_obj_set_style_text_color(state, lv_color_hex(0x40c060), 0);
    lv_obj_align(state, LV_ALIGN_TOP_LEFT, 6, 36);
    lv_obj_t *radio = lv_label_create(scr);
    lv_label_set_text(radio, "TH-D75\n40:79:12:E4:95:9D");
    lv_obj_set_style_text_font(radio, &lv_font_montserrat_12, 0);
    lv_obj_align(radio, LV_ALIGN_TOP_LEFT, 6, 92);
    lv_obj_t *phones = lv_label_create(scr);
    lv_label_set_text(phones, LV_SYMBOL_BLUETOOTH " 2 phones");
    lv_obj_set_style_text_font(phones, &lv_font_montserrat_16, 0);
    lv_obj_align(phones, LV_ALIGN_TOP_LEFT, 6, 130);
    lv_obj_t *counts = lv_label_create(scr);
    lv_label_set_text(counts, LV_SYMBOL_DOWNLOAD " 128   " LV_SYMBOL_UPLOAD " 17");
    lv_obj_set_style_text_font(counts, &lv_font_montserrat_14, 0);
    lv_obj_align(counts, LV_ALIGN_TOP_LEFT, 6, 160);
    for (int i = 0; i < 2; i++) {
        lv_obj_t *key = lv_button_create(scr);
        lv_obj_set_size(key, 60, 26);
        lv_obj_align(key, i ? LV_ALIGN_BOTTOM_RIGHT : LV_ALIGN_BOTTOM_LEFT, i ? -4 : 4, -4);
        lv_obj_t *t = lv_label_create(key);
        lv_label_set_text(t, i ? "Pair" : "Menu");
        lv_obj_set_style_text_font(t, &lv_font_montserrat_14, 0);
        lv_obj_center(t);
    }
    lvgl_port_unlock();
    // The backlight: LDO2's voltage, 2.8 V as the factory firmware left it.
    vTaskDelay(pdMS_TO_TICKS(1000));
    heap("with the status screen drawn");
    for (int i = 0; i < 5; i++) {
        lvgl_port_lock(0);
        lv_label_set_text_fmt(counts, LV_SYMBOL_DOWNLOAD " %d   " LV_SYMBOL_UPLOAD " %d", 128 + i, 17 + i);
        lvgl_port_unlock();
        vTaskDelay(pdMS_TO_TICKS(500));
    }
    heap("after five redraws");
}
#endif

void lcd_test()
{
    static const struct {
        const char *name;
        uint16_t c;
    } fills[] = {{"red", rgb(255, 0, 0)}, {"green", rgb(0, 255, 0)}, {"blue", rgb(0, 0, 255)},
                 {"white", rgb(255, 255, 255)}};
    for (auto &f : fills) {
        ESP_LOGI(TAG, "screen: all %s", f.name);
        fill(f.c);
        show();
        vTaskDelay(pdMS_TO_TICKS(1500));
    }
    // A 1-pixel white border shows the offsets; red top left, green top
    // right, blue bottom left show the orientation and the colour order.
    fill(rgb(0, 0, 0));
    rect(0, 0, W, 1, rgb(255, 255, 255));
    rect(0, H - 1, W, 1, rgb(255, 255, 255));
    rect(0, 0, 1, H, rgb(255, 255, 255));
    rect(W - 1, 0, 1, H, rgb(255, 255, 255));
    rect(4, 4, 30, 30, rgb(255, 0, 0));
    rect(W - 34, 4, 30, 30, rgb(0, 255, 0));
    rect(4, H - 34, 30, 30, rgb(0, 0, 255));
    show();
    ESP_LOGI(TAG, "screen: white border; red top left, green top right, blue bottom left");
}

// ── LED and buzzer ──────────────────────────────────────────────────────

void led(bool on) { gpio_set_level(LED, on ? 0 : 1); } // active low

void tone(uint32_t hz, uint32_t ms)
{
    ledc_set_freq(LEDC_LOW_SPEED_MODE, LEDC_TIMER_0, hz);
    ledc_set_duty(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_0, 512);
    ledc_update_duty(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_0);
    vTaskDelay(pdMS_TO_TICKS(ms));
    ledc_set_duty(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_0, 0);
    ledc_update_duty(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_0);
}

void io_init()
{
    gpio_config_t out = {};
    out.pin_bit_mask = 1ULL << LED;
    out.mode = GPIO_MODE_OUTPUT;
    gpio_config(&out);
    led(false);

    gpio_config_t in = {};
    in.pin_bit_mask = (1ULL << BUTTON_A) | (1ULL << BUTTON_B);
    in.mode = GPIO_MODE_INPUT; // input-only pins: the board has the pull-ups
    gpio_config(&in);

    ledc_timer_config_t t = {};
    t.speed_mode = LEDC_LOW_SPEED_MODE;
    t.duty_resolution = LEDC_TIMER_10_BIT;
    t.timer_num = LEDC_TIMER_0;
    t.freq_hz = 2000;
    t.clk_cfg = LEDC_AUTO_CLK;
    ESP_ERROR_CHECK(ledc_timer_config(&t));
    ledc_channel_config_t c = {};
    c.gpio_num = BUZZER;
    c.speed_mode = LEDC_LOW_SPEED_MODE;
    c.channel = LEDC_CHANNEL_0;
    c.timer_sel = LEDC_TIMER_0;
    c.duty = 0;
    ESP_ERROR_CHECK(ledc_channel_config(&c));
}

void battery()
{
    uint8_t b[2];
    float v = 0, chg = 0, dis = 0;
    if (axp_read(0x78, b, 2)) {
        v = ((b[0] << 4) | (b[1] & 0x0f)) * 1.1f / 1000.0f;
    }
    if (axp_read(0x7a, b, 2)) {
        chg = ((b[0] << 5) | (b[1] & 0x1f)) * 0.5f;
    }
    if (axp_read(0x7c, b, 2)) {
        dis = ((b[0] << 5) | (b[1] & 0x1f)) * 0.5f;
    }
    ESP_LOGI(TAG, "battery %.3f V, charging %.1f mA, discharging %.1f mA; power 0x%02x, charge 0x%02x", v, chg,
             dis, axp_read8(0x00), axp_read8(0x01));
}

} // namespace

extern "C" void app_main()
{
    heap("at start");
    i2c_scan();
    if (axp) {
        axp_dump();
        axp_screen_on();
    }
    io_init();
    lcd_init();
    heap("after the screen (with a 64 KB frame)");
#if CONFIG_SPIKE_LVGL
    lvgl_status();
#else
    if (frame) {
        lcd_test();
    }
#endif
    ESP_LOGI(TAG, "LED: three blinks");
    for (int i = 0; i < 3; i++) {
        led(true);
        vTaskDelay(pdMS_TO_TICKS(300));
        led(false);
        vTaskDelay(pdMS_TO_TICKS(300));
    }
    ESP_LOGI(TAG, "buzzer: 880, 1760, 3520 Hz");
    tone(880, 200);
    tone(1760, 200);
    tone(3520, 200);
    ESP_LOGI(TAG, "ready: press A (LED on while held), B (a tone), the power button");

    bool a_was = false, b_was = false;
    for (int tick = 0;; tick++) {
        bool a = gpio_get_level(BUTTON_A) == 0;
        bool bb = gpio_get_level(BUTTON_B) == 0;
        if (a != a_was) {
            ESP_LOGI(TAG, "button A %s", a ? "down" : "up");
            led(a);
            a_was = a;
        }
        if (bb != b_was) {
            ESP_LOGI(TAG, "button B %s", bb ? "down" : "up");
            if (bb) {
                ESP_LOGI(TAG, "buzzer: 2000, 2700, 4000 Hz, 0.4 s each");
                tone(2000, 400);
                tone(2700, 400);
                tone(4000, 400);
            }
            b_was = bb;
        }
        uint8_t irq = axp_read8(0x46);
        if (irq != 0xee && (irq & 0x03)) {
            axp_write8(0x46, irq & 0x03);
            ESP_LOGI(TAG, "power button: %s press", (irq & 0x02) ? "short" : "long");
        }
        if (tick % 20 == 0) {
            ESP_LOGI(TAG, "levels: GPIO37 %d, GPIO39 %d (0 is pressed)", gpio_get_level(BUTTON_A),
                     gpio_get_level(BUTTON_B));
        }
        if (tick % 200 == 0) {
            battery();
        }
        vTaskDelay(pdMS_TO_TICKS(50));
    }
}
