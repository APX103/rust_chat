// Arduino UNO Q LED Matrix RPC Server
// Flash this sketch to the STM32 MCU via Arduino IDE / App Lab
// Provides: matrix_draw(pattern), matrix_clear(), matrix_set_grayscale(bits)

#include <Arduino_LED_Matrix.h>
#include "Arduino_RouterBridge.h"

Arduino_LED_Matrix matrix;
uint8_t buffer[104];  // 8 rows x 13 cols

// pattern: 104 characters, each is '0' or '1' (row-major, 8 rows x 13 cols)
void matrix_draw(const char* bits) {
    for (int i = 0; i < 104 && bits[i]; i++) {
        buffer[i] = (bits[i] == '1') ? 255 : 0;
    }
    matrix.draw(buffer);
}

void matrix_clear() {
    memset(buffer, 0, 104);
    matrix.draw(buffer);
}

void matrix_set_grayscale(int bits) {
    matrix.setGrayscaleBits(bits);
}

void setup() {
    matrix.begin();
    matrix.setGrayscaleBits(1);
    memset(buffer, 0, 104);

    Bridge.begin();
    Bridge.provide_safe("matrix_draw", matrix_draw);
    Bridge.provide_safe("matrix_clear", matrix_clear);
    Bridge.provide_safe("matrix_set_grayscale", matrix_set_grayscale);
}

void loop() {
    // Nothing — all work done via RPC
}
