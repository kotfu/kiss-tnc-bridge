idf_component_register(
    SRCS "main.cpp" "radio.cpp" "ble.cpp"
    INCLUDE_DIRS "."
    PRIV_REQUIRES bt nvs_flash esp_timer esp_driver_gpio
)
