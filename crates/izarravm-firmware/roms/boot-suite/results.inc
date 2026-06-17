%define RESULT_RECORD_COUNT 22
%define RESULT_PAYLOAD_LEN 473
%define RESULT_PAYLOAD_CHECKSUM 0xa346

result_block_template:
    db 'VDTS'
    dw 1
    dw RESULT_RECORD_COUNT
    dw RESULT_PAYLOAD_LEN
    dw RESULT_PAYLOAD_CHECKSUM
result_payload:
    db 'BEGIN suite.boot', 10
    db 'PASS boot.sector', 10
    db 'PASS boot.stage2', 10
    db 'PASS report.serial', 10
    db 'PASS report.result_block', 10
    db 'PASS cpu.real_mode', 10
    db 'PASS cpu.protected_mode', 10
    db 'PASS cpu.paging', 10
    db 'PASS memory.conventional_pattern', 10
    db 'MEASURE memory.conventional_kib 640', 10
    db 'PASS video.vga_text', 10
    db 'FAIL video.cga_graphics', 10
    db 'FAIL video.ega_planar', 10
    db 'PASS video.vga_mode13h', 10
    db 'FAIL sound.pc_speaker', 10
    db 'FAIL sound.opl2', 10
opl3_record:
    db 'FAIL sound.opl3', 10
sb_reset_record:
    db 'FAIL sound.sb_dsp_reset', 10
    db 'FAIL sound.sb_8bit_dma', 10
    db 'FAIL sound.sb_16bit_dma', 10
    db 'FAIL sound.sb_adpcm', 10
timer_record:
    db 'FAIL timer.irq0', 10
result_payload_end:
    db 0
result_block_end:
