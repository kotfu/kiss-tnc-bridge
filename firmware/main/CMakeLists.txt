# The UI is the board's: the LED and the button on the ATOM Lite (and, for
# now, the StickC Plus SE), the touch screen on the Core2 (board.h).
set(srcs "main.cpp" "console.cpp" "control.cpp" "settings.cpp" "auth.cpp"
         "firmware.cpp" "radio.cpp" "storage.cpp" "control_radio.cpp" "kiss.cpp"
         "phones.cpp" "activity.cpp" "errors.cpp" "power.cpp")
if(CONFIG_KTB_BOARD_CORE2)
    list(APPEND srcs "core2_ui.cpp" "core2_hw.cpp" "axp192.cpp")
else()
    list(APPEND srcs "ui.cpp" "led.cpp")
endif()

idf_component_register(
    SRCS ${srcs}
    INCLUDE_DIRS "."
    PRIV_REQUIRES nvs_flash esp_driver_gpio esp_driver_rmt esp_driver_uart esp_driver_i2c
                  esp_driver_spi esp_lcd esp_timer app_update esp_app_format bootloader_support
                  mbedtls bt
)

# When this build was configured, for device.info. Release builds configure
# from scratch, so this is the release's build time.
string(TIMESTAMP KTB_BUILT "%Y-%m-%dT%H:%M:%SZ" UTC)
set_source_files_properties(firmware.cpp PROPERTIES COMPILE_DEFINITIONS "KTB_BUILT=\"${KTB_BUILT}\"")
