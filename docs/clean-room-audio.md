# Clean-Room Audio Policy

IzarraVM reproduces the Resonique 2 sound hardware (OPL3 FM synthesis, Sound
Blaster 16 DSP) for game compatibility. Behaviour is derived from primary
hardware documentation, not from other emulators' source code.

Permitted sources, to study and cite freely:

- Yamaha YMF262 (OPL3) datasheet.
- "Programmer's Guide to the Yamaha YMF262/OPL3" (V. Arnost, 1994) and ADLIB.DOC.

Restricted:

- Nuked-OPL3 and the DOSBox OPL cores are LGPL 2.1 / GPL. They may be consulted
  ONLY to check an assumption — confirm a table value or an edge-case behaviour.
  Their code must not be copied, translated, or otherwise derived into this
  repository. The constraint is clean-room provenance, not license
  compatibility: even a permissively-licensed implementation is off-limits as a
  source to translate.

Lookup tables are generated from published formulas. Any value cross-checked
against a restricted source must be independently re-derived from the datasheet
or first principles before it lands here.
