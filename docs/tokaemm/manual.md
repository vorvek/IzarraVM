<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# TOKAEMM.SYS: Memory Manager

TOKAEMM.SYS is the General Simulation Works memory manager for the Izarra
3000. A single driver provides extended memory (XMS), expanded memory (EMS),
upper memory blocks (UMBs), and the high memory area (HMA), in the manner of
EMM386.EXE on a 386-or-better personal computer.

## Loading it

`CONFIG.SYS` loads TOKAEMM as a device driver, before `DOS=HIGH,UMB`:

```
DEVICE=C:\DOS\TOKAEMM.SYS [RAM | NOEMS] [/T]
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
| *(none)*, or `RAM` | Provides XMS, UMBs, HMA, and EMS 4.0 with its page frame at segment `E000`. This is the shipped default. |
| `NOEMS` | Keep XMS, UMBs, and HMA, but disable the EMS page frame and page pool. `INT 67h` still reports the manager as present with zero EMS pages. |
| `/T` | Prefix the signon banner with the tree-styled connector used by the Toka-DOS boot screen. Off by default; combine freely with `RAM` or `NOEMS` in any order. |

### How extended memory is divided

It is not divided. XMS blocks, VCPI pages, and EMS pages are all drawn from a
single pool as they are requested, and memory returned by one interface becomes
available to the others. There is no fixed EMS partition and no reserved XMS
area.

On a 64 MiB Izarra 3000, MEM reports 640K of conventional memory, a 384K upper
region, and a 64,512K extended category, of which 64,125K is free after Toka-DOS
and its drivers have loaded.

**Note:** Because expanded memory is taken from the same pool, MEM does not
print a separate `Expanded (EMS)` line. The `Extended (XMS)` line is marked with
an asterisk and a footnote stating that EMS is simulated as required. This is
the same reporting convention MS-DOS 6.22 uses with EMM386.

## Resident footprint

TOKAEMM occupies 23K of conventional memory for its code, its state, its task
structure, and its monitor stack. The tables that track the shared arena — the
allocation bitmap, the VCPI ownership bitmap, and the EMS page chain — are not
among them. They live in a system window above the 1 MB line, reachable only by
the manager's own ring-0 monitor and mapped in no client's address space.

Keeping them there matters for more than the 18K it returns. Their size is
proportional to installed memory, about 288 bytes per megabyte between them, so
in conventional memory they grew with every machine size; past roughly 148 MB
they could not be fitted at all. Held outside it, the resident figure above is
the same on a 64 MB machine and a 512 MB one.

The page directory and page tables require a further 68K, also reserved above
the 1 MB line on a machine with sufficient extended memory. The standard 64 MiB
configuration therefore leaves 598K of conventional memory free.

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
  free space (08h-0Fh), through a 32-handle allocator. Blocks are allocated in
  1 KB units from the shared pool described above, so the largest block
  obtainable depends on what EMS and VCPI have taken, and function 08h reports
  the largest free block separately from the total.
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

### The upper memory area

The 384K region from `A000` through `FFFF` is reserved for adapters and system
ROM. TOKAEMM converts the part of it that no device occupies into upper memory
blocks, backed by real extended memory mapped in over the address hole.

| Address range | Size | Contents |
| --- | --- | --- |
| `A000-AFFF` | 64K | VGA graphics buffer |
| `B000-B7FF` | 32K | Monochrome text and Hercules graphics |
| `B800-BFFF` | 32K | VGA text buffer |
| `C000-C7FF` | 32K | Video BIOS |
| `C800-DFFF` | 96K | Upper memory blocks |
| `E000-EFFF` | 64K | EMS page frame, or upper memory blocks under `NOEMS` |
| `F000-FFFF` | 64K | System BIOS |

With the default EMS page frame at `E000`, TOKAEMM allocates 96K of upper
memory from `C800` through `DFFF`. `NOEMS` removes the frame and raises the
allocatable upper memory to 160K, from `C800` through `EFFF`.

**Note:** The 32K at `B000-B7FF` is not converted to upper memory, although
nothing occupies it while the display is in a colour mode. The video adapter
decodes that range whenever a program selects monochrome text (`INT 10h` mode
07h, as `MODE MONO` does), switches the adapter to its Hercules personality, or
programs the graphics controller for a 128K aperture. A block loaded there would
be overwritten without warning the moment any of those occurred, and the display
would then be driven from memory the program no longer owns. Memory managers on
other machines offer this range through an `I=B000-B7FF` switch; TOKAEMM does
not, because on the Izarra 3000 the same adapter serves both purposes.

## EMS 4.0 (the RAM keyword)

`DEVICE=C:\DOS\TOKAEMM.SYS RAM` turns on Lotus/Intel/Microsoft Expanded Memory
Specification 4.0 support: a 64 KB page frame at segment `E000`, mapped in
16 KB pages drawn from the shared pool as programs request them. Software
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
