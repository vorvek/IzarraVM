<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Environment variables

IzarraVM reads a number of `IZARRAVM_*` environment variables. This page lists
the ones that have a read site in the code. For the command-line switches, see
the [command-line reference](command-line.md).

**Note 1.** Most of these variables are developer controls. They exist to
measure the emulator and to test one change at a time. Their names and their
meanings can change between versions. Only the user settings in the first
table are stable. A normal user needs no other variable.

**Note 2.** The spelling of "off" is not the same for all variables. Read the
Values column of each variable. These are the cases in the code:

- Many on/off controls accept `0` or `off` to turn the feature off, and `1` or
  `on` to turn it on. They stop with a message on any other value. They do not
  guess.
- Some controls turn off only on the exact value `0`. Any other value, or no
  value, leaves them on.
- Some controls turn on when the variable is set to any value, even `0` or an
  empty value.
- Some controls turn on only on the exact value `1`.
- Some controls treat an empty value and `0` as off.
- Number controls have no off value. Remove the variable to get the default.
- An empty value can mean the default. For a control that is on by default, an
  empty value means on. For a control that is off by default, it means off.

Almost all of these variables are read once, when the process or the machine
starts. Change them before you start IzarraVM.

## User settings

| Name | Values | Default | Effect |
| --- | --- | --- | --- |
| `IZARRAVM_DOSROOT` | A directory path. | Not set | Gives the fallback C: drive folder. The `--c-drive` switch has priority. An empty value counts as not set. |
| `IZARRAVM_NO_DIALOG` | Any value except empty, `0`, `off` and `false`. | Not set | On Windows, stops the message box that the AVX2 check shows when no console is attached. |
| `IZARRAVM_CORPUS_ROOT` | A directory path. | Not set | Gives the root folder for the `izarravm-corpus` scanner. The `--corpus-root` switch has priority. |

## Traces and diagnostics

These variables write extra output, usually to stderr. Some of them need a
build feature. This is stated in the Effect column.

| Name | Values | Default | Effect |
| --- | --- | --- | --- |
| `IZARRAVM_FAULT_TRACE` | Any value except empty, `0`, `off` and `false`. | Not set | Prints a trace of CPU faults and of the exit of the test device. |
| `IZARRAVM_DIFF_TRACE` | Any value except empty, `0`, `off` and `false`. | Not set | Writes a per-instruction trace for differential checks. |
| `IZARRAVM_INT_TRACE` | Hexadecimal vectors, separated by commas. | Not set | Prints the arguments and the answer of each traced `INT n`. Needs the `int-trace` build feature. |
| `IZARRAVM_WATCH_WRITE` | `<hex address>[,<hex length>]`, as physical addresses. | Not set | Reports the guest stores to the address range. The default length is `0x100`. Needs the `watch-write` build feature. |
| `IZARRAVM_WATCH_WRITE_LIMIT` | A decimal number. | The built-in limit | Sets the maximum number of store reports. |
| `IZARRAVM_OPL_TRACE` | A decimal number. | `0` (off) | Records up to this number of OPL events. |
| `IZARRAVM_MIDI_TRACE` | A decimal number. | `0` (off) | Records up to this number of MIDI events. |
| `IZARRAVM_PIT_TRACE` | A decimal number. | `0` (off) | Records up to this number of timer events. |
| `IZARRAVM_SB_CMD_TRACE` | Any value except empty, `0`, `off` and `false`. | Not set | Prints the Sound Blaster DSP commands, the mixer writes, and the DMA mode writes. |
| `IZARRAVM_WSS_TRACE` | Any value except empty, `0`, `off` and `false`. | Not set | Prints each access to the Windows Sound System ports. |
| `IZARRAVM_SB_DEBUG` | Any value except empty, `0`, `off` and `false`. | Not set | Prints Sound Blaster path statistics. |
| `IZARRAVM_AUDIO_DEBUG` | Any value except empty, `0`, `off` and `false`. | Not set | Counts the events of the host audio output. |
| `IZARRAVM_VBE_TRACE` | Any value except empty, `0`, `off` and `false`. | Not set | Prints the VBE mode set requests. |
| `IZARRAVM_SMC_TRACE` | Any value other than empty or `0` turns it on. | Off | Records the self-modifying code invalidations in a headless run. |
| `IZARRAVM_SMC_TRACE_OUT` | A file path. | Standard output | Names the file for the report of `IZARRAVM_SMC_TRACE`. |
| `IZARRAVM_UNIT_SIM` | Any value other than empty or `0` turns it on. | Off | Runs the unit-growth simulator in a headless run. |
| `IZARRAVM_IPE_WINDOW_TRACE` | A file path. | Not set | Writes a windowed trace of instructions per native entry. The parent directory must exist. |
| `IZARRAVM_DUMP_LINEAR` | `<hex address>[,<hex length>]` | Around the hottest address | Chooses the memory window that the CPU profile dumps. The default length is `0x180`. |
| `IZARRAVM_GUI_INJECT_KEYS` | `<guest ms>:<text>` steps, separated by `;`. | Not set | Types keys into the guest at fixed times in the GUI. A bad value is ignored with a warning. |
| `IZARRAVM_AUDIO_WAV` | A file path. | Not set | Writes the audio mix of a headless run to this WAV file. It cannot be set together with `IZARRAVM_AUDIO_COST`. |
| `IZARRAVM_AUDIO_COST` | `count`, `off`. | Not set | Runs the audio capture observer in count mode, or as a control that renders nothing. |
| `IZARRAVM_AUDIO_COST_SLICE_MS` | A positive number. | `10` | Sets the step of the audio capture, in guest milliseconds. |
| `IZARRAVM_AUDIO_WAV_WALL` | Any value other than empty or `0` turns it on. | Off | Paces the audio capture by wall time and not by guest time. |
| `IZARRAVM_REFLECTED_CALL_DIAGNOSTIC` | `shape`, `journal`. | Off | Measures reflected calls. Needs the `reflected-call-diagnostic` build feature. |
| `IZARRAVM_REFLECTED_CALL_DIAG_WINDOW` | A window of retired instructions. | Whole run | Limits the reflected-call diagnostic to a window. |
| `IZARRAVM_REFLECTED_CALL_MEMO` | `1`, `on`, `memo` turn it on. `0`, `off`, or empty turn it off. | Off | Arms the reflected-call memo. Needs the `reflected-call-memo` build feature. |
| `IZARRAVM_REFLECTED_CALL_MEMO_AUDIT` | A number. `0` turns the audit off. | A built-in period | Sets the period of the memo audit. Needs the `reflected-call-memo` build feature. |

### Census variables

A census counts events and prints a summary. Use a build without other
diagnostics when you compare timing.

| Name | Values | Default | Effect |
| --- | --- | --- | --- |
| `IZARRAVM_RMW_CENSUS` | Any value other than empty or `0` turns it on. | Off | Counts the read, write, and read-modify-write shapes of memory access. |
| `IZARRAVM_SLOT_CENSUS` | Any value other than empty or `0` turns it on. | Off | Counts the instruction slots that the recompiler rejects. |
| `IZARRAVM_SLOW_READ_HISTO` | Any value other than empty or `0` turns it on. | Off | Counts slow data reads for each 4 KiB page. |
| `IZARRAVM_IO_HIST` | Any value other than empty or `0` turns it on. | Off | Counts port reads for each port. |
| `IZARRAVM_DIRECT_BARRIER_CENSUS` | `1` | Off | Arms the census of the places where native blocks end. The `direct-admission-census` build feature compiles in the hooks. |
| `IZARRAVM_DIRECT_LINK_REFUSAL_CENSUS` | `1` | Off | Counts refused block links. Needs the `direct-link-refusal-census` build feature. |
| `IZARRAVM_SMC_CENSUS` | `1` | Off | Counts self-modifying code invalidations. Needs the `smc-census` build feature. |
| `IZARRAVM_SMC_CENSUS_WINDOW` | `<start>,<end>`, in retired instructions. | Whole run | Limits the SMC census to a window. Needs the `smc-census` build feature. |
| `IZARRAVM_RETF_ARITY_CENSUS` | `1` | Off | Counts the targets of far returns. Needs the `retf-arity-census` build feature. |
| `IZARRAVM_DIRECT_CALLOUT_ATTRIBUTION` | `1` | Off | Attributes native call-outs to helpers and ports. Needs the `direct-callout-attribution` build feature. |
| `IZARRAVM_VGA_WIPE_CENSUS` | Any value other than `0` turns it on. | Off | Counts which VGA register writes change the direct-write token of the video map. |
| `IZARRAVM_KATEA_REGION_CENSUS` | `1` | Off | Makes a host-folder volume count its FAT, directory, and free-space sectors. |
| `IZARRAVM_MKII_CENSUS` | Set to any value. | Off | Counts block events of the second recompiler. Needs the `dynarec-mkii` build feature. |
| `IZARRAVM_SHADOW_L1_PROBE` | `1` | Off | Turns on the shadow L1 cache probe. |
| `IZARRAVM_SHADOW_PROBE_SAMPLE` | A positive number. | `64` | Sends every Nth native block entry to the interpreter while the probe is on. |

## Profiling

| Name | Values | Default | Effect |
| --- | --- | --- | --- |
| `IZARRAVM_CPU_PROFILE` | A sample stride, as a number. | Off | Samples the CPU profile every Nth instruction in the bench, boot profile, and `--hdd-folder` runs. |
| `IZARRAVM_CPU_PROFILE_ADDRS` | A file path. | Not set | Writes every sampled address of the CPU profile to a CSV file. |
| `IZARRAVM_MACHINE_PROFILE` | Any value other than empty or `0` turns it on. | Off | Measures the machine phases and leaves the native compiler on. |
| `IZARRAVM_INT13_PROFILE` | `1` | Off | Counts the BIOS fixed-disk calls, their sectors, and their wall time. |
| `IZARRAVM_PHASE_INTERVAL_MS` | A positive number. | Off | Records a phase mark every N guest milliseconds. |
| `IZARRAVM_RUNTIME_PROFILE` | `1` | Off | Prints a runtime profile report as JSON on stderr. |
| `IZARRAVM_RIP_PROFILE` | A file path. Empty and `0` count as not set. | Off | On Windows, samples the instruction pointer of the emulation thread and writes the result to the file. |
| `IZARRAVM_RIP_PROFILE_DELAY_SECS` | A number of seconds. | `0` | On Windows, waits this long before the sampler starts. |

## Recompiler controls

These variables change how the native recompiler admits and emits code. They
are for developers who compare two settings in one binary. Do not set them to
get speed. The defaults are the tuned settings.

Most of the on/off controls in this table stop the process with a message when
the value is not one of the listed spellings. An empty value is listed where it
changes the result.

| Name | Values | Default | Effect |
| --- | --- | --- | --- |
| `IZARRAVM_JIT` | `0` or empty turn it off. Other values turn it on. | On | Controls whether the machine admits native blocks on its own. |
| `IZARRAVM_JIT16` | `0`, `1` | `1` | `0` refuses 16-bit code segments. `1` admits them in ordinary RAM. Each other value has the effect of `1`. |
| `IZARRAVM_JIT16_486` | `0` turns it off. | On | Lets 486 mode compile 16-bit word operands. |
| `IZARRAVM_LANE_FAMILY` | `0` turns it off. | On | Admits the whole `0x81` register-immediate dword group, not only ADD. |
| `IZARRAVM_SMC_LANE_TRIAL` | `0` turns it off. | On | Lets a hot self-modifying code chunk try a compile once for each key in each heat epoch. |
| `IZARRAVM_SMC_LANE_TRIAL_BUDGET` | `1` to `4` | `4` | Sets how many trials one key can use in one heat epoch. |
| `IZARRAVM_DISP_LANES` | `1`/`on`, `0`/`off`, empty is off. | On | Admits the `0x8A` displacement lane family. |
| `IZARRAVM_DISP_STORE_LANES` | `1`/`on`, `0`/`off`, empty is off. | On | Admits the `0x89` and `0x88` stores with a displacement. |
| `IZARRAVM_DISP_LOAD_WIDEN` | `1`/`on`, `0`/`off`, empty is off. | Off | Admits the `0x8B` load with a displacement. |
| `IZARRAVM_DIRECT_RETF_V86` | `v86`, `1`/`on`, `0`/`off`, empty is off. | `v86` | Selects the modes in which far returns and far calls run natively. `v86` is virtual-8086 mode. `1` adds real mode. |
| `IZARRAVM_IMM8_LANES` | `1`/`on`, `0`/`off`, empty is off. | On | Admits the one-byte `0x80` immediate lanes. |
| `IZARRAVM_COUNT_LANES` | `1`/`on`, `0`/`off`, empty is off. | On | Admits the one-byte shift and rotate count lanes. |
| `IZARRAVM_FPU_LOOP_ROWS` | `1`/`on`, `0`/`off`, empty is off. | On | Admits five 32-bit floating-point loop forms. |
| `IZARRAVM_MUL_MEM_ROWS` | `1`/`on`, `0`/`off`, empty is off. | Off | Admits the memory form of `0xF7 /4` MUL. |
| `IZARRAVM_LOOP_ROWS` | `1`/`on`, `0`/`off`, empty is off. | On | Runs `LOOP` (`0xE2`) natively. |
| `IZARRAVM_LOOPCC_ROWS` | `1`/`on`, `0`/`off`, empty is off. | On | Runs `LOOPE` and `LOOPNE` (`0xE1`, `0xE0`) natively. |
| `IZARRAVM_V86_LOOP_ROWS` | `1`/`on`, `0`/`off`, empty is off. | On | Admits instruction forms, such as `POP ES` and `POP DS`, that virtual-8086 loops use. |
| `IZARRAVM_TEST_WORD_ROWS` | `1`/`on`, `0`/`off`, empty is on. | On | Admits `0x85` TEST with 16-bit register operands. |
| `IZARRAVM_WORD_SHIFT_CL_ROWS` | `1`/`on`, `0`/`off`, empty is on. | On | Admits 16-bit register shifts by CL (`0xD3 /4` to `/7`). |
| `IZARRAVM_BYTE_SHIFT_ROWS` | `1`/`on`, `0`/`off`, empty is on. | On | Admits the byte register shift and rotate forms `0xD0` and `0xC0`. |
| `IZARRAVM_GENERIC_CALLOUT` | `1`/`on`, `0`/`off`, empty is on. | On | Admits the unprefixed string instructions (MOVS, CMPS, STOS, LODS, SCAS) as interpreter call-outs. |
| `IZARRAVM_DIRECT_IN_IMM8_CALLOUT` | `1`/`on`, `0`/`off`, empty is on. | On | Admits `IN AL,imm8` as a call-out for any port. |
| `IZARRAVM_OUT_IMM8_ROWS` | `1`/`on`, `0`/`off`, empty is on. | On | Admits `OUT imm8,AL` as a call-out. |
| `IZARRAVM_INT_IMM8_ROWS` | `1`/`on`, `0`/`off`, empty is on. | On | Admits `INT imm8` as a call-out. |
| `IZARRAVM_ROTATE_ROWS` | `1`/`on`, `heat`/`heat_gated`, `0`/`off`, empty is off. | On | Selects how rotate forms run natively. `heat` admits them in hot code only. |
| `IZARRAVM_RETRY_LIFT` | `1`/`on`, `0`/`off`, empty is off. | Off | Lets a parked block with a clearable compile failure be tried again. |
| `IZARRAVM_SEGMENT_RETIRE_GOVERNOR` | `cap`, `1`/`on`, `0`/`off`, empty is off. | `cap` | Selects the limit on retired blocks after a data-segment reject. `cap` uses a limit for each key. |
| `IZARRAVM_CHAIN_ENTRY_CHECK` | `1`/`on`/`chain`, `0`/`off`, empty is on. | On | Makes the block entry check compare only the segments that the block chain needs. |
| `IZARRAVM_LIVE_DATA_SEGMENTS` | `0`, `1`, `2`, empty is `0`. | `0` | At `1` and `2`, real-mode and virtual-8086 blocks read data segments from the CPU state. `2` also links successors. |
| `IZARRAVM_DIRECT_POLL_SKIP` | `1`/`on`/`poll`, `0`/`off`, empty is on. | On | Lets the `IN AL,DX` call-out skip repeated port-poll loops. |
| `IZARRAVM_DIRECT_POLL_SKIP_16` | `1`/`on`, `0`/`off`, empty is on. | On | Lets 16-bit code reach the poll-loop scan. |
| `IZARRAVM_DIRECT_POLL_MIN_ITERATIONS` | A number. | `2` | Sets the lowest iteration count that the poll skip tries. |
| `IZARRAVM_DIRECT_POLL_MAX_RAW` | A number. | A built-in limit below 2^32 | Sets the upper limit of the raw clock count that a poll skip returns. |
| `IZARRAVM_JCC_SHADOW` | `1`/`on`, `0`/`off`, empty is off. | Off | Tests the flags shadow register directly for conditional jumps. |
| `IZARRAVM_DIRECT_HOLD_LOAD_BIAS` | `1`/`on`, `0`/`off`, empty is off. | Off | Keeps the load biases in a host register in integer blocks. |
| `IZARRAVM_DIRECT_ALIGN_TEST_AL` | `1`/`on`, `0`/`off`, empty is off. | Off | Uses `test al,mask` for the alignment test in emitted code. |
| `IZARRAVM_DIRECT_EAGER_FLAGS` | `1`/`on`/`true`, `0`/`off`/`false`, empty is on. | On | Writes the flags straight to the CPU state and not to a lazy flags record. |
| `IZARRAVM_DECODE_CACHE_LINES` | A power of two. | `65536` | Sets the number of lines in the instruction decode cache. A bad value gives the default. |
| `IZARRAVM_DECODE_PACK` | Any value other than `0` turns it on. | Off | Takes the first touch of a decode line from a packed side array. |
| `IZARRAVM_POLL_SKIP_NEG_CACHE` | Empty and `0` turn it off. | On | Turns on the negative cache of the poll skip. |
| `IZARRAVM_JIT_ARENA_MIB` | A number from `8` to `192`. | `128` | Sets the size of the executable memory for native code, in MiB. Other numbers are clamped. |
| `IZARRAVM_JIT_UNWIND` | `0` turns it off. | On | On Windows x86-64, registers unwind data for the native code arena. |
| `IZARRAVM_DYNAREC_MKII` | `1` | Off | Selects the second recompiler. Needs the `dynarec-mkii` build feature. |
| `IZARRAVM_DIRECT_ENTRY_ATTRIBUTION` | `1`/`on`/`full`, `2`/`coarse`, `0`/`off`/empty. | Off | Times the phases of each native entry. Needs the `direct-entry-attribution` build feature. |
| `IZARRAVM_DIRECT_ENTRY_ATTRIBUTION_SAMPLE` | A positive number. | `1` | Times one in N entries. Needs the `direct-entry-attribution` build feature. |
| `IZARRAVM_TEST_ADDRESS_SCRATCH_POISON` | `1` | Off | For tests. Writes a poison value to RDX in emitted address code, so a test finds code that relies on RDX. |

## Device and storage controls

| Name | Values | Default | Effect |
| --- | --- | --- | --- |
| `IZARRAVM_DEVICE_TIMING` | `period`, or a list of `pic`, `dma`, `ata`, `cd`, `fdc`, `kbc`, `sb`, separated by commas. | Empty (no family) | Selects the device families that use period timing. An unknown family stops the process. |
| `IZARRAVM_PIT_BULK_ADVANCE` | `1`/`on`, `0`/`off`, empty is on. | On | Advances the timer in one step, and not one input clock at a time. |
| `IZARRAVM_ATA_POLL_SKIP` | `1`/`on`, `0`/`off`, empty is off. | On | Skips guest time while the guest polls a busy ATA or ATAPI device. |
| `IZARRAVM_ATA_POLL_RUN` | A positive number. | `16` | Sets how many alternate-status reads arm the ATA poll skip. |
| `IZARRAVM_ATA_POLL_FLOOR_US` | A number of microseconds. | `20` | Sets the smallest skip of the ATA poll skip. |
| `IZARRAVM_ATA_POLL_SKIP_DIAG` | `1`/`on`, `0`/`off`, empty is off. | Off | Turns on diagnostics for the ATA poll skip. |
| `IZARRAVM_POLL_SKIP` | Empty and `0` turn it off. | On | Lets the interpreter skip poll loops. It has no effect with the native backend. |
| `IZARRAVM_POLL_SKIP_DIAG` | Any value other than empty or `0` turns it on. | Off | Counts the poll-skip decisions. |
| `IZARRAVM_HDD_CACHE` | `0` turns it off. | On | Controls the host sector cache of the hard disk. It changes host work and never changes guest time. |
| `IZARRAVM_HDD_COMMAND_READ_BATCH` | `0` turns it off. | On | Batches the sector reads of one disk command in the host-folder disk. |
| `IZARRAVM_HDD_READAHEAD` | `0` turns it off. | On | Controls the read-ahead of the host-folder disk. |
| `IZARRAVM_KATEA_INCREMENTAL_CLAIMS` | `0` turns it off. | On | At `0`, the host-folder disk runs the full anti-clobber scan on every pass. |
| `IZARRAVM_KATEA_CLAIMS_VERIFY` | `1` | Off | Computes both the incremental answer and the full scan, and prefers the full scan. It is slow. |
| `IZARRAVM_EXTENDED_RAM_SCREEN` | `0`, `off`, `false`, `no`, or empty turn it off. | On | Lets the bus skip the device-window checks for extended RAM. |
| `IZARRAVM_LAZY_PORT_386` | `1`, `on`, `true`, `yes` turn it on. | Off | Lets 386 mode answer time-derived poll ports without ending the CPU batch. |
| `IZARRAVM_PORT_FATAL` | Hexadecimal ports, separated by commas. | Not set | Makes access to the listed unclaimed ports a fatal error. Other unclaimed ports are counted. |
| `IZARRAVM_DISTIRA_ASYNC` | `0` turns it off. | On | At `0`, the Distira swap command waits for the raster pool and does not overlap it with the guest. |
| `IZARRAVM_DISTIRA_LANES` | A number from `1` to `32`. | By core count | Sets the number of Distira raster lanes. A bad value or `0` gives the default. |
