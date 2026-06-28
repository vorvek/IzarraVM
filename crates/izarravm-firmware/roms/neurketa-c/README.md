# Dhrystone 2.1 benchmark (.EXE)

`dhrystone.exe` is the Dhrystone 2.1 integer benchmark by Reinhold P. Weicker
(public domain), adapted to run freestanding under the IzarraVM harness. The
adaptation removes every host service: no malloc (the two records are static),
no printf or scanf, no timing. The measurement loop and every procedure are the
original, so the distribution statistics still hold.

## It is a .EXE, not a .COM

The first attempt built this as a flat .COM. That mis-relocates the global
variables: Open Watcom places the C globals in DGROUP at offsets the loader can
only satisfy through MZ relocations, which a .COM has none of, so data writes
land in the code segment and corrupt it. Built as a small-model .EXE the loader
applies the relocations, DGROUP is a real data segment, and the globals resolve
where the code expects them. `cstart.asm` declares the standard Open Watcom
small-model segments so DGROUP merges with the C objects, points DS=ES at DGROUP
at entry, and relies on the linked STACK segment for SS:SP.

Load it with `Machine::new_dos_program`. Read the result back with
`Machine::bench_iterations` and `Machine::bench_aux`.

## Run count and self-check

`Number_Of_Runs` is fixed at 10000. The iteration count is reported back through
the device as a 16-bit low word, so it must stay below 65536; 10000 gives a
stable run with a reasonable wall time and keeps the loop well clear of being
trivially short.

Instead of printing a "should be" report, the driver folds the documented final
values to a 16-bit checksum and reports it as the aux value, so a wrong run
yields a wrong checksum. For this build at `Number_Of_Runs=10000` the observed
checksum is **10214 (0x27E6)**. That value is the source of truth and is pinned
by the end-to-end test.

## Files

- `dhry.h`, `dhry_1.c`, `dhry_2.c` - the adapted public-domain Dhrystone 2.1.
- `cstart.asm` - the freestanding .EXE startup and the strcpy/strcmp/report
  helpers the measurement loop calls.
- `dhrystone.exe` - the checked-in linked benchmark.

## Rebuild

From this directory, with Open Watcom at `D:/DevTools/OpenWatcom`:

    export WATCOM="D:/DevTools/OpenWatcom"
    export INCLUDE="D:/DevTools/OpenWatcom/h"
    "D:/DevTools/OpenWatcom/binnt/wasm.exe" cstart.asm
    "D:/DevTools/OpenWatcom/binnt/wcc.exe" -ms -0 -s -zl -ot dhry_1.c
    "D:/DevTools/OpenWatcom/binnt/wcc.exe" -ms -0 -s -zl -ot dhry_2.c
    "D:/DevTools/OpenWatcom/binnt/wlink.exe" format dos option start=cstart_, quiet \
        name dhrystone.exe file cstart.obj file dhry_1.obj file dhry_2.obj

The link must report no undefined symbols and no warnings; the STACK segment
removes the stack warning. The output starts with the bytes `4D 5A` ("MZ").
Delete the `cstart.obj`, `dhry_1.obj`, and `dhry_2.obj` intermediates afterward.
The `wcc` "no prototype" and "missing return value" warnings are inherent to the
original K&R-style source and are expected.
