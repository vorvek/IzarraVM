/****************************************************************/
/*                                                              */
/*                           main.c                             */
/*                            DOS-C                             */
/*                                                              */
/*                    Main Kernel Functions                     */
/*                                                              */
/*                   Copyright (c) 1995, 1996                   */
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
/* write to the Free Software Foundation, Inc.,                 */
/* 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA.     */
/****************************************************************/
/* Modified by the Toka-DOS project, 2026: signon() paints a rainbow Toka-DOS boot
   logo straight into B800:0 text RAM and prints a welcome box (build number, compile
   date, copyright) instead of the FreeDOS copyright block; the full FreeDOS/Villani
   copyright + GPL still ships in C:\LICENSE.TXT (see scripts/license_txt.py). */

#include "portab.h"
#include "init-mod.h"
#include "dyndata.h"
#include "debug.h"

#ifdef VERSION_STRINGS
static BYTE *mainRcsId =
    "$Id: main.c 1699 2012-01-16 20:45:44Z perditionc $";
#endif

/* Boot logo, drawn by signon() straight into B800:0 text RAM with a static
   diagonal rainbow attribute. Substitution glyphs keep the source ASCII:
   '%' = 0xB1 (medium shade), '#' = 0xDB (full block), ']' = 0xDD (left half). */
static char toka_logo_r0[] = " %####### %#########  %######       %# %###     %##          %#";
static char toka_logo_r1[] = "       %# %###      %###     %##    %# %###    %##          %###  %#####%##%##";
static char toka_logo_r2[] = "       %# %###   %# %###       %#]  %# %###   %##          %#####   %#  %#%#%#";
static char toka_logo_r3[] = "       %# %###  %## %###        %#] %# %###  %##            %#####  %#  %#  %#";
static char toka_logo_r4[] = "       %# %###  %## %###        %## %# %### %##          %#  %#####";
static char toka_logo_r5[] = "       %# %###  %## %###        %## %# %###%##          %#%#  %#####";
static char toka_logo_r6[] = "       %# %###  %## %###        %#] %# %#####]         %#  %#  %#####";
static char toka_logo_r7[] = "       %# %###   %# %###       %#]  %# %### %##       %#    %#  %#####";
static char toka_logo_r8[] = "       %# %###      %###     %##    %# %###   %##    %#########  %#####";
static char toka_logo_r9[] = "       %# %###        %######]      %# %###     %## %#        %#  %#####";

/* Every row must fit 80 columns (81 bytes with NUL): a negative array size
   is the loudest guard C89 offers. */
#define TOKA_ROW_FITS(n) \
  typedef char toka_logo_row_##n##_fits[(sizeof(toka_logo_r##n) <= 81) ? 1 : -1]
TOKA_ROW_FITS(0); TOKA_ROW_FITS(1); TOKA_ROW_FITS(2); TOKA_ROW_FITS(3);
TOKA_ROW_FITS(4); TOKA_ROW_FITS(5); TOKA_ROW_FITS(6); TOKA_ROW_FITS(7);
TOKA_ROW_FITS(8); TOKA_ROW_FITS(9);

/* Same idiom for the welcome-box strings (version.h): TOKA_BOX_TEXT_W (76,
   init-mod.h) is the usable column count, so a fitting string plus its NUL
   must be <= TOKA_BOX_TEXT_W + 1 bytes. Catches an overlong build line at
   compile time instead of silently truncating it on screen. */
typedef char toka_build_line_1_fits[
  (sizeof(TOKA_BUILD_LINE_1) <= TOKA_BOX_TEXT_W + 1) ? 1 : -1];
typedef char toka_build_line_2_fits[
  (sizeof(TOKA_BUILD_LINE_2) <= TOKA_BOX_TEXT_W + 1) ? 1 : -1];

static char *toka_logo[] = {
  toka_logo_r0, toka_logo_r1, toka_logo_r2, toka_logo_r3, toka_logo_r4,
  toka_logo_r5, toka_logo_r6, toka_logo_r7, toka_logo_r8, toka_logo_r9,
};
#define TOKA_LOGO_ROWS (sizeof(toka_logo) / sizeof(toka_logo[0]))

struct _KernelConfig InitKernelConfig BSS_INIT({0});

STATIC VOID InitIO(void);

STATIC VOID update_dcb(struct dhdr FAR *);
STATIC VOID init_kernel(VOID);
STATIC VOID signon(VOID);
STATIC VOID kernel(VOID);
STATIC VOID FsConfig(VOID);
STATIC VOID InitPrinters(VOID);
STATIC VOID InitSerialPorts(VOID);
STATIC void CheckContinueBootFromHarddisk(void);
STATIC void setup_int_vectors(void);

#ifdef _MSC_VER
BYTE _acrtused = 0;

__segment DosDataSeg = 0;       /* serves for all references to the DOS DATA segment 
                                   necessary for MSC+our funny linking model
                                 */
__segment DosTextSeg = 0;

#endif

struct lol FAR *LoL = &DATASTART;

VOID ASMCFUNC FreeDOSmain(void)
{
  unsigned char drv;
  unsigned char FAR *p;

#ifdef _MSC_VER
  extern FAR prn_dev;
  DosDataSeg = (__segment) & DATASTART;
  DosTextSeg = (__segment) & prn_dev;
#endif

  /* clear the Init BSS area (what normally the RTL does */
  memset(_ib_start, 0, _ib_end - _ib_start);

                        /*  if the kernel has been UPX'ed,
                                CONFIG info is stored at 50:e2 ..fc
                            and the bootdrive (passed from BIOS)
                            at 50:e0
                        */

  drv = LoL->BootDrive + 1;
  p = MK_FP(0, 0x5e0);
  if (fmemcmp(p+2,"CONFIG",6) == 0)      /* UPX */
  {
    fmemcpy(&InitKernelConfig, p+2, sizeof(InitKernelConfig));

    drv = *p + 1;
    *(DWORD FAR *)(p+2) = 0;
  }
  else
  {
    *p = drv - 1;
    fmemcpy(&InitKernelConfig, &LowKernelConfig, sizeof(InitKernelConfig));
  }

  if (drv >= 0x80)
    drv = 3; /* C: */
  LoL->BootDrive = drv;

  /* install DOS API and other interrupt service routines, basic kernel functionality works */
  setup_int_vectors();

  CheckContinueBootFromHarddisk();

  /* display copyright info and kernel emulation status */
  signon();

  /* initialize all internal variables, process CONFIG.SYS, load drivers, etc */
  init_kernel();

#ifdef DEBUG
  /* Non-portable message kludge alert!   */
  printf("KERNEL: Boot drive = %c\n", 'A' + LoL->BootDrive - 1);
#endif

  DoInstall();

  kernel();
}

/*
    InitializeAllBPBs()
    
    or MakeNortonDiskEditorHappy()

    it has been determined, that FDOS's BPB tables are initialized,
    only when used (like DIR H:).
    at least one known utility (norton DE) seems to access them directly.
    ok, so we access for all drives, that the stuff gets build
*/
void InitializeAllBPBs(VOID)
{
  static char filename[] = "A:-@JUNK@-.TMP";
  int drive, fileno;
  for (drive = 'C'; drive < 'A' + LoL->nblkdev; drive++)
  {
    filename[0] = drive;
    if ((fileno = open(filename, O_RDONLY)) >= 0)
      close(fileno);
  }
}

STATIC void PSPInit(void)
{
  psp far *p = MK_FP(DOS_PSP, 0);

  /* Clear out new psp first                              */
  fmemset(p, 0, sizeof(psp));

  /* initialize all entries and exits                     */
  /* CP/M-like exit point                                 */
  p->ps_exit = 0x20cd;

  /* CP/M-like entry point - call far to special entry    */
  p->ps_farcall = 0x9a;
  p->ps_reentry = MK_FP(0, 0x30 * 4);
  /* unix style call - 0xcd 0x21 0xcb (int 21, retf)      */
  p->ps_unix[0] = 0xcd;
  p->ps_unix[1] = 0x21;
  p->ps_unix[2] = 0xcb;

  /* Now for parent-child relationships                   */
  /* parent psp segment                                   */
  p->ps_parent = FP_SEG(p);
  /* previous psp pointer                                 */
  p->ps_prevpsp = MK_FP(0xffff,0xffff);

  /* Environment and memory useage parameters             */
  /* memory size in paragraphs                            */
  /*  p->ps_size = 0; clear from above                    */
  /* environment paragraph                                */
  p->ps_environ = DOS_PSP + 8;
  /* terminate address                                    */
  p->ps_isv22 = getvec(0x22);
  /* break address                                        */
  p->ps_isv23 = getvec(0x23);
  /* critical error address                               */
  p->ps_isv24 = getvec(0x24);

  /* user stack pointer - int 21                          */
  /* p->ps_stack = NULL; clear from above                 */

  /* File System parameters                               */
  /* maximum open files                                   */
  p->ps_maxfiles = 20;
  fmemset(p->ps_files, 0xff, 20);

  /* open file table pointer                              */
  p->ps_filetab = p->ps_files;

  /* default system version for int21/ah=30               */
  p->ps_retdosver = (LoL->os_setver_minor << 8) + LoL->os_setver_major;

  /* first command line argument                          */
  /* p->ps_fcb1.fcb_drive = 0; already set                */
  fmemset(p->ps_fcb1.fcb_fname, ' ', FNAME_SIZE + FEXT_SIZE);
  /* second command line argument                         */
  /* p->ps_fcb2.fcb_drive = 0; already set                */
  fmemset(p->ps_fcb2.fcb_fname, ' ', FNAME_SIZE + FEXT_SIZE);

  /* local command line                                   */
  /* p->ps_cmd.ctCount = 0;     command tail, already set */
  p->ps_cmd.ctBuffer[0] = 0xd; /* command tail            */
}

#ifndef __WATCOMC__
/* for WATCOMC we can use the ones in task.c */
intvec getvec(unsigned char intno)
{
  intvec iv;
  disable();
  iv = *(intvec FAR *)MK_FP(0,4 * (intno));
  enable();
  return iv;
}

void setvec(unsigned char intno, intvec vector)
{
  disable();
  *(intvec FAR *)MK_FP(0,4 * intno) = vector;
  enable();
}
#endif

/* Toka-DOS 2026: the boot INT 2Fh vector, saved by setup_int_vectors and
   consumed by IzarraCdClaim. */
STATIC intvec Izarra_old2f;
STATIC VOID IzarraCdClaim(VOID);
STATIC VOID IzarraHddMapClaim(VOID);

STATIC void setup_int_vectors(void)
{
  static struct vec
  {
    unsigned char intno;
    size_t handleroff;
  } vectors[] =
    {
      /* all of these are in the DOS DS */
      { 0x0, FP_OFF(int0_handler) },   /* zero divide */
      { 0x1, FP_OFF(empty_handler) },  /* single step */
      { 0x3, FP_OFF(empty_handler) },  /* debug breakpoint */
      { 0x6, FP_OFF(int6_handler) },   /* invalid opcode */
      { 0x19, FP_OFF(int19_handler) },
      { 0x20, FP_OFF(int20_handler) },
      { 0x21, FP_OFF(int21_handler) },
      { 0x22, FP_OFF(int22_handler) },
      { 0x24, FP_OFF(int24_handler) },
      { 0x25, FP_OFF(low_int25_handler) },
      { 0x26, FP_OFF(low_int26_handler) },
      { 0x27, FP_OFF(int27_handler) },
      { 0x28, FP_OFF(int28_handler) },
      { 0x2a, FP_OFF(int2a_handler) },
      { 0x2f, FP_OFF(int2f_handler) }
    };
  struct vec *pvec;
  struct lowvec FAR *plvec;
  int i;

  for (plvec = intvec_table; plvec < intvec_table + 5; plvec++)
    plvec->isv = getvec(plvec->intno);
  /* Toka-DOS 2026: keep the boot INT 2Fh vector (the Izarra BIOS stub)
     before the kernel handler replaces it. When the IzarraCD claim
     succeeds, int2f.asm forwards AH=11h/15h to it. */
  Izarra_old2f = getvec(0x2f);
  for (i = 0x23; i <= 0x3f; i++)
    setvec(i, empty_handler);
  /* Modified by the Toka-DOS project, 2026: 0 -> 1, matching the default set in
     config.c. This is the earlier of the two initializers; leaving it at 0
     would idle-spin for the whole window between here and CONFIG.SYS. */
  HaltCpuWhileIdle = 1;
  for (pvec = vectors; pvec < vectors + (sizeof vectors/sizeof *pvec); pvec++)
    setvec(pvec->intno, (intvec)MK_FP(FP_SEG(empty_handler), pvec->handleroff));
  pokeb(0, 0x30 * 4, 0xea);
  pokel(0, 0x30 * 4 + 1, (ULONG)cpm_entry);

  /* these two are in the device driver area LOWTEXT (0x70) */
  setvec(0x1b, got_cbreak);
  setvec(0x29, int29_handler);  /* required for printf! */
}

STATIC void init_kernel(void)
{
  COUNT i;

  LoL->os_setver_major = LoL->os_major = MAJOR_RELEASE;
  LoL->os_setver_minor = LoL->os_minor = MINOR_RELEASE;

  /* Init oem hook - returns memory size in KB    */
  ram_top = init_oem();

  /* move kernel to high conventional RAM, just below the init code */
#ifdef __WATCOMC__
  lpTop = MK_FP(_CS, 0);
#else
  lpTop = MK_FP(_CS - (FP_OFF(_HMATextEnd) + 15) / 16, 0);
#endif

  MoveKernel(FP_SEG(lpTop));
  lpTop = MK_FP(FP_SEG(lpTop) - 0xfff, 0xfff0);

  /* Initialize IO subsystem                                      */
  InitIO();
  InitPrinters();
  InitSerialPorts();

  init_PSPSet(DOS_PSP);
  set_DTA(MK_FP(DOS_PSP, 0x80));
  PSPInit();

  Init_clk_driver();

  /* Do first initialization of system variable buffers so that   */
  /* we can read config.sys later.  */

  /* use largest possible value for the initial CDS */
  LoL->lastdrive = 26;

  /*  init_device((struct dhdr FAR *)&blk_dev, NULL, 0, &ram_top); */
  blk_dev.dh_name[0] = dsk_init();

  PreConfig();

  /* Number of units */
  if (blk_dev.dh_name[0] > 0)
    update_dcb(&blk_dev);

  /* Now config the temporary file system */
  FsConfig();

  /* Now process CONFIG.SYS     */
  DoConfig(0);
  DoConfig(1);

  /* initialize near data and MCBs */
  PreConfig2();
  /* and process CONFIG.SYS one last time for device drivers */
  DoConfig(2);


  /* Close all (device) files */
  for (i = 0; i < 20; i++)
    close(i);

  /* and do final buffer allocation. */
  PostConfig();

  /* Init the file system one more time     */
  FsConfig();
  
  configDone();

  IzarraCdClaim();
  IzarraHddMapClaim();

  InitializeAllBPBs();
}

#ifdef __WATCOMC__
STATIC UBYTE izarra_inp(UWORD port);
#pragma aux izarra_inp = "in al,dx" parm [dx] value [al] modify exact [al];
STATIC VOID izarra_outp(UWORD port, UBYTE value);
#pragma aux izarra_outp = "out dx,al" parm [dx] [al] modify exact [];
#endif

/* Added by the Toka-DOS project, 2026: claim the IzarraCD ROM extension.
 *
 * The Izarra3000's CD-ROM is a proprietary-interface drive whose support
 * software lives in the system BIOS. This claim runs once CONFIG.SYS is
 * done: probe the Lotura chipset (port 0xE0 answers 0x5A), hand the BIOS
 * the DOS data segment through the IzarraCD mailbox (0000:063Ch) and the
 * doorbell (port 0xE8, command 2), then mark drive D: as a redirector
 * drive with the exact CDS flag word the IZCDEX redirector wrote, and arm
 * the INT 2Fh forward in int2f.asm. On any other machine the probe fails
 * and every stock path stays. */
STATIC VOID IzarraCdClaim(VOID)
{
#ifdef __WATCOMC__
  struct cds FAR *cdsp;
  UBYTE status;

  if (izarra_inp(0xE0) != 0x5A)
    return;
  if (LoL->lastdrive <= 3)
    return;
  cdsp = &LoL->CDSp[3];
  if (cdsp->cdsFlags & (CDSNETWDRV | CDSPHYSDRV))
    return;

  pokew(0, 0x63C, FP_SEG(LoL));
  izarra_outp(0xE8, 0x02);
  {
    /* Bounded, like the other hardware gates: a host that parks the
       doorbell busy must degrade to "no CD", not hang the boot. */
    UWORD spin = 0xFFFF;
    while ((status = izarra_inp(0xE8)) == 0x01 && --spin != 0)
      ;
  }
  if (status != 0)
    return;

  /* Network + physical + hidden-from-redirector: the word IZCDEX's
     SetRoot stored, and the signature it scanned for at uninstall. */
  cdsp->cdsFlags = CDSNETWDRV | CDSPHYSDRV | 0x80;

  izarra_cd_arm(FP_OFF(Izarra_old2f), FP_SEG(Izarra_old2f));

  /* One boot-tree line in the shared glyph style. */
  printf("%c%c> IzarraCD ROM Extensions: CD-ROM is drive %c:\n",
         0xC3, 0xC4, 'A' + 3);
#endif
}

/* Toka-DOS 2026, Tier B B3: claim the IzarraVM FAT-position hypercall.
   Probe first with a null mailbox: a host that knows command 3 parks
   0xFE ("parsed, nothing registered"); an older host parks 0xFF, and
   open bus reads 0xFF. Only a 0xFE probe answer registers the real
   block. Spins are bounded like IzarraCdClaim's: a wedged host must
   degrade to the native walk, never hang the boot. */
STATIC VOID IzarraHddMapClaim(VOID)
{
#ifdef __WATCOMC__
  UBYTE status;
  UWORD spin;

  if (izarra_inp(0xE0) != 0x5A)
    return;

  disable();
  pokew(0, 0x63C, 0);
  pokew(0, 0x63E, 0);
  izarra_outp(0xE8, 0x03);
  enable();
  spin = 0xFFFF;
  while ((status = izarra_inp(0xE8)) == 0x01 && --spin != 0)
    ;
  if (status != 0xFE)
    return;

  disable();
  pokew(0, 0x63C, FP_OFF(&IzarraMapReq));
  pokew(0, 0x63E, FP_SEG(LoL));
  izarra_outp(0xE8, 0x03);
  enable();
  spin = 0xFFFF;
  while ((status = izarra_inp(0xE8)) == 0x01 && --spin != 0)
    ;
  if (status != 0)
    return;
  IzarraMapArmed = 1;
#endif
}

STATIC VOID FsConfig(VOID)
{
  struct dpb FAR *dpb = LoL->DPBp;
  int i;

  /* Initialize the current directory structures    */
  for (i = 0; i < LoL->lastdrive; i++)
  {
    struct cds FAR *pcds_table = &LoL->CDSp[i];

    fmemcpy(pcds_table->cdsCurrentPath, "A:\\\0", 4);

    pcds_table->cdsCurrentPath[0] += i;

    if (i < LoL->nblkdev && (ULONG) dpb != 0xffffffffl)
    {
      pcds_table->cdsDpb = dpb;
      pcds_table->cdsFlags = CDSPHYSDRV;
      dpb = dpb->dpb_next;
    }
    else
    {
      pcds_table->cdsFlags = 0;
    }
    pcds_table->cdsStrtClst = 0xffff;
    pcds_table->cdsParam = 0xffff;
    pcds_table->cdsStoreUData = 0xffff;
    pcds_table->cdsJoinOffset = 2;
  }

  /* Log-in the default drive. */
  init_setdrive(LoL->BootDrive - 1);

  /* The system file tables need special handling and are "hand   */
  /* built. Included is the stdin, stdout, stdaux and stdprn. */
  /* a little bit of shuffling is necessary for compatibility */

  /* sft_idx=0 is /dev/aux                                        */
  open("AUX", O_RDWR);

  /* handle 1, sft_idx=1 is /dev/con (stdout) */
  open("CON", O_RDWR);

  /* 3 is /dev/aux                */
  dup2(STDIN, STDAUX);

  /* 0 is /dev/con (stdin)        */
  dup2(STDOUT, STDIN);

  /* 2 is /dev/con (stdin)        */
  dup2(STDOUT, STDERR);

  /* 4 is /dev/prn                                                */
  open("PRN", O_WRONLY);

  /* Initialize the disk buffer management functions */
  /* init_call_init_buffers(); done from CONFIG.C   */
}

STATIC VOID signon_box_edge(int left, int right)
{
  int i;
  printf("%c", left);
  for (i = 0; i < TOKA_BOX_INNER_W; i++)
    printf("%c", 0xC4);
  printf("%c\n", right);
}

STATIC VOID signon_box_text(char *s)
{
  int col = 2;
  printf("%c ", 0xB3);
  for (; *s && col < TOKA_BOX_TEXT_END; s++, col++)   /* clamp: overlong text truncates, never shears the frame */
    printf("%c", *s);
  for (; col < TOKA_BOX_TEXT_END; col++)
    printf(" ");
  printf("%c\n", 0xB3);
}

STATIC VOID signon()
{
  static char ramp[6] = { 0x0C, 0x0E, 0x0A, 0x0B, 0x09, 0x0D };
  iregs r;
  int row, col;

  /* Mode 3 reset clears the POST screen and homes the cursor. */
  r.a.x = 0x0003;
  init_call_intr(0x10, &r);

  /* The logo goes straight to text RAM: printf (INT 29h TTY) writes
     attribute 07h only, and the rainbow is the point. */
  for (row = 0; row < TOKA_LOGO_ROWS; row++)
  {
    char *p = toka_logo[row];
    for (col = 0; p[col]; col++)
    {
      unsigned char glyph;
      switch (p[col])
      {
        case '%': glyph = 0xB1; break;
        case '#': glyph = 0xDB; break;
        case ']': glyph = 0xDD; break;
        default:  continue;
      }
      pokew(0xB800, (row * 80 + col) * 2,
            ((UWORD)ramp[((col + 2 * row) / 7) % 6] << 8) | glyph);
    }
  }

  /* Park the cursor under the logo so the box prints below the art instead
     of over it. No spacer row: the 25-row budget has no room for one. */
  r.a.x = 0x0200;
  r.b.b.h = 0;
  r.d.b.h = TOKA_LOGO_ROWS;       /* row */
  r.d.b.l = 0;                    /* col */
  init_call_intr(0x10, &r);

  signon_box_edge(0xDA, 0xBF);
  signon_box_text(TOKA_BUILD_LINE_1);
  signon_box_text(TOKA_BUILD_LINE_2);
  signon_box_edge(0xC3, 0xD9);    /* left tee: the box edge feeds the tree */

  printf(TOKA_TREE_PREFIX "Kernel compatibility: %d.%d - "
#if defined(__WATCOMC__)
  "WATCOMC"
#else
#error unsupported compiler for the Toka-DOS signon
#endif
#ifdef WITHFAT32
  " - FAT32 support"
#endif
  "\n", MAJOR_RELEASE, MINOR_RELEASE);
}

STATIC void kernel()
{
  CommandTail Cmd;

  if (master_env[0] == '\0')   /* some shells panic on empty master env. */
    strcpy(master_env, "PATH=.");
  fmemcpy(MK_FP(DOS_PSP + 8, 0), master_env, sizeof(master_env));

  /* process 0       */
  /* Execute command.com from the drive we just booted from    */
  memset(Cmd.ctBuffer, 0, sizeof(Cmd.ctBuffer));
  strcpy(Cmd.ctBuffer, Config.cfgInitTail);

  for (Cmd.ctCount = 0; Cmd.ctCount < sizeof(Cmd.ctBuffer); Cmd.ctCount++)
    if (Cmd.ctBuffer[Cmd.ctCount] == '\r')
      break;

  /* if stepping CONFIG.SYS (F5/F8), tell COMMAND.COM about it */

  /* 3 for string + 2 for "\r\n" */
  if (Cmd.ctCount < sizeof(Cmd.ctBuffer) - 5)
  {
    char *insertString = NULL;

    if (singleStep)
      insertString = " /Y";     /* single step AUTOEXEC */

    if (SkipAllConfig)
      insertString = " /D";     /* disable AUTOEXEC */

    if (insertString)
    {

      /* insert /D, /Y as first argument */
      char *p, *q;

      for (p = Cmd.ctBuffer; p < &Cmd.ctBuffer[Cmd.ctCount]; p++)
      {
        if (*p == ' ' || *p == '\t' || *p == '\r')
        {
          for (q = &Cmd.ctBuffer[Cmd.ctCount + 1]; q >= p; q--)
            q[3] = q[0];
          memcpy(p, insertString, 3);
          break;
        }
      }
      /* save buffer -- on the stack it's fine here */
      Config.cfgInitTail = Cmd.ctBuffer;
    }
  }
  init_call_p_0(&Config); /* go execute process 0 (the shell) */
}

/* check for a block device and update  device control block    */
STATIC VOID update_dcb(struct dhdr FAR * dhp)
{
  REG COUNT Index;
  COUNT nunits = dhp->dh_name[0];
  struct dpb FAR *dpb;

  if (LoL->nblkdev == 0)
    dpb = LoL->DPBp;
  else
  {
    for (dpb = LoL->DPBp; (ULONG) dpb->dpb_next != 0xffffffffl;
         dpb = dpb->dpb_next)
      ;
    dpb = dpb->dpb_next =
      KernelAlloc(nunits * sizeof(struct dpb), 'E', Config.cfgDosDataUmb);
  }

  for (Index = 0; Index < nunits; Index++)
  {
    dpb->dpb_next = dpb + 1;
    dpb->dpb_unit = LoL->nblkdev;
    dpb->dpb_subunit = Index;
    dpb->dpb_device = dhp;
    dpb->dpb_flags = M_CHANGED;
    if ((LoL->CDSp != 0) && (LoL->nblkdev < LoL->lastdrive))
    {
      LoL->CDSp[LoL->nblkdev].cdsDpb = dpb;
      LoL->CDSp[LoL->nblkdev].cdsFlags = CDSPHYSDRV;
    }
    ++dpb;
    ++LoL->nblkdev;
  }
  (dpb - 1)->dpb_next = (void FAR *)0xFFFFFFFFl;
}

/* If cmdLine is NULL, this is an internal driver */

BOOL init_device(struct dhdr FAR * dhp, char *cmdLine, COUNT mode,
                 char FAR **r_top)
{
  request rq;
  char name[8];

  if (cmdLine) {
    char *p, *q, ch;
    int i;

    p = q = cmdLine;
    for (;;)
    {
      ch = *p;
      if (ch == '\0' || ch == ' ' || ch == '\t')
        break;
      p++;
      if (ch == '\\' || ch == '/' || ch == ':')
        q = p; /* remember position after path */
    }
    for (i = 0; i < 8; i++) {
      ch = '\0';
      if (p != q && *q != '.')
        ch = *q++;
      /* copy name, without extension */
      name[i] = ch;
    }
  }

  rq.r_unit = 0;
  rq.r_status = 0;
  rq.r_command = C_INIT;
  rq.r_length = sizeof(request);
  rq.r_endaddr = *r_top;
  rq.r_bpbptr = (void FAR *)(cmdLine ? cmdLine : "\n");
  rq.r_firstunit = LoL->nblkdev;

  execrh((request FAR *) & rq, dhp);

/*
 *  Added needed Error handle
 */
  if ((rq.r_status & (S_ERROR | S_DONE)) == S_ERROR)
    return TRUE;

  if (cmdLine)
  {
    /* Don't link in device drivers which do not take up memory */
    if (rq.r_endaddr == (BYTE FAR *) dhp)
      return TRUE;

    /* Don't link in block device drivers which indicate no units */
    if (!(dhp->dh_attr & ATTR_CHAR) && !rq.r_nunits)
    {
      rq.r_endaddr = (BYTE FAR *) dhp;
      return TRUE;
    }


    /* Fix for multisegmented device drivers:                          */
    /*   If there are multiple device drivers in a single driver file, */
    /*   only the END ADDRESS returned by the last INIT call should be */
    /*   the used.  It is recommended that all the device drivers in   */
    /*   the file return the same address                              */

    if (FP_OFF(dhp->dh_next) == 0xffff)
    {
      KernelAllocPara(FP_SEG(rq.r_endaddr) + (FP_OFF(rq.r_endaddr) + 15)/16
                      - FP_SEG(dhp), 'D', name, mode);
    }

    /* Another fix for multisegmented device drivers:                  */
    /*   To help emulate the functionallity experienced with other DOS */
    /*   operating systems when calling multiple device drivers in a   */
    /*   single driver file, save the end address returned from the    */
    /*   last INIT call which will then be passed as the end address   */
    /*   for the next INIT call.                                       */

    *r_top = (char FAR *)rq.r_endaddr;
  }

  if (!(dhp->dh_attr & ATTR_CHAR) && (rq.r_nunits != 0))
  {
    dhp->dh_name[0] = rq.r_nunits;
    update_dcb(dhp);
  }

  if (dhp->dh_attr & ATTR_CONIN)
    LoL->syscon = dhp;
  else if (dhp->dh_attr & ATTR_CLOCK)
    LoL->clock = dhp;

  return FALSE;
}

STATIC void InitIO(void)
{
  struct dhdr far *device = &LoL->nul_dev;

  /* Initialize driver chain                                      */
  do {
    init_device(device, NULL, 0, &lpTop);
    device = device->dh_next;
  }
  while (FP_OFF(device) != 0xffff);
}

/* issue an internal error message                              */
VOID init_fatal(BYTE * err_msg)
{
  printf("\nInternal kernel error - %s\nSystem halted\n", err_msg);
  for (;;) ;
}

/*
       Initialize all printers
 
       this should work. IMHO, this might also be done on first use
       of printer, as I never liked the noise by a resetting printer, and
       I usually much more often reset my system, then I print :-)
 */

STATIC VOID InitPrinters(VOID)
{
  iregs r;
  int num_printers, i;

  init_call_intr(0x11, &r);     /* get equipment list */

  num_printers = (r.a.x >> 14) & 3;     /* bits 15-14 */

  for (i = 0; i < num_printers; i++)
  {
    r.a.x = 0x0100;             /* initialize printer */
    r.d.x = i;
    init_call_intr(0x17, &r);
  }
}

STATIC VOID InitSerialPorts(VOID)
{
  iregs r;
  int serial_ports, i;

  init_call_intr(0x11, &r);     /* get equipment list */

  serial_ports = (r.a.x >> 9) & 7;      /* bits 11-9 */

  for (i = 0; i < serial_ports; i++)
  {
    r.a.x = 0xA3;               /* initialize serial port to 2400,n,8,1 */
    r.d.x = i;
    init_call_intr(0x14, &r);
  }
}

/*****************************************************************
        if kernel.config.BootHarddiskSeconds is set,
        the default is to boot from harddisk, because
        the user is assumed to just have forgotten to
        remove the floppy/bootable CD from the drive.
        
        user has some seconds to hit ANY key to continue
        to boot from floppy/cd, else the system is 
        booted from HD
*/

STATIC int EmulatedDriveStatus(int drive,char statusOnly)
{
  iregs r;
  char buffer[0x13];
  buffer[0] = 0x13;

  r.a.b.h = 0x4b;               /* bootable CDROM - get status */
  r.a.b.l = statusOnly;
  r.d.b.l = (char)drive;          
  r.si  = (int)buffer;
  init_call_intr(0x13, &r);     
  
  if (r.flags & 1)
        return FALSE;
  
  return TRUE;  
}

STATIC void CheckContinueBootFromHarddisk(void)
{
  char *bootedFrom = "Floppy/CD";
  iregs r;
  int key;

  if (InitKernelConfig.BootHarddiskSeconds == 0)
    return;

  if (LoL->BootDrive >= 3)
  {
#if 0
    if (!EmulatedDriveStatus(0x80,1))
#endif
    {
      /* already booted from HD */
      return;
    }
  }
  else {
#if 0
    if (!EmulatedDriveStatus(0x00,1))
#endif
      bootedFrom = "Floppy";
  }

  printf("\n"
         "\n"
         "\n"
         "     Hit any key within %d seconds to continue boot from %s\n"
         "     Hit 'H' or    wait %d seconds to boot from Harddisk\n",
         InitKernelConfig.BootHarddiskSeconds,
         bootedFrom,
         InitKernelConfig.BootHarddiskSeconds
    );

  key = GetBiosKey(InitKernelConfig.BootHarddiskSeconds);
  
  if (key != -1 && (key & 0xff) != 'h' && (key & 0xff) != 'H')
  {
    /* user has hit a key, continue to boot from floppy/CD */
    printf("\n");
    return;
  }

  /* reboot from harddisk */
  EmulatedDriveStatus(0x00,0);
  EmulatedDriveStatus(0x80,0);

  /* now jump and run */
  r.a.x = 0x0201;
  r.c.x = 0x0001;
  r.d.x = 0x0080;
  r.b.x = 0x7c00;
  r.es  = 0;

  init_call_intr(0x13, &r);

  {
#if __GNUC__
    asm volatile("jmp $0,$0x7c00");
#else
    void (far *reboot)(void) = (void (far*)(void)) MK_FP(0x0,0x7c00);

    (*reboot)();
#endif
  }
}
