; This is an LBA-enabled FreeDOS FAT32 boot sector (single sector!).
; You can use and copy source code and binaries under the terms of the
; GNU Public License (GPL), version 2 or newer. See www.gnu.org for more.

; Based on earlier work by FreeDOS kernel hackers, modified heavily by
; Eric Auer and Jon Gentle in 7 / 2003.
;
; Features: Uses LBA and calculates all variables from BPB/EBPB data,
; thus making partition move / resize / image-restore easier. FreeDOS
; can boot from FAT32 partitions which start > 8 GB boundary with this
; boot sector. Disk geometry knowledge is not needed for booting.
;
; Windows uses 2-3 sectors for booting (sector stage, statistics sector,
; filesystem stage). Only using 1 sector for FreeDOS makes multi-booting
; of FreeDOS and Windows on the same filesystem easier.
;
; Requirements: LBA BIOS and 186 or better CPU. (Toka-DOS: the upstream
; sector required a 386; this copy does all its 32-bit LBA/cluster math as
; 16-bit word pairs so a GSW-386-slow cold boot uses the same compact path as
; every faster GSW mode. `cpu 286` below preserves the assembler boundary;
; the only 186+ features used are push-immediate and shift-by-imm8.)
;
; FAT12 / FAT16 hints: Use the older CHS-only boot sector unless you
; have to boot from > 8 GB. The LBA-and-CHS FAT12 / FAT16 boot sector
; needs applying SYS again after move / resize / ... a variant of that
; boot sector without CHS support but with better move / resize / ...
; support would be good for use on LBA harddisks.


; Memory layout for the FreeDOS FAT32 single stage boot process:

;	...
;	|-------| 1FE0:7E00
;	|BOOTSEC|
;	|RELOC.	|
;	|-------| 1FE0:7C00
;	...
;	|-------| 2000:0200
;	|  FAT  | (only 1 sector buffered)
;	|-------| 2000:0000
;	...
;	|-------| 0000:7E00
;	|BOOTSEC| overwritten by the kernel, so the
;	|ORIGIN | bootsector relocates itself up...
;	|-------| 0000:7C00
;	...
;	|-------|
;	|KERNEL	| maximum size 134k (overwrites bootsec origin)
;	|LOADED	| (holds 1 sector directory buffer before kernel load)
;	|-------| 0060:0000
;	...

segment	.text

		cpu	286		; see the requirements note above
		org	0x7c00		; this is a boot sector

Entry:		jmp	short real_start
		nop

;	bp is initialized to 7c00h
; %define bsOemName	bp+0x03	; OEM label (8)
%define bsBytesPerSec	bp+0x0b ; bytes/sector (dw)
%define bsSecPerClust	bp+0x0d	; sectors/allocation unit (db)
%define bsResSectors	bp+0x0e	; # reserved sectors (dw)
%define bsFATs		bp+0x10	; # of fats (db)
; %define bsRootDirEnts	bp+0x11	; # of root dir entries (dw, 0 for FAT32)
			; (FAT32 has root dir in a cluster chain)
; %define bsSectors	bp+0x13	; # sectors total in image (dw, 0 for FAT32)
			; (if 0 use nSectorHuge even if FAT16)
; %define bsMedia	bp+0x15	; media descriptor: fd=2side9sec, etc... (db)
; %define sectPerFat	bp+0x16	; # sectors in a fat (dw, 0 for FAT32)
			; (FAT32 always uses xsectPerFat)
%define sectPerTrack	bp+0x18	; # sectors/track
; %define nHeads	bp+0x1a	; # heads (dw)
%define nHidden		bp+0x1c	; # hidden sectors (dd)
; %define nSectorHuge	bp+0x20	; # sectors if > 65536 (dd)
%define xsectPerFat	bp+0x24	; Sectors/Fat (dd)
			; +0x28 dw flags (for fat mirroring)
			; +0x2a dw filesystem version (usually 0)
%define xrootClst	bp+0x2c	; Starting cluster of root directory (dd)
			; +0x30 dw -1 or sector number of fs.-info sector
			; +0x32 dw -1 or sector number of boot sector backup
			; (+0x34 .. +0x3f reserved)
%define drive		bp+0x40	; Drive number
%define loadsegoff_60	bp+loadseg_off-Entry

%define LOADSEG		0x0060

%define FATSEG		0x2000

%define	fat_secshift	fat_afterss-1	; each fat sector describes 2^??
					; clusters (db) (selfmodifying)
%define fat_sector	bp+0x44		; last accessed FAT sector (dd)
					; (overwriting unused bytes)
%define fat_start	bp+0x48		; first FAT sector (dd)
					; (overwriting unused bytes)
%define data_start	bp+0x4c		; first data sector (dd)
					; (overwriting unused bytes)
%define cur_clust	bp+0x34		; current cluster in the walk (dd)
					; (overwriting reserved bytes)
%define sect_left	bp+0x38		; sectors left in the cluster (db)
					; (overwriting reserved bytes)

		times   52h - ($ - $$) db 0
		; The filesystem ID is used by lDOS's instsect (by ecm)
		;  by default to validate that the filesystem matches.
		db "FAT32"
		times   5Ah - ($ - $$) db 32
		; not used: [0x42] = byte 0x29 (ext boot param flag)
		; [0x43] = dword serial
		; [0x47] = label (padded with 00, 11 bytes)
		; [0x52] = "FAT32",32,32,32 (not used by Windows)
		; ([0x5a] is where FreeDOS parts start)

;-----------------------------------------------------------------------
; ENTRY
;-----------------------------------------------------------------------

real_start:	cld
		cli
		sub	ax, ax
		mov	ds, ax
		mov	bp, 0x7c00

		mov	ax, 0x1FE0
		mov	es, ax
		mov	si, bp
		mov	di, bp
		mov	cx, 0x0100
		rep	movsw		; move boot code to the 0x1FE0:0x0000
		jmp	word 0x1FE0:cont

loadseg_off	dw	0, LOADSEG

; -------------

cont:		mov	ds, ax
		mov	ss, ax		; stack and BP-relative moves up, too
                lea     sp, [bp-0x20]
		sti
		mov	[drive], dl	; BIOS passes drive number in DL

		; (Toka-DOS: the BIOS already prints "Starting Toka-DOS..." and the
		; kernel prints its own banner, so the boot sector loads silently --
		; the old "Loading FreeDOS " message and string were removed.)

; -------------

;	CALCPARAMS: figure out where FAT and DATA area starts
;	(modifies AX DX CX, sets fat_start and data_start variables)
;	All 32-bit values live in memory dwords handled as word pairs.

calc_params:	xor	ax, ax
		mov	[fat_sector], ax	; init buffer status: sector 0 is
		mov	[fat_sector+2], ax	; never a FAT sector (reserved>0)

		; first, find fat_start = bsResSectors + nHidden:
		mov	ax, [bsResSectors]
		xor	dx, dx
		add	ax, [nHidden]
		adc	dx, [nHidden+2]
		mov 	[fat_start], ax		; first FAT sector
		mov 	[fat_start+2], dx
		mov	[data_start], ax
		mov	[data_start+2], dx

		; next, data_start += bsFATs * xsectPerFat, one add per FAT
		; (bsFATs is 1 or 2; a loop beats a 32x16 multiply here).
		; CH is 0: the relocation rep movsw above ran CX down to zero
		; and nothing since has touched it.
		mov	cl, [bsFATs]
add_fat:	mov	ax, [xsectPerFat]
		mov	dx, [xsectPerFat+2]
		add	[data_start], ax	; first DATA sector
		adc	[data_start+2], dx
		loop	add_fat

		; finally, find fat_secshift:
		mov	ax, 512	; default sector size (means default shift)
				; shift = log2(secSize) - log2(fatEntrySize)
;---		mov	cl, 9-2	; shift is 7 for 512 bytes per sector
fatss_scan:	cmp	ax, [bsBytesPerSec]
		jz	fatss_found
		add	ax,ax
;---		inc	cx
		inc	word [fat_secshift]	;XXX	; initially 9-2 (byte!)
		jmp 	short fatss_scan	; try other sector sizes
fatss_found:
;---		mov	[fat_secshift], cl

; -------------

; FINDFILE:	Searches for the file in the root directory.
; Returns:	DX:AX = first cluster of file
; Cluster numbers travel in DX:AX; convert_cluster parks the cluster being
; walked in [cur_clust] and next_cluster reads it back from there
; (readDisk/cmpsb need the registers).

		mov	ax, [xrootClst]		; root dir cluster
		mov	dx, [xrootClst+2]

ff_next_clust:	call	convert_cluster
		jc	boot_error		; EOC encountered
		; DX:AX is the sector, [sect_left] sectors per cluster

ff_next_sector:	les	bx, [loadsegoff_60]	; load to loadseg:0
		call	readDisk		; advances DX:AX to the next sector

		xor	di, di			;XXX

		; Search for KERNEL.SYS file name, and find start cluster.
ff_next_entry:	mov	cx, 11
		mov	si, filename
		repe	cmpsb
		jz	ff_done		; note that di now is at dirent+11

		add	di, byte 0x20		;XXX
		and	di, byte -0x20 ; 0xffe0	;XXX
		cmp	di, [bsBytesPerSec]	;XXX
		jnz	ff_next_entry

		dec 	byte [sect_left]	; next sector in cluster
		jnz	ff_next_sector

ff_walk_fat:	call	next_cluster		; find next cluster
		jmp	short ff_next_clust	; (reads [cur_clust] itself)

ff_done:	mov	ax, [es:di+0x1A-11]	; get cluster number LO
		mov	dx, [es:di+0x14-11]	; get cluster number HI

		sub	bx, bx			; ES points to LOADSEG
						; (kernel -> ES:BX)

; -------------

read_kernel:	call	convert_cluster
		jc	boot_success		; EOC encountered - done
		; DX:AX is the sector, [sect_left] sectors per cluster

rk_in_cluster:	call	readDisk
		dec	byte [sect_left]
		jnz	rk_in_cluster		; loop over sect. in cluster

rk_walk_fat:	call	next_cluster
		jmp	short read_kernel
		
;-----------------------------------------------------------------------

boot_success:	mov	bl, [drive]
		jmp	far [loadsegoff_60]

;-----------------------------------------------------------------------

boot_error:	mov	si, msg_BootError
		call	print			; modifies AX BX SI

wait_key:	xor	ah,ah
		int	0x16			; wait for a key
reboot:		int	0x19			; reboot the machine

;-----------------------------------------------------------------------

; given a cluster number, find the number of the next cluster in
; the FAT chain. Needs fat_secshift and fat_start.
; input:	[cur_clust] - cluster (parked there by convert_cluster)
; output:	DX:AX - next cluster
; (modifies CL: callers re-derive their counts from convert_cluster)

next_cluster:	push	es
		push	di
		push	bx
		mov	ax, [cur_clust]
		mov	dx, [cur_clust+2]

		mov	di, ax
		shl	di, 2			; 32bit FAT

		push	ax
		mov	ax, [bsBytesPerSec]
		dec	ax
		and	di, ax			; mask to sector size
		pop	ax

		mov	cl, 7			; e.g. 9-2 for 512 by/sect.
fat_afterss:	; selfmodifying code: previous byte is patched!
		; (to hold the fat_secshift value)
cn_shift:	shr	dx, 1			; DX:AX >>= fat_secshift, as a
		rcr	ax, 1			; word-pair loop (8086-safe)
		dec	cl
		jnz	cn_shift

		add	ax, [fat_start]		; absolute sector number now
		adc	dx, [fat_start+2]

		mov	bx, FATSEG
		mov	es, bx
		sub	bx, bx

		cmp	ax, [fat_sector]	; already buffered?
		jnz	cn_load
		cmp	dx, [fat_sector+2]
		jz	cn_buffered
cn_load:	mov	[fat_sector], ax	; number of buffered sector
		mov	[fat_sector+2], dx
		call	readDisk

cn_buffered:	mov	ax, [es:di]		; read next cluster number
		mov	dx, [es:di+2]
		and	dh, 0x0f		; mask out the top 4 bits

		pop	bx
		pop 	di
		pop	es
		ret


;-----------------------------------------------------------------------

; Convert cluster number to the absolute sector number
; ... or return carry if EndOfChain! Needs data_start.
; input:	DX:AX - target cluster
; output:	DX:AX - absolute sector
;		[sect_left] - [bsSectPerClust] (byte)
;		carry clear
;		(if carry set, DX:AX unchanged, end of chain)

convert_cluster:
		mov	[cur_clust], ax	; park the cluster: ff/rk_walk_fat's
		mov	[cur_clust+2], dx ; next_cluster reads it back
		cmp	dx, 0x0fff	; if end of cluster chain...
		jb	cc_in_chain	; (EOC = high word 0x0FFF with low >=
		cmp	ax, 0xfff8	; 0xFFF8; next_cluster masks the top
		jnb	end_of_chain	; nibble so the high word can't exceed
cc_in_chain:				; 0x0FFF)
		; sector = (cluster-2) * clustersize + data_start
		sub	ax, 2
		sbb	dx, 0

		; sectors/cluster is a power of two by the FAT specification,
		; so the multiply is a word-pair shift by log2(spc).
		mov	cl, [bsSecPerClust]
		mov	[sect_left], cl	; per-cluster count for the callers
cc_mul:		shr	cl, 1
		jz	cc_mul_done	; spc=1 shifts zero times
		shl	ax, 1
		rcl	dx, 1
		jmp	short cc_mul
cc_mul_done:
		add	ax, [data_start]
		adc	dx, [data_start+2]
		; here, carry is unset (unless parameters are wrong)
		ret

end_of_chain:	stc			; indicate EOC by carry
		ret

;-----------------------------------------------------------------------

; PRINT - prints string DS:SI
; modifies AX BX SI

printchar:	xor	bx, bx		; video page 0
		mov	ah, 0x0e	; print it
		int	0x10		; via TTY mode
print:		lodsb			; get token
		cmp	al, 0		; end of string?
		jne	printchar	; until done
		ret			; return to caller

;-----------------------------------------------------------------------

; Read a sector from disk, using LBA
; input:	DX:AX - 32-bit DOS sector number
;		ES:BX - destination buffer
;		(will be filled with 1 sector of data)
; output:	ES:BX points one byte after the last byte read.
;		DX:AX - next sector
; (CX untouched; SI/DI preserved)

readDisk:	push	si
		push	di

read_next:	push	dx	; save the sector for retry / increment
		push	ax
		mov	di, sp	; remember parameter block end

		push	0	; [E] sector number high 32bit, upper word
		push	0	; [C] sector number high 32bit, lower word
		push	dx	; [A] sector number low 32bit, upper word
		push	ax	; [8] sector number low 32bit, lower word
		push	es	; [6] buffer segment
		push	bx	; [4] buffer offset
		push	byte 1	; [2] 1 sector (word)
		push	byte 16	; [0] size of parameter block (word)
		mov	si, sp
		mov	dl, [drive]
		mov	ah, 42h	; disk read
		int	0x13

		mov	sp, di	; remove parameter block from stack
				; (without changing flags!)
		jc	rd_retry	; on error: reset and retry

		pop	ax	; restore the sector number...
		pop	dx
		inc	ax	; ...and advance to the next one
		jnz	rd_no_hi
		inc	dx
rd_no_hi:	add	bx, word [bsBytesPerSec]
		jnc	no_incr_es		; if overflow...

		mov	si, es
		add	si, 0x1000		; ...add 1000h to ES
		mov	es, si

no_incr_es:	pop	di
		pop 	si
		ret

rd_retry:	xor	ah, ah	; disk reset; DL is still the drive number
		int	0x13	; (the BIOS preserves it across AH=42h)
		pop	ax	; restore the sector number
		pop	dx
		jmp	short read_next

;-----------------------------------------------------------------------

       times 0x01ee-$+$$ db 0

msg_BootError	db "No "
		; currently, only "kernel.sys not found" gives a message,
		; but read errors in data or root or fat sectors do not.

filename	db "KERNEL  SYS"

sign		dw 0, 0xAA55
		; Win9x uses all 4 bytes as magic value here.
