#include "console.h"

#include <cstdarg>
#include <cstdio>
#include <memory>

#include "driver/uart.h"
#include "driver/uart_vfs.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

namespace ktb::console {

namespace {

constexpr uart_port_t port = static_cast<uart_port_t>(CONFIG_ESP_CONSOLE_UART_NUM);

// Held while a whole line is written, by the log and by write_line.
SemaphoreHandle_t lock;
// The log formats into this, under the lock, so it costs no task stack.
char log_buf[512];

int log_vprintf(const char *fmt, va_list args)
{
    xSemaphoreTake(lock, portMAX_DELAY);
    int n = vsnprintf(log_buf, sizeof(log_buf), fmt, args);
    if (n > 0) {
        size_t len = static_cast<size_t>(n) < sizeof(log_buf) ? n : sizeof(log_buf) - 1;
        if (static_cast<size_t>(n) >= sizeof(log_buf)) {
            log_buf[len - 1] = '\n'; // truncated: keep the line ending
        }
        uart_write_bytes(port, log_buf, len);
    }
    xSemaphoreGive(lock);
    return n;
}

struct ReaderArgs {
    LineHandler handler;
};

void reader_task(void *arg)
{
    LineHandler handler = static_cast<ReaderArgs *>(arg)->handler;
    delete static_cast<ReaderArgs *>(arg);

    std::unique_ptr<char[]> line(new char[max_message + 1]);
    size_t len = 0;
    bool overflow = false;
    uint8_t chunk[128];

    for (;;) {
        int n = uart_read_bytes(port, chunk, sizeof(chunk), pdMS_TO_TICKS(100));
        for (int i = 0; i < n; i++) {
            char c = static_cast<char>(chunk[i]);
            if (c == '\n') {
                if (overflow) {
                    handler(nullptr, 0);
                } else {
                    if (len > 0 && line[len - 1] == '\r') {
                        len--;
                    }
                    line[len] = '\0';
                    if (len > 0) {
                        handler(line.get(), len);
                    }
                }
                len = 0;
                overflow = false;
            } else if (overflow) {
                // Skip the rest of a line that's too long.
            } else if (len < max_message) {
                line[len++] = c;
            } else {
                overflow = true;
            }
        }
    }
}

} // namespace

void start()
{
    lock = xSemaphoreCreateMutex();
    ESP_ERROR_CHECK(uart_driver_install(port, 2048, 4096, 0, nullptr, 0));
    // printf and friends go through the driver too, rather than around it.
    uart_vfs_dev_use_driver(port);
    esp_log_set_vprintf(log_vprintf);
}

void write_line(const char *text, size_t len)
{
    xSemaphoreTake(lock, portMAX_DELAY);
    uart_write_bytes(port, text, len);
    uart_write_bytes(port, "\n", 1);
    xSemaphoreGive(lock);
}

void serve(LineHandler handler)
{
    xTaskCreate(reader_task, "console", 6144, new ReaderArgs{handler}, 5, nullptr);
}

void flush()
{
    uart_wait_tx_done(port, pdMS_TO_TICKS(500));
}

} // namespace ktb::console
