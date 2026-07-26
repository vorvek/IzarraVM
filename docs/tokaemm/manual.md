<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# TOKAEMM.SYS: Memory Manager

TOKAEMM.SYS is General Simulation Works's memory manager for the Izarra
3000: one driver providing extended memory (XMS), expanded memory (EMS),
and upper memory blocks (UMBs), in the tradition of EMM386.EXE on a real
386-or-better PC. This is its manual.

## Loading it

`CONFIG.SYS` loads TOKAEMM as a device driver, before `DOS=HIGH,UMB`:

```
DEVICE=C:\DOS\TOKAEMM.SYS [RAM | NOEMS]
```

The Toka-DOS default ships as:

```
DEVICE=C:\DOS\TOKAEMM.SYS RAM
DOS=HIGH,UMB
```

## Switches

TOKAEMM starts with EMS enabled when the `DEVICE=` line has no argument.
`RAM` selects that same default explicitly. Use `NOEMS` when a program needs
the manager to run without an EMS page frame. There is no `FRAME=` switch or
memory-size argument.

| Argument | Effect |
| --- | --- |
| *(none)*, or `RAM` | Provide XMS, UMBs, HMA, and a 3 MiB EMS 4.0 pool with its page frame at segment `E000`. This is the shipped default. |
| `NOEMS` | Keep XMS, UMBs, and HMA, but disable the EMS page frame and page pool. `INT 67h` still reports the manager as present with zero EMS pages. |

On the Izarra 3000's 24 MiB memory map, Toka-DOS reports 640 KiB of
conventional memory, a 384 KiB upper region, a 20 MiB extended-memory
category, and a separate 3 MiB EMS partition at the top of RAM. The first
2 MiB of allocatable extended memory is reserved for XMS blocks. VCPI owns the
rest. Under `NOEMS`, the EMS category is zero and those 3 MiB are added to the
VCPI pool, making the extended-memory category 23 MiB.

The Toka-DOS MEM command combines free XMS and VCPI memory in its existing
`Extended (XMS)` row. MEM and TOKAEMM are shipped as a matched pair. An older
MEM used with this driver is safe, but it may show free VCPI pages as used.

## Resident footprint

TOKAEMM keeps about 23 KiB of code, state, its task structure, and its monitor
stack in conventional memory. On machines with enough extended RAM, its seven
page-directory and page-table pages use 28 KiB of reserved space at the start
of the XMS category instead. The standard 24 MiB boot therefore leaves about
600 KiB of conventional memory free.

The 1 MiB profile has no extended pages to reserve, so TOKAEMM keeps a low
page-table fallback there. Loading the whole manager into a UMB would not help
the normal configuration: TOKAEMM creates the UMB service during its own
initialization, and the default EMS frame leaves only 96 KiB of allocatable
UMB space. Keeping the paging pages in extended RAM avoids that bootstrapping
problem and leaves the UMBs available for drivers and environments.

## XMS

TOKAEMM installs as the extended memory manager via the standard `INT 2Fh`
hook, and answers as **XMS 3.0**. It implements the core function set DOS
and drivers rely on:

- **HMA**: request and release the high memory area (functions 01h/02h),
  which lets `DOS=HIGH` relocate the kernel there.
- **A20 control**: global and local enable/disable, with nesting, plus a
  query function (03h-07h).
- **Extended memory blocks**: allocate, free, resize, lock/unlock, and query
  free space (08h-0Fh), through a 32-handle allocator over the dedicated XMS
  pool. A normal Izarra 3000 provides up to 2 MiB for these blocks.
- **Block moves**: the `INT 21h`-style bulk-copy function (0Bh) that moves
  data between conventional and extended memory, or between two extended
  blocks.
- **UMB functions**: request, release, and reallocate upper memory blocks
  (10h-12h), which is what `DOS=UMB` and `LOADHIGH`/`DEVICEHIGH` actually
  call.

## Upper memory blocks (DOS=UMB)

With `DOS=UMB` in `CONFIG.SYS`, the Toka-DOS kernel claims upper memory blocks
from TOKAEMM through the XMS UMB functions above, and `DEVICEHIGH=`/`LH`
lines (including `LH TOKAMOUS` in the stock `AUTOEXEC.BAT`) load into them
instead of conventional memory whenever one is free. TOKAEMM backs this
upper memory window with real extended RAM mapped in over the address hole
above the video BIOS, so loading high genuinely frees conventional memory
rather than faking it.

The full upper memory region from `A0000` through `FFFFF` is 384 KiB. Video
memory and ROMs occupy part of it. With the default EMS frame at `E000`,
TOKAEMM can allocate 96 KiB of UMB space from `C800` through `DFFF`. `NOEMS`
removes the frame and raises the allocatable UMB space to 160 KiB, from
`C800` through `EFFF`.

## EMS 4.0 (the RAM keyword)

`DEVICE=C:\DOS\TOKAEMM.SYS RAM` turns on Lotus/Intel/Microsoft Expanded Memory
Specification 4.0 support: a 64 KB page frame at segment `E000`, mapped in
16 KB pages backed by a separate 3 MiB partition at the top of RAM. Software
that checks for EMS the standard way (looking for the `EMMXXXX0` device name)
finds it, and `INT 67h` answers the LIM 4.0 function set: status, frame address,
page counts, allocate/map/free/save/restore, and version.

The shipped Toka-DOS `CONFIG.SYS` writes `RAM` explicitly. A bare `DEVICE=`
line behaves the same way. Replace `RAM` with `NOEMS` to turn off EMS and make
the page-frame area available for UMB allocation.

## HMA and DOS=HIGH

`DOS=HIGH,UMB` in `CONFIG.SYS` does two things together: it asks TOKAEMM for
the HMA (moving most of the resident kernel above the 1 MB line, out of
conventional memory) and enables the UMB loading described above. Both
depend on TOKAEMM already being loaded as a device driver, which is why
`DEVICE=C:\DOS\TOKAEMM.SYS` comes before the `DOS=` line.

## A20

The A20 gate (the line that decides whether memory addressing wraps at
1 MB, the way the original 8086 did, or continues into extended memory)
is under TOKAEMM's control the normal way: through `INT 21h`/`XMS`
local and global enable/disable calls, and through the classic keyboard
controller port `0x92` that DOS and period software poke directly. From
outside the machine, A20 behaves exactly as real EMM386-managed DOS expects:
off gives you the 8086 wraparound, on gives you the full address range.

## The V86 monitor

Right after Toka-DOS finishes booting, TOKAEMM switches the whole running
system into the CPU's Virtual-8086 mode and installs itself underneath it
as a small supervisor. From that point on, the kernel, the shell, and every
program you run all execute inside this virtualized real-mode environment
rather than directly on the bare CPU. Whenever DOS or an application does
something normally reserved for privileged code (masking interrupts,
reading or writing the flags register, raising or returning from a software
interrupt, or touching the A20 gate), the CPU hands control to TOKAEMM's
monitor instead of faulting, and the monitor carries out the request
faithfully before handing control back, invisibly to the software involved.
This is also the mechanism TOKAEMM uses internally: its XMS block-move and
EMS page-remap operations both make a brief privileged call into the same
monitor to do the raw memory work safely. The net effect is that Toka-DOS
looks and behaves like an ordinary real-mode system throughout, while
TOKAEMM gets the low-level access it needs to provide XMS, EMS, and UMBs
above the classic 1 MB ceiling.

## Next

- [Using Toka-DOS](../toka-dos/using-toka-dos.md): the shell TOKAEMM boots
  underneath.
- [DOS command reference](../toka-dos/commands.md): `GSWMODE`, `TOKAMOUS`,
  and the rest of the shipped tools.
