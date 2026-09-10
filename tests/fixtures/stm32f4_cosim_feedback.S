.syntax unified
.cpu cortex-m4
.thumb

.section .isr_vector,"a",%progbits
.global __vector_table
__vector_table:
  .word 0x20020000
  .word Reset_Handler

.section .text.Reset_Handler,"ax",%progbits
.thumb_func
.global Reset_Handler
.type Reset_Handler,%function
Reset_Handler:
  bl main
1:
  b 1b
.size Reset_Handler, .-Reset_Handler
