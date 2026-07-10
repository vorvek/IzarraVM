<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Company Story

Two names sit behind the Izarra 3000: the company that built the box, and
the company whose name is on the disk operating system it shipped with. This
page keeps the story short, the way an old product brochure would.

## Izarra Computer Systems (hardware)

Izarra Computer Systems started in 1987 as a small Spanish workstation shop
that built graphics terminals for schools and local studios. Its engineers
wanted a home computer that could run DOS games without feeling like a beige
PC, and they spent the next decade chasing that idea across three machines.

The Izarra 1000 arrived in 1990 around a 286 at 12 MHz, followed in 1993 by
the 386-based Izarra 2000 at 25 MHz. Both sold modestly to the schools and
studios that already knew the brand. Work on the 3000 began in late 1994 as
the most ambitious of the three: a tight motherboard around VGA, MIDI,
CD-ROM audio, and a friendly ROM shell, fast enough to make DOS games feel
at home.

The prototype was fast, but the timing was brutal. Windows 95 made
compatibility the only spec retailers cared about, so Izarra kept adding
bridge chips and fallback modes to reassure publishers. The board became
expensive and late. In April 1997, with the first production run still in
testing and suppliers asking for cash, the company filed for bankruptcy.
IzarraVM is what survived in the lab notes.

## General Simulation Works (software)

The system software (Toka Disk Operating System, the GSW-586 CPU, and the
`GSWMODE` speed switch) carries a different name: General Simulation Works.
It shows up once, in Toka-DOS's own boot banner:

```
Toka-DOS 3.0 (C) 1997 General Simulation Works - tongue firmly in cheek.
```

General Simulation Works is Izarra's software arm, or its software supplier,
depending on which lab note you believe. The record is thin, because the
company didn't outlive the hardware it was written for. What is clear from
the product itself: the "GSW" in GSW-586 is their initials, not a
coincidence, and the throttle-without-rebooting trick that lets one CPU
act as a 586, a 486, or a 386 at two speeds fits the pragmatic, slightly
cheeky engineering their boot banner promises.

## Reading the fiction

IzarraVM's documentation is written from inside this fiction: pages read as
manuals for real 1997 hardware and software, because holding that voice
consistently is more useful than breaking it every few paragraphs to remind
you it's an emulator. Where the emulator's actual behavior differs from what
the in-universe hardware would do (an unimplemented feature, a timing
shortcut, a placeholder), the relevant page says so plainly. See the [VEGA
Technical Reference, section 9](../vega/vega-technical-reference.md#9-timing-and-fidelity)
for the model this documentation follows: the fiction is a voice, not a
cover story.
