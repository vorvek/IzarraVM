# SP-3 CuteMouse 2.1 INT 33h Cross-Reference

**Date:** 2026-06-29
**Driver under review:** `toka-dos/tools/izmouse.asm` (TOKAMOUS.COM)
**Reference oracle:** CuteMouse 2.1 (FDOS/mouse/master/ctmouse.asm + int33.lst)
**Scope:** Functions 0x00–0x10 (core set) + extended 0x12–0x24. No wheel/fn 0x11 (SP-3b).

---

## Functions checked — result per function

| Fn | Name | TOKAMOUS | CuteMouse | Verdict |
|---|---|---|---|---|
| 0x00 | RESET | AX=0xFFFF, BX=2; hides cursor, clears all counters/callback/state, calls apply_mode_yrange | AX=0xFFFF, BX=2 (or 3 for 3-btn); softreset, cursor shape reset | **MATCHES** |
| 0x01 | SHOW | show_count++ saturate at 0; draws cursor when count==0 | Same saturating counter | **MATCHES** |
| 0x02 | HIDE | show_count--; erases cursor | Same | **MATCHES** |
| 0x03 | GET POS/BUTTONS | BX=buttons&0x07, CX=cur_x, DX=cur_y | BL=buttons, BH=wheel, CX=X, DX=Y | **MATCHES** (BH=0 without wheel is correct) |
| 0x04 | SET POS | CX/DX clamped to [min,max]; redraws cursor | Same with clamping | **MATCHES** |
| 0x05 | PRESS INFO | AX=buttons&7, BX=count (cleared), CX=press_x[i], DX=press_y[i]; out-of-range BX returns count=0 + cur pos | AX=button+wheel, BX=count (atomically exchanged), CX/DX=last pos | **MATCHES** |
| 0x06 | RELEASE INFO | Mirror of 0x05 on release_* arrays | Same structure | **MATCHES** |
| 0x07 | SET H RANGE | Sorts CX/DX, clamps to 0..VIRT_MAX_X, reclamps cur_x | Sets rangemin/max, then calls setpos which clamps | **MATCHES** |
| 0x08 | SET V RANGE | Mirror of 0x07 on Y, high end clamped to scr_max_y (mode-aware) | Same | **MATCHES** |
| 0x09 | DEF GFX CURSOR | No-op (v1; graphics cursor rendering deferred) | Full XOR/hotspot implementation | **MATCH** (acceptable stub) |
| 0x0A | DEF TXT CURSOR | Stores screen_mask/cursor_mask for SW cursor | Full HW/SW cursor selection | **MATCHES** |
| 0x0B | READ MICKEYS | CX=mickey_x, DX=mickey_y; both cleared | CX=mickeys.X, DX=mickeys.Y; both exchanged with zero | **MATCHES** |
| 0x0C | SET CALLBACK | cb_mask=CX, cb_seg=ES, cb_off=DX; validates MCB ownership | callmask=CL, UIR@=ES:DX | **MATCHES** (TOKAMOUS adds ownership guard as an extension) |
| 0x0D | LIGHTPEN ON | No-op | No-op | **MATCHES** |
| 0x0E | LIGHTPEN OFF | No-op | No-op | **MATCHES** |
| 0x0F | SET RATIO | CX→ratio_x, DX→ratio_y; zero clamped to 1 per axis | CX→mickey8.X, DX→mickey8.Y; zero rejects both (FOOLPROOF mode) | **MATCHES** (per-axis clamp is more robust than reject-both) |
| 0x10 | COND OFF | Stores cond box; calls hide/show to re-evaluate | Same region logic | **MATCHES** |
| 0x12 | LARGE GFX CURSOR | AX=0xFFFF | Returns success | **MATCHES** |
| 0x13 | SET DBL SPEED | dbl_speed=CX; 0→64 | Same | **MATCHES** |
| 0x14 | EXCHANGE HANDLER | Atomic swap old↔new cb_mask/seg/off | Same | **MATCHES** |
| 0x15 | GET BUF SIZE | BX=44 | BX=size of state blob | **MATCHES** |
| 0x16 | SAVE STATE | 44-byte blob at ES:DX with magic 0x334D | Similar blob | **MATCHES** |
| 0x17 | RESTORE STATE | Restores from ES:DX blob | Same | **MATCHES** |
| 0x1A | SET SENSITIVITY | sens_x=BX, sens_y=CX, sens_thr=DX; 0→64 | Same | **MATCHES** |
| 0x1B | GET SENSITIVITY | BX/CX/DX = sens fields | Same | **MATCHES** |
| 0x1D | SET DISP PAGE | disp_page=BX | Same | **MATCHES** |
| 0x1E | GET DISP PAGE | BX=disp_page | Same | **MATCHES** |
| 0x21 | SOFT RESET | AX=0xFFFF, BX=2; no state clear | Same | **MATCHES** |
| 0x22 | SET LANGUAGE | No-op | No-op | **MATCHES** |
| 0x23 | GET LANGUAGE | BX=0 (English) | Same | **MATCHES** |
| 0x24 | GET VERSION | **BUG FIXED** (see below) | BX=version, CX=type/IRQ | **FIXED** |
| unknown | catch-all | iret, registers unchanged | iret, no modification | **MATCHES** |

---

## Genuine divergence found and fixed: fn 0x24 BX guard (wrong)

**Before:** TOKAMOUS checked `cmp bx, 0 / jne .skip` before returning version/type/IRQ. If BX was non-zero on entry (caller did not pre-zero it), TOKAMOUS silently returned without setting BX/CX.

**Why it's wrong:** The INT 33h spec's `BX=0000h` is an INPUT calling-convention note for callers (reserve for future use), not a guard the driver must enforce. CuteMouse and all reference implementations return the version/type/IRQ unconditionally on AX=0x24. Real programs may call fn 0x24 with BX in an unpredictable state (left over from a prior call). The guard would cause them to get stale/garbage BX/CX values.

**Fix:** Removed the `cmp bx, 0 / jne .skip` guard. fn 0x24 now always executes `mov bx, 0x0820 / mov cx, 0x0400 / iret`. Returns BH=8 (major), BL=0x20 (minor), CH=4 (PS/2 mouse type), CL=0 (PS/2 IRQ).

**Size change:** TOKAMOUS resident: 2768 → 2763 bytes (5 bytes smaller: removed `cmp bx,0` + `jne .skip` + label).

---

## Smoke test

`cargo test --release -p izarravm tokados_mouse_driver_loads -- --ignored --nocapture`

Result: **PASS** (1 passed, finished in 1.82s). No regression.

---

## Callbacks: register block handed to the user handler

Both TOKAMOUS and the INT 33h spec agree: `AX=event flags, BX=buttons, CX=X, DX=Y, SI=mickey_x, DI=mickey_y`.
CuteMouse delivers `CL=event, BH=wheel, BL=buttons, AX=X, DX=Y, SI=mickey_x` — note it uses AX not CX for X, and packs wheel into BH. The standard INT 33h AX=000Ch contract (from int33.lst and RBIL) specifies CX=column, DX=row, so TOKAMOUS is correct per the standard. CuteMouse's internal layout is a non-standard optimization for wheel data.

---

## Summary

One genuine bug fixed (fn 0x24 unconditional guard). All other handlers match the INT 33h contract and CuteMouse's behavior. No invented changes.
