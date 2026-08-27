/****************************************************************/
/*                                                              */
/*                            buffer.h                          */
/*                                                              */
/* Sector buffer structure                                      */
/*                                                              */
/*                      Copyright (c) 2001                      */
/*			Bart Oldeman				*/
/*								*/
/*			Largely taken from globals.h:		*/
/*			Copyright (c) 1995, 1996                */
/*                      Pasquale J. Villani                     */
/*                      All Rights Reserved                     */
/*                                                              */
/* This file is part of DOS-C.                                  */
/*                                                              */
/* DOS-C is free software; you can redistribute it and/or       */
/* modify it under the terms of the GNU General Public License  */
/* as published by the Free Software Foundation; either version */
/* 2, or (at your option) any later version.                    */
/*                                                              */
/* DOS-C is distributed in the hope that it will be useful, but */
/* WITHOUT ANY WARRANTY; without even the implied warranty of   */
/* MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See    */
/* the GNU General Public License for more details.             */
/*                                                              */
/* You should have received a copy of the GNU General Public    */
/* License along with DOS-C; see the file COPYING.  If not,     */
/* write to the Free Software Foundation, 675 Mass Ave,         */
/* Cambridge, MA 02139, USA.                                    */
/****************************************************************/

#ifdef MAIN
#ifdef VERSION_STRINGS
static BYTE *buffer_hRcsId =
    "$Id: buffer.h 1702 2012-02-04 08:46:16Z perditionc $";
#endif
#endif

#include "dsk.h"                /* only for MAX_SEC_SIZE        */
#define BUFFERSIZE MAX_SEC_SIZE

/* modified by the Toka-DOS project, 2026: getblk_fat (blockio.c) fills a
   run of this many FAT sectors in one dskxfer call. PostConfig (config.c)
   sizes the UMB span buffer from the same count, so the count lives here
   next to BUFFERSIZE. */
#ifndef FAT_PREFETCH_SECS
#define FAT_PREFETCH_SECS 32
#endif

/* modified by the Toka-DOS project, 2026: searchblock's offset-hint table
   (blockio.c). One UWORD buffer offset per slot, keyed by the block
   number's low bits; 0xFFFF marks an empty slot (a 532-byte buffer can
   never start at that offset, while offset 0 is a legal KernelAlloc
   result). PostConfig (config.c) sizes the UMB carve from this count, so
   it lives here next to FAT_PREFETCH_SECS. */
#define BUF_INDEX_SLOTS 64
#define BUF_INDEX_EMPTY 0xFFFFu

struct buffer {
  UWORD b_next;                 /* next buffer in LRU list      */
  UWORD b_prev;                 /* previous buffer in LRU list  */
  BYTE b_unit;                  /* disk for this buffer         */
  BYTE b_flag;                  /* buffer flags                 */
  ULONG b_blkno;                /* block for this buffer        */
  UBYTE b_copies;               /* number of copies to write    */
  UWORD b_offset;               /* offset in sectors between copies
                                   to write for FAT sectors     */
  struct dpb FAR *b_dpbp;       /* pointer to DPB               */
  UWORD b_remotesz;             /* size of remote buffer if remote */
  BYTE b_padding;
  UBYTE b_buffer[BUFFERSIZE];   /* 512 byte sectors for now     */
};

#define BFR_DIRTY       0x40    /* buffer modified              */
#define BFR_VALID       0x20    /* buffer contains valid data   */
#define BFR_DATA        0x08    /* buffer is from data area     */
#define BFR_DIR         0x04    /* buffer is from dir area      */
#define BFR_FAT         0x02    /* buffer is from fat area      */
#define BFR_UNCACHE     0x01    /* buffer to be released ASAP   */

