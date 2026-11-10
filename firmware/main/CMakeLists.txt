idf_component_register(
    SRCS "main.cpp" "console.cpp" "control.cpp" "settings.cpp" "auth.cpp"
         "firmware.cpp" "led.cpp" "ui.cpp" "radio.cpp" "storage.cpp"
         "control_radio.cpp" "kiss.cpp" "phones.cpp" "activity.cpp"
    INCLUDE_DIRS "."
    PRIV_REQUIRES nvs_flash esp_driver_gpio esp_driver_rmt esp_driver_uart
                  esp_timer app_update esp_app_format bootloader_support mbedtls bt
)

# When this build was configured, for device.info. Release builds configure
# from scratch, so this is the release's build time.
string(TIMESTAMP KTB_BUILT "%Y-%m-%dT%H:%M:%SZ" UTC)
set_source_files_properties(firmware.cpp PROPERTIES COMPILE_DEFINITIONS "KTB_BUILT=\"${KTB_BUILT}\"")
