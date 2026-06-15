# Clean-Room DOS Policy

IzarraVM implements DOS-compatible behavior for game compatibility. FreeDOS and MS-DOS behavior may be studied as references, but their source code must not be copied, translated, or derived into this repository. The constraint is clean-room provenance, not license compatibility: no implementation is a source to translate, regardless of its license.

The first DOS service target is a host-backed `C:` drive and enough INT/service scaffolding to support early game bring-up later.
