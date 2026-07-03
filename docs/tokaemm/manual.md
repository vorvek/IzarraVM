# TOKAEMM.SYS: Memory Manager

TOKAEMM.SYS is General Simulation Works's memory manager for the Izarra
3000: one driver providing extended memory (XMS), expanded memory (EMS),
and upper memory blocks (UMBs), in the tradition of EMM386.EXE on a real
386-or-better PC. This is its manual.

## Loading it

`CONFIG.SYS` loads TOKAEMM as a device driver, before `DOS=HIGH,UMB`:

```
DEVICE=C:\DOS\TOKAEMM.SYS [NOEMS | RAM]
```

The Toka-DOS default ships as:

```
DEVICE=C:\DOS\TOKAEMM.SYS NOEMS
DOS=HIGH,UMB
```

## Switches

TOKAEMM recognizes exactly one command-line keyword: **RAM**. Everything
else on the `DEVICE=` line (including `NOEMS`) is accepted but not
specially parsed; the driver is frameless by default, and writing `NOEMS`
explicitly is just documentation of that default, the same convention
EMM386.EXE uses. There is no `FRAME=` switch and no memory-size argument.

| Argument | Effect |
| --- | --- |
| *(none)*, or `NOEMS` | Frameless: XMS, UMBs, and HMA are provided; `INT 67h` answers "present" but with zero EMS pages. This is the shipped default. |
| `RAM` | Provisions a real EMS 4.0 page frame at segment `E000`, backed by a pool carved from extended memory, in addition to everything the frameless mode provides. |

## XMS

TOKAEMM installs as the extended memory manager via the standard `INT 2Fh`
hook, and answers as **XMS 3.0**. It implements the core function set DOS
and drivers rely on:

- **HMA**: request and release the high memory area (functions 01h/02h),
  which lets `DOS=HIGH` relocate the kernel there.
- **A20 control**: global and local enable/disable, with nesting, plus a
  query function (03h-07h).
- **Extended memory blocks**: allocate, free, resize, lock/unlock, and query
  free space (08h-0Fh), through a 32-handle allocator over the machine's
  extended memory.
- **Block moves**: the `INT 21h`-style bulk-copy function (0Bh) that moves
  data between conventional and extended memory, or between two extended
  blocks.
- **UMB functions**: request, release, and reallocate upper memory blocks
  (10h-12h), which is what `DOS=UMB` and `LOADHIGH`/`DEVICEHIGH` actually
  call.

## Upper memory blocks (DOS=UMB)

With `DOS=UMB` in `CONFIG.SYS`, FreeDOS's kernel claims upper memory blocks
from TOKAEMM through the XMS UMB functions above, and `DEVICEHIGH=`/`LH`
lines (including `LH TOKAMOUS` in the stock `AUTOEXEC.BAT`) load into them
instead of conventional memory whenever one is free. TOKAEMM backs this
upper memory window with real extended RAM mapped in over the address hole
above the video BIOS, so loading high genuinely frees conventional memory
rather than faking it.

## EMS 4.0 (the RAM keyword)

`DEVICE=C:\DOS\TOKAEMM.SYS RAM` turns on Lotus/Intel/Microsoft Expanded Memory
Specification 4.0 support: a 64 KB page frame at segment `E000`, mapped in
16 KB pages backed by a 4 MB pool of extended memory. Software that checks
for EMS the standard way (looking for the `EMMXXXX0` device name) finds
it, and `INT 67h` answers the LIM 4.0 function set: status, frame address,
page counts, allocate/map/free/save/restore, and version.

The shipped Toka-DOS `CONFIG.SYS` uses the frameless default instead, so EMS
page mapping is off out of the box; add `RAM` to the `DEVICE=` line to turn
it on for software that specifically wants EMS pages rather than XMS.

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
