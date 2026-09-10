#include <stdint.h>

#define RCC_AHB1ENR (*(volatile uint32_t *)0x40023830u)
#define GPIOD_MODER (*(volatile uint32_t *)0x40020C00u)
#define GPIOD_IDR   (*(volatile uint32_t *)0x40020C10u)
#define GPIOD_BSRR  (*(volatile uint32_t *)0x40020C18u)

__attribute__((noreturn)) void main(void) {
    RCC_AHB1ENR |= (1u << 3);
    GPIOD_MODER &= ~((3u << (12u * 2u)) | (3u << (13u * 2u)));
    GPIOD_MODER |= (1u << (12u * 2u));

    for (;;) {
        if ((GPIOD_IDR & (1u << 13)) != 0u) {
            GPIOD_BSRR = (1u << (12u + 16u));
        } else {
            GPIOD_BSRR = (1u << 12u);
        }
    }
}
