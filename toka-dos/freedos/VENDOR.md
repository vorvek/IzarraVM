# Vendored FreeDOS source (corresponding source for shipped Toka-DOS binaries)

These trees are unmodified FreeDOS source from the FreeDOS 1.4 release, with Toka-DOS
rebrand edits applied on top (each modified file carries a "modified by the Toka-DOS
project, 2026" note; original FreeDOS/Villani copyright + GPL notices preserved verbatim).
FreeDOS is GPLv2-or-later; this project is GPL-3.0-only. This tree is the GPL
"corresponding source" for the committed crates/izarravm-firmware/roms/tokados.img.

- kernel:  github.com/FDOS/kernel  tag ke2043  commit 4f7bdda16a84c416a82a2616aa67335ca4f2bd74
- freecom: github.com/FDOS/freecom tag com086  commit f1b8f4f464eae5a70348b6d362484d733d45c427

Neither tag has git submodules. (Upstream master has since diverged: kernel 2045 / FreeCom 0.87.)
