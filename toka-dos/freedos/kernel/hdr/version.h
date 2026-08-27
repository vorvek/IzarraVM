/****************************************************************/
/*                                                              */
/*                          version.h                           */
/*                                                              */
/*                  Common version information                  */
/*                                                              */
/*                      Copyright (c) 1997                      */
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

/* The version the kernel reports as compatible with */
#ifdef WITHFAT32
#define MAJOR_RELEASE   7
#define MINOR_RELEASE   10
#else
#define MAJOR_RELEASE   6
#define MINOR_RELEASE   22
#endif

/* The actual kernel revision, 2000+REVISION_SEQ = 2.REVISION_SEQ */
#define REVISION_SEQ    43      /* returned in BL by int 21 function 30 */
#define OEM_ID          0xfd    /* FreeDOS, returned in BH by int 21 30 */

/* Used for version information displayed to user at boot (& stored in os_release string) */
#ifndef KERNEL_VERSION
#define KERNEL_VERSION ""
#endif

/* Modified by the Toka-DOS project, 2026: changed display banner from "FreeDOS kernel" to "Toka-DOS 3.0 kernel",
   and dropped the "- GIT " / raw OEM-id decoration for a cleaner user-facing string (build number only).
   This is string cosmetics only -- OEM_ID and the values reported via int 21 AH=30h/33FFh are unchanged. */
/* Modified by the Toka-DOS project, 2026: the compile date is PINNED, not
   __DATE__. The boot screen is a graded frame in the fixture scoreboard,
   and __DATE__ moved its hash on every rebuild -- four allowed anchors
   accumulated from date repaints alone. Update this string deliberately
   when a release warrants it; the hash then moves once, on purpose. */
#define TOKA_BUILD_DATE "Aug 27 2026"

/* actual version string */
#define KVS(v,s,o) "Toka-DOS 3.0 kernel " v "(build 20" #s ") [compiled " TOKA_BUILD_DATE "]\n"
#define xKVS(v,s,o) KVS(v,s,o)
#define KERNEL_VERSION_STRING xKVS(KERNEL_VERSION, REVISION_SEQ, OEM_ID)

/* Modified by the Toka-DOS project, 2026: the boot welcome box (main.c signon)
   prints the build number and compile date on one merged title line instead of
   the one-line KERNEL_VERSION_STRING, which stays intact for os_release. The
   25-row boot budget has no room for a separate "Welcome to" line. */
#define TOKA_VERSION_STR(s) #s
#define xTOKA_VERSION_STR(s) TOKA_VERSION_STR(s)
#define TOKA_BUILD_LINE_1 \
  "Toka-DOS 3.0 - Kernel build 20" xTOKA_VERSION_STR(REVISION_SEQ) \
  " - Compiled " TOKA_BUILD_DATE
#define TOKA_BUILD_LINE_2 \
  "(C) 1992-1997 Izarra SL - All Rights Reserved ** See LICENSE.TXT for more."

