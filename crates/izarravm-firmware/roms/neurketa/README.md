# Neurketa benchmark harness image

- `boot.asm` is a 512-byte real-mode boot sector. It loads stage 2 with INT 13h.
- `neurketa-stage2.asm` is the freestanding runtime loaded at `0000:8000`. It
  reads the host-preloaded selector byte from the Lotura unit-tester register
  file (port 0xE4 index 16), runs the selected payload, writes the result
  primitives back, and issues CMD_EXIT.
- `neurketa.img` is the checked-in 1.44 MiB boot image.

Payloads: selector 0 is the empty baseline (boot and report overhead only),
selector 1 is the BYTE Sieve (8190 flags, 40 passes, 1899 primes per pass). The
host subtracts the baseline cycles from the Sieve cycles to isolate the work.

Rebuild:

    nasm -f bin -I . boot.asm -o boot.bin
    nasm -f bin -I . neurketa-stage2.asm -o stage2.bin

then concatenate boot.bin (offset 0) and stage2.bin (offset 512) into a
1,474,560-byte neurketa.img (see the plan for the PowerShell step).
