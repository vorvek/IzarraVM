<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# TOKAEMM.SYS: Memory Manager

TOKAEMM.SYS is the General Simulation Works memory manager for the Izarra
3000. One driver supplies extended memory (XMS), expanded memory (EMS), upper
memory blocks (UMBs), and the high memory area (HMA). It does this in the same
way as EMM386.EXE on a personal computer with a 386 processor or a later
processor.

## How to load it

`CONFIG.SYS` loads TOKAEMM as a device driver, before `DOS=HIGH,UMB`:

```
DEVICE=C:\DOS\TOKAEMM.SYS [RAM | NOEMS] [/T]
```

The Toka-DOS default is:

```
DEVICE=C:\DOS\TOKAEMM.SYS RAM
DOS=HIGH,UMB
```

## Switches

TOKAEMM starts with EMS enabled when the `DEVICE=` line has no argument. `RAM`
selects the same default directly. Use `NOEMS` when a program needs the
manager without an EMS page frame. There is no `FRAME=` switch, and there is
no memory-size argument.

| Argument | Effect |
| --- | --- |
| *(none)*, or `RAM` | Supplies XMS, UMBs, HMA, and EMS 4.0, with the page frame at segment `E000`. This is the default. |
| `NOEMS` | Keeps XMS, UMBs, and HMA, but disables the EMS page frame and the page pool. `INT 67h` continues to report the manager as present, with zero EMS pages. |
| `/T` | Puts the tree connector of the Toka-DOS boot screen in front of the sign-on banner. It is off by default. You can use it with `RAM` or `NOEMS`, in any order. |

### The division of extended memory

TOKAEMM does not divide extended memory. XMS blocks, VCPI pages, and EMS pages
all come from one pool, as programs request them. Memory that one interface
releases becomes available to the others. There is no fixed EMS partition, and
there is no reserved XMS area.

On a 64 MiB Izarra3000, MEM reports 640K of conventional memory, a 384K upper
region, and 64,512K of extended memory. After Toka-DOS and its drivers load,
64,125K of the extended memory is free.

**Note:** Expanded memory comes from the same pool. Thus MEM does not print a
separate `Expanded (EMS)` line. The `Extended (XMS)` line has an asterisk and
a footnote. The footnote says that the manager simulates EMS as necessary.
MS-DOS 6.22 with EMM386 reports memory in the same way.

## Resident footprint

TOKAEMM uses 23K of conventional memory. This holds its code, its state, its
task structure, and its monitor stack. The tables for the shared pool are not
in conventional memory. These tables are the allocation bitmap, the VCPI
ownership bitmap, and the EMS page chain. They are in a system window above
the 1 MB line. Only the ring-0 monitor of the manager can read them, and they
are in no client address space.

This position gives back 18K of conventional memory. It also removes a limit
on the size. The total size of the tables is proportional to the installed
memory, at approximately 288 bytes for each megabyte. In conventional memory,
the tables thus increased with the size of the machine, and above
approximately 148 MB they did not fit. Above the 1 MB line, the resident size
is the same on a 64 MB machine and on a 512 MB machine.

The page directory and the page tables need a further 68K. On a machine with
sufficient extended memory, these are also above the 1 MB line. The standard
64 MiB configuration thus leaves 598K of conventional memory free.

The 1 MiB profile has no extended pages, so TOKAEMM keeps the page tables low
on that profile. A load of the full manager into a UMB does not help the
normal configuration. TOKAEMM makes the UMB service during its own
initialization, and the default EMS frame leaves only 96 KiB of UMB space.
Paging pages in extended RAM prevent that sequence problem, and they keep the
UMBs available for drivers and environments.

## XMS

TOKAEMM installs as the extended memory manager through the standard
`INT 2Fh` hook. It answers as **XMS 3.0**. It supplies the core functions that
DOS and the drivers need:

- **HMA**: request and release the high memory area (functions 01h and 02h).
  `DOS=HIGH` uses this to move the kernel there.
- **A20 control**: global and local enable and disable, with nesting, and a
  query function (03h to 07h).
- **Extended memory blocks**: allocate, free, resize, lock, unlock, and query
  the free space (08h to 0Fh), through an allocator with 32 handles. The
  blocks come from the shared pool above, in units of 1 KB. Thus the largest
  available block depends on the memory that EMS and VCPI hold. Function 08h
  reports the largest free block and the total separately.
- **Block moves**: the bulk-copy function (0Bh). It moves data between
  conventional memory and extended memory, or between two extended blocks.
- **UMB functions**: request, release, and reallocate upper memory blocks
  (10h to 12h). `DOS=UMB`, `LOADHIGH`, and `DEVICEHIGH` call them.

## Upper memory blocks (DOS=UMB)

With `DOS=UMB` in `CONFIG.SYS`, the Toka-DOS kernel takes upper memory blocks
from TOKAEMM. It uses the XMS UMB functions above. A `DEVICEHIGH=` line or an
`LH` line then loads into a block when one is free. `LH TOKAMOUS` in the
default `AUTOEXEC.BAT` is an example. For this window, TOKAEMM maps real
extended RAM into the address hole above the video BIOS. Thus a high load
frees conventional memory.

### The upper memory area

The 384K region from `A000` to `FFFF` is reserved for the adapters and the
system ROM. TOKAEMM makes upper memory blocks from the part of the region that
no device uses. Real extended memory, mapped into the address hole, holds
these blocks.

| Address range | Size | Contents |
| --- | --- | --- |
| `A000-AFFF` | 64K | VGA graphics buffer |
| `B000-B7FF` | 32K | Monochrome text and Hercules graphics |
| `B800-BFFF` | 32K | VGA text buffer |
| `C000-C7FF` | 32K | Video BIOS |
| `C800-DFFF` | 96K | Upper memory blocks |
| `E000-EFFF` | 64K | EMS page frame, or upper memory blocks under `NOEMS` |
| `F000-FFFF` | 64K | System BIOS |

With the default EMS page frame at `E000`, TOKAEMM makes 96K of upper memory,
from `C800` to `DFFF`. `NOEMS` removes the frame and increases the upper
memory to 160K, from `C800` to `EFFF`.

**Note:** TOKAEMM does not make upper memory from the 32K at `B000-B7FF`.
Nothing uses that range while the display is in a color mode. But the video
adapter decodes the range in three conditions: a program selects monochrome
text (`INT 10h` mode 07h, as `MODE MONO` does), a program selects the Hercules
mode of the adapter, or a program sets the graphics controller to a 128K
aperture. In each of these conditions, the adapter writes over a block at that
address, with no warning. The display then reads memory that the program no
longer owns.

Memory managers on other machines make this range available with an
`I=B000-B7FF` switch. TOKAEMM does not, because on the Izarra3000 one adapter
does both functions.

## EMS 4.0 (the RAM keyword)

`DEVICE=C:\DOS\TOKAEMM.SYS RAM` enables Lotus/Intel/Microsoft Expanded Memory
Specification 4.0. This gives a 64 KB page frame at segment `E000`, in 16 KB
pages from the shared pool. TOKAEMM maps a page when a program requests it.
Software that looks for the `EMMXXXX0` device name finds the manager in the
standard way. `INT 67h` answers the LIM 4.0 function set: status, frame
address, page counts, allocate, map, free, save, restore, and version.

The Toka-DOS `CONFIG.SYS` writes `RAM` directly. A `DEVICE=` line with no
argument has the same effect. Replace `RAM` with `NOEMS` to disable EMS and to
make the page-frame area available for UMB allocation.

## HMA and DOS=HIGH

`DOS=HIGH,UMB` in `CONFIG.SYS` does two things. It asks TOKAEMM for the HMA,
which moves most of the resident kernel above the 1 MB line and out of
conventional memory. It also enables the UMB load above. Both functions need
TOKAEMM in memory as a device driver. Thus `DEVICE=C:\DOS\TOKAEMM.SYS` must
come before the `DOS=` line.

## A20

The A20 gate controls the behavior of an address at the 1 MB boundary. When
A20 is off, the address goes back to zero at 1 MB, as on the original 8086.
When A20 is on, the address continues into extended memory.

TOKAEMM controls the gate in the usual way: with the local and global enable
and disable calls of XMS, and through the keyboard controller port `0x92`. DOS
and software of the period write to that port directly. From outside the
machine, the gate behaves as it does under a real EMM386.

## The V86 monitor

When Toka-DOS completes its boot, TOKAEMM puts the full system into the
Virtual-8086 mode of the CPU. TOKAEMM installs itself below the system as a
small supervisor. After that, the kernel, the shell, and each program that you
run operate in this virtual real-mode environment. They do not operate
directly on the CPU.

Usually, only privileged code can do some operations. Examples are a mask of
the interrupts, a read or a write of the flags register, a software interrupt
or a return from one, and a change to the A20 gate. When DOS or an application
does one of these operations, the CPU gives control to the monitor of TOKAEMM
instead of a fault. The monitor does the operation and gives control back. The
software does not detect the monitor.

TOKAEMM also uses this mechanism internally. Its XMS block move and its EMS
page remap each make a short privileged call into the same monitor, which then
does the memory operation safely. The result is that Toka-DOS behaves as a
usual real-mode system. At the same time, TOKAEMM has the low-level access
that it needs to supply XMS, EMS, and UMBs above the 1 MB boundary.

## Next

- [How to use Toka-DOS](../toka-dos/using-toka-dos.md): the shell that boots
  above TOKAEMM.
- [DOS command reference](../toka-dos/commands.md): `GSWMODE`, `TOKAMOUS`, and
  the other supplied tools.
