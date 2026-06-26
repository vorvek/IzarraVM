/* keyb.c - KEYB: keyboard layout selector for Toka-DOS.
 *
 * Usage: KEYB [xx]
 *   With no argument, reports the current BIOS layout.
 *   With a supported MS-DOS layout code, updates the BIOS layout byte and CMOS.
 *
 * CMOS 0x10 holds the keyboard layout index; CMOS 0x11 holds the code-page
 * index that drives the VGA font loader on the next boot.
 *
 * Code-page indices: 0=CP437 (US/UK), 1=CP850 (Western Europe),
 * 2=CP860 (Portugal), 3=CP863 (Canadian French), 4=CP865 (Nordic).
 */
#include <string.h>
#include <conio.h>
#include <i86.h>
#include "toka.h"

#define BDA_KB_LAYOUT (*(unsigned char far *)MK_FP(0x0040, 0x0096))
#define CMOS_KB_LAYOUT  0x10
#define CMOS_KB_CODEPAGE 0x11

struct layout_entry {
    const char *code;
    unsigned char index;
    unsigned char codepage;
};

static const struct layout_entry layouts[] = {
    { "US",  0, 0 },
    { "UK",  1, 0 },
    { "SP",  2, 1 },
    { "FR",  3, 1 },
    { "GR",  4, 1 },
    { "IT",  5, 1 },
    { "BE",  6, 1 },
    { "CF",  7, 3 },
    { "DK",  8, 4 },
    { "NL",  9, 1 },
    { "NO", 10, 4 },
    { "PO", 11, 2 },
    { "SF", 12, 1 },
    { "SG", 13, 1 },
    { "SU", 14, 1 },
    { "SV", 15, 1 },
    { "LA", 16, 1 },
};

static const struct layout_entry *find_layout(const char *code)
{
    unsigned i;
    for (i = 0; i < sizeof(layouts) / sizeof(layouts[0]); i++) {
        if (strcmp(layouts[i].code, code) == 0) {
            return &layouts[i];
        }
    }
    return 0;
}

static const char *layout_name(unsigned char index)
{
    unsigned i;
    for (i = 0; i < sizeof(layouts) / sizeof(layouts[0]); i++) {
        if (layouts[i].index == index) {
            return layouts[i].code;
        }
    }
    return "US";
}

static void set_bios_layout(unsigned char index, unsigned char codepage)
{
    BDA_KB_LAYOUT = index;
    outp(0x70, CMOS_KB_LAYOUT);
    outp(0x71, index);
    outp(0x70, CMOS_KB_CODEPAGE);
    outp(0x71, codepage);
}

int main(void)
{
    char *p = t_cmdtail();
    static char code[8];
    const struct layout_entry *layout;
    int k = 0;
    char c;

    /* Skip the leading blanks DOS leaves before the first argument. */
    while (*p == ' ' || *p == '\t') {
        p++;
    }

    /* MS-DOS KEYB takes xx as the first comma or whitespace-delimited token. */
    while (*p && *p != ' ' && *p != '\t' && *p != ',' && k < 7) {
        c = *p;
        if (c >= 'a' && c <= 'z') {
            c = (char)(c - 'a' + 'A');
        }
        code[k++] = c;
        p++;
    }
    code[k] = 0;

    if (k == 0) {
        t_puts("Current keyboard layout: ");
        t_putln(layout_name(BDA_KB_LAYOUT));
    } else {
        layout = find_layout(code);
        if (!layout) {
            t_putln("Invalid keyboard layout");
            t_exit(1);
            return 1;
        }
        set_bios_layout(layout->index, layout->codepage);
        t_puts("Keyboard layout set: ");
        t_putln(layout->code);
    }

    t_exit(0);
    return 0;
}
