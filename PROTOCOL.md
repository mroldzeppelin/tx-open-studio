# TONEX Pedal USB Protocol

Unofficial reference for the USB control protocol of the **IK Multimedia TONEX Pedal** (the big floor unit), as implemented and checked against hardware by TX Open Studio.

- Executable spec: [`src/tonex_serial.rs`](src/tonex_serial.rs) (framing, parsing, request builders, tests built from captured frames).
- Parameter tables: [`src/fx.rs`](src/fx.rs).
- MIDI side: [`src/main.rs`](src/main.rs).

Line anchors below are given by function/constant name; they are the stable reference (line numbers drift as the code changes).

Conventions: all bytes hex. `f32` = IEEE-754 single, little-endian. "HW-verified" = confirmed against a real pedal (captured frames or observed behaviour), as stated in the source code or its tests. Anything else comes from community references and is marked as such.

---

## 1. Transport

| Item | Value |
|---|---|
| USB ID | `1963:0068` (vendor 0x1963 = IK Multimedia) |
| Class | CDC-ACM (Linux: `/dev/ttyACM*`). The same cable also exposes a USB-MIDI port (see §7). |
| Line settings | 115200 8N1, DTR and RTS asserted (per Builty's driver; the app sets both) |
| Session start | Host must send **hello**, then **request state**, before the pedal serves preset details (§4.1) |

The app picks `/dev/ttyACM*` ports whose sysfs `idVendor` is `1963` (`candidate_ports`).

## 2. Framing

HDLC-like asynchronous framing, no address/control fields.

```
7E  <stuffed payload>  <stuffed CRC lo>  <stuffed CRC hi>  7E
```

| Rule | Detail |
|---|---|
| Flag | `7E` opens and closes a frame. A flag may close one frame and open the next; empty bodies (`7E 7E`) are skipped. |
| Byte stuffing | `7E` → `7D 5E`, `7D` → `7D 5D` (i.e. `7D, b XOR 20`). Applied to payload **and** CRC bytes. |
| CRC | CRC-16/X.25 (a.k.a. IBM-SDLC): reflected poly `0x8408`, init `0xFFFF`, xorout `0xFFFF`. |
| CRC input | The **unstuffed** payload only (no flags, no CRC). |
| CRC byte order | Little-endian (low byte first). |
| Bad CRC | Frame dropped silently. |

Reference implementation: `crc16`, `build_frame`, `decode_frame` in `src/tonex_serial.rs`.

```rust
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x8408 } else { crc >> 1 };
        }
    }
    !crc
}
```

### Worked examples

Big-pedal hello (payload 13 bytes, CRC `0x4DD0` → wire `D0 4D`):

```
7E  B9 03 00 82 04 00 80 10 01  B9 02 02 10  D0 4D  7E
    |------- header ---------|  |- body --|  |CRC|
```

Test vector from the TONEX ONE hello in vit3k's `protocol.md` (device byte `0B` instead of `10`); unit test `crc_matches_protocol_md_hello`:

```
7E  B9 03 00 82 04 00 80 0B 01  B9 02 02 0B  17 8C  7E      CRC = 0x8C17
```

Stuffing (unit test `frame_roundtrip_with_escapes`): payload `01 7E 7D 02`, CRC `0x1308`:

```
7E  01  7D 5E  7D 5D  02  08 13  7E
```

## 3. Value encoding

Payloads are a tree of tagged values. One value at a time, by its first byte:

| First byte | Meaning | Total length |
|---|---|---|
| `00`–`7F` | Literal integer 0–127 | 1 |
| `80 xx` | **One-byte** extended integer, value `xx` (128–255) | 2 |
| `81 lo hi` | u16 little-endian | 3 |
| `82 lo hi` | u16 little-endian | 3 |
| `88 b0 b1 b2 b3` | f32 little-endian | 5 |
| `B9 n` | Collection of `n` values (struct/list) | 2 + items |
| `BA n` | Collection of `n` values (array; in practice arrays of `88` floats) | 2 + items |
| `BC n <n bytes>` | Raw byte blob of `n` bytes (strings) | 2 + n |
| other | Not seen / unknown; the parser stops | – |

Reference: `value_len`, `read_u16_value`, `collection_items`.

> **Correction to earlier community references.** vit3k's `protocol.md` and Builty's driver treat `80` as a u16 prefix (`80 lo hi`). On the TONEX Pedal that is **wrong**: `80` is followed by exactly **one** byte. Captured on hardware (unit test `extracts_state_preset_captured_layout`), pedal on preset 141 (0-based index 140):
>
> ```
> B9 03 81 01 02 05 10 | B9 02 80 8C 01
>                        |     |     |
>                        |     |     +-- 2nd list item: 01
>                        |     +-------- 80 8C = 140 (one-byte extended)
>                        +-------------- list of 2 items
> ```
>
> Read as u16, `80 8C 01` would give 0x018C = 396 and the 2-item list would be one item short. The size field (`05`) also matches the 5-byte body only with the one-byte reading.

`81` vs `82`: both decode as u16 LE; no semantic difference has been established. In practice `81` appears on message types and `82` on size fields.

## 4. Message structure

Every payload starts with a 3-item header collection, followed by the body:

```
B9 03  <type>  <size>  <third field>  | <body>
```

| Field | Notes |
|---|---|
| `type` | u16 message type, usually `81 lo hi`. Big pedal uses `0x02xx`; TONEX ONE uses `0x03xx` for the same messages (vit3k). |
| `size` | Body length in bytes. Pedal sends it as a literal (`05`, `0A`) or `82 lo hi`; host requests use `82 lo hi`. |
| third field | Pedal frames: literal `10` (state) or `02` (param-change push). Host requests copied from Builty end in `80 10 01` / `80 10 03` — see §9 (open question). |

Message type → handler (`frame_type`, `Session::dispatch`):

| Type | Direction | Meaning |
|---|---|---|
| `0x0000` (`00`) | host → pedal | Hello / "send me message type X" requests |
| `0x0200` | host → pedal | Preset details request |
| `0x0201` | pedal → host | State (reply and unsolicited push) |
| `0x0204` | pedal → host | Preset details (reply and footswitch push) |
| `0x0206` | both | Global config (reply; host write) |
| `0x0207` | both | Single parameter change (pedal push; host write) |

### 4.1 Hello (handshake)

```
host:  B9 03 00 82 04 00 80 10 01 | B9 02 02 10
                                    body: 4 bytes, size 0x0004
```

The pedal answers with a frame (contents not parsed by the app; its arrival is the "this is a TONEX" check). Then send **request state** (§4.2). Timeout used: 1.5 s each.

### 4.2 Request state / state `0x0201`

Request (body names the wanted type `81 01 02` = 0x0201):

```
host:  B9 03 00 82 06 00 80 10 03 | B9 02 81 01 02 10
wire:  7E B9 03 00 82 06 00 80 10 03 B9 02 81 01 02 10 DF A1 7E
```

Reply, also pushed unsolicited at any time (HW-verified captures):

```
B9 03 81 01 02 04 10 | B9 02 09 00          preset 10   (index 9)
B9 03 81 01 02 05 10 | B9 02 80 8C 01       preset 141  (index 140)
       type    size 3rd  list[0] = current preset (0-based), list[1] = unknown (00 / 01 seen)
```

Parser (`extract_state_preset`) walks the 3 header values, skips an optional `B9 n`/`BA n` collection header, and decodes the first value. Builty's layout (index as the first body value, no list) is also accepted.

### 4.3 Preset details request → `0x0204`

```
idx 0..=127:   B9 03 81 00 02 82 06 00 80 10 03 | B9 04 10 01 <idx>    <full>
idx 128..=149: B9 03 81 00 02 82 06 00 80 10 03 | B9 04 10 01 80 <idx> <full>
```

| Field | Value |
|---|---|
| `idx` | Preset index, 0-based (0..149), encoded per §3 |
| `full` | `00` = summary (~2 KB reply); `01` = full dump (~30 KB) |

Note: for idx ≥ 128 the body is 7 bytes but the size field stays `0x0006` (copied verbatim from both references; the pedal accepts it).

Examples (`preset_request`):

```
preset 10 summary:   7E B9 03 81 00 02 82 06 00 80 10 03 B9 04 10 01 09 00 DD 0A 7E
preset 141 summary:  7E B9 03 81 00 02 82 06 00 80 10 03 B9 04 10 01 80 8C 00 84 48 7E
```

The app reads all 150 names by requesting summaries 0..149 in sequence (1 s timeout each, 10 s overall). While a request is pending, the first frame containing the name marker is taken as the reply.

#### Summary reply layout (`0x0204`, `full = 00`)

Header `B9 03 81 04 02 ...`. Elements are located by byte markers, not fixed offsets:

```
B9 03 81 04 02 <size> <3rd> ...
  ... B9 04 B9 02 BC 21 <32 name bytes><1 byte> <strlen>   name group
          BA 01 88 <f32>                                     one float (meaning unknown, §9)
  ... B9 0D <13 string fields>                               metadata block
  ... BA 03                                                  effects collection, 3 arrays:
        BA 6D 88 <f32> × 109                                 [0] preset parameter array (§5)
        BA .. 88 <f32> ...                                   [1] unknown (see §9)
        BA .. 88 <f32> ...                                   [2] unknown
```

(Order of the metadata block relative to the other groups is not asserted by the code; each is found by marker.)

| Element | Marker / rule | Source |
|---|---|---|
| Preset name | 32 bytes after `B9 04 B9 02 BC 21` (`BC 21` = 33-byte blob: 32 name bytes NUL-padded + 1 trailing byte). Decode UTF-8 (lossy), cut at first NUL, trim spaces. | `extract_name` |
| Parameter array | First `BA 03 BA 6D`; then 109 × `88 <f32>`, parameter `i` at offset `i * 5` after the marker | `extract_param_array`, `extract_amp_params` |
| Metadata | `B9 0D` followed by 13 fields, each `B9 02 BC <bufsize> <buf> <strlen>`; text = `buf[..strlen]`, cut at first NUL (bytes after it are garbage). Field 0 author, 1 date, **2 character**, 3 instrument, …, 12 description. Several `B9 0D` candidates may exist; take the first where all 13 fields parse. | `extract_character` |
| Footswitch preset index | `B9 04 00 00 [80] <idx>` — present only in details **pushed** after a preset change on the pedal, absent from replies to host requests | `extract_current_preset` |

On the pedal, the metadata strings are **empty** in practice, even on factory presets; only **character** (e.g. `CLEAN`, `DRIVE`, `STOMP - OVERDRIVE`) is populated. Amp model names are not stored (§9).

Layout sanity check used by the app before trusting the array (guards against a shifted index table, since references disagree, §5): param 12 in 75..600, param 17 in 1000..4000, param 18 ∈ {0, 1}.

#### Unsolicited preset details

When the preset is changed on the pedal (footswitch), it pushes a `0x0204` frame containing the pushed-preset-index marker above plus the same name/array/metadata content. The app only treats a `0x0204` as a push while no request is pending; a push landing during a request is missed.

#### Full dump (`full = 01`) → `0x0203`

As the official editor sends it (Windows USB capture), the header size field is the real body length (`07 00` for idx ≥ 128):

```
B9 03 81 00 02 82 <len LE> 80 10 03 | B9 04 10 01 <idx> 01
```

Reply, type `0x0203`:

```
B9 03 81 03 02 81 <len lo> <len hi> 10 | B9 04 01 00 <idx> ... (body)
```

The body length equals the header's `len` (30358 B on FW 1.6.24; don't hardcode it). Two consecutive reads are byte-identical. The body holds the raw model data (`BC 81 C8 35 …` region) the app does not decode.

**Name buffer**: the 33 bytes right after the **first** `BC 21` following the body start (within its first few dozen bytes): ASCII, NUL-terminated, zero-filled after the name (32 characters max).

### 4.3.1 Preset write (rename) `0x0203`

Verified on hardware (official editor capture + the app's own write tests): the whole dump body, modified, sent back as type `0x0203`:

```
B9 03 81 03 02 82 <len lo> <len hi> 80 10 03 | <body>
```

- Rename = read a fresh full dump, set the name buffer (zero-fill the rest of the 33 bytes), write it back. Reading the dump back afterwards shows the new name and no other byte changed, **if the preset is not the active one**.
- When the target **is** the active preset, the pedal stores the model blob in a different but benign form (~10 KB differ, sounds identical; the official editor shows the same). So a write is verified by comparing the **name buffer only**, never the whole body.
- No acknowledgement frame was seen for the app's writes (the editor saw a `0x0204` push); verify by re-reading the dump. The app ignores `0x0204` pushes for the renamed preset while it reads/writes, and for 3 s after.
- The app validates the dump before patching (type `0x0203`, `len` == body length, body starts `B9 04 01 00 <idx>`, name tag near the start) and saves it to `~/.local/share/tx-open-studio/backups/<UTC YYYYmmdd-HHMMSS>-<preset id>.bin` first; no valid dump or no backup → no write.

Writing edited parameters (saving a preset) would use the same message but is not implemented.

### 4.3.2 Preset select over serial (not used by the app)

Seen in the official editor's traffic; the app selects presets over MIDI (§7):

```
B9 03 81 01 02 82 05 00 80 10 03 | B9 02 <idx> 00
```

### 4.4 Parameter change `0x0207`

Same body format in both directions (pedal push when a knob is turned on the pedal; host write):

```
B9 03 81 07 02 82 0A 00 80 10 03 | B9 04 <section> 00 <idx> 88 <f32 LE>
                                   body = 10 bytes = size 0x000A
```

| `section` | `idx` | Target |
|---|---|---|
| `02` | 0..108 | Preset parameter array (§5). HW-verified for writes and pushes. |
| `03` | `00` | Preset volume (0..10). HW-verified (captured while turning PRESET VOLUME). |

Host write examples (`set_param_request`; unit test `set_param_matches_builty_bytes`):

```
amp enable = 1.0 (idx 18 = 0x12):
7E B9 03 81 07 02 82 0A 00 80 10 03 B9 04 02 00 12 88 00 00 80 3F 95 4A 7E

model gain = 7.5 (idx 20 = 0x14):
7E B9 03 81 07 02 82 0A 00 80 10 03 B9 04 02 00 14 88 00 00 F0 40 DB 29 7E

preset volume = 7.5 (section 03):
7E B9 03 81 07 02 82 0A 00 80 10 03 B9 04 03 00 00 88 00 00 F0 40 78 FA 7E
```

Pedal push captured on hardware while turning PRESET VOLUME (unit test `parses_captured_preset_volume_frames`); note the pedal's literal size and third field:

```
B9 03 81 07 02 0A 02 | B9 04 03 00 00 88 33 33 13 41      9.2
B9 03 81 07 02 0A 02 | B9 04 03 00 00 88 CD CC DC 40      6.9
```

Notes:
- Writes address the currently loaded preset (no preset index in the message).
- Indices ≥ `0x80` would need extended encoding; none exist in the 109-array, and the app rejects them.
- The pedal echoes changes back; the app ignores reports for a value it wrote in the last 600 ms (`ECHO_GUARD`).
- Requires a firmware with IK Editor support (per Builty).

### 4.5 Global config `0x0206`

Pedal-wide settings, not per preset.

Request:

```
host:  B9 03 00 82 06 00 80 10 03 | B9 02 81 06 02 10
wire:  7E B9 03 00 82 06 00 80 10 03 B9 02 81 06 02 10 DA 2D 7E
```

Reply (type `0x0206`; per Builty possibly also pushed). Body:

```
B9 01 B9 02 BA 16  88 <f32> × 22  ... (rest of config)
```

Index map, from a hardware mapping session (FW 1.6.24: each GLOBAL SETUP
item was changed on the pedal while polling this message and diffing the
22 values; see the session notes in the repo history):

| Global idx | Meaning | Values / range | Default |
|---|---|---|---|
| 0 | Exp. pedal calibration, heel (raw) — unverified, deduced from menu position | | 100 |
| 1 | Exp. pedal calibration, toe (raw) — unverified, deduced from menu position | | 4000 |
| 2 | Naming (display format) | 0 Name · 1 PC+Name · 2 Bank+Name | 2 |
| 3 | User mode | 0 Easy · 1 Advanced | 1 |
| 4 | Ext. Ctrl type | 0 TRS exp. pedal · 1 RTS exp. pedal | 0 |
| 5 | Single switch mode | 0 Macro TGL · 1 Tap Tempo · 2 Macro MOM · 3 Tuner | 1 |
| 6 | Dual switch mode | 0 Bank · 1 Preset · 2 Tune Tap | 1 |
| 7 | MIDI channel | 1..16 | 1 |
| 8 | MIDI Thru | 0 off · 1 thru | 0 |
| 9 | Input trim (dB) | -15..15 | 0 |
| 10 | Main (master) volume (dB) | -40..+3 | 0 |
| 11 | Interface (USB) volume (dB) | -40(?)..0 | -10 |
| 12 | Operation mode | 0 Live · 1 Interface | 0 |
| 13 | USB Out | 0 Stereo · 1 Dual | 0 |
| 14 | Not identified (always 0 so far) | | 0 |
| 15 | Global cab sim | 1 Active · 0 Bypass | 1 |
| 16 | Tuner mode | 0 Off · 1 Through · 2 Muted | 0 |
| 17 | Tuner reference (Hz) | ~427..453 | 440 |
| 18 | Tempo (BPM) | one decimal | |
| 19 | Tempo mode | 0 Global · 1 Preset | 0 |
| 20 | MIDI Clock | 0 Off · 1 Master · 2 Slave Mix · 3 Slave USB | 0 |
| 21 | Bypass mode | 1 Enabled · 0 Disabled | 1 |

Caveat observed during the mapping session: the pedal re-enumerates USB
(CDC port disappears and returns) while navigating some GLOBAL SETUP
screens, so a host should expect the serial link to drop around menu use.

Write: there is **no single-global message**. Resend the whole received body with values patched, under a write header with the real body length (`global_write_request`):

```
B9 03 81 06 02 82 <len lo> <len hi> 80 10 03 | <received body from B9 01 B9 02 BA 16 ... to end, patched>
```

Builty sends a stray literal `7E` before the header, a hard-coded size `0x0DF0`, and includes the 2 received CRC bytes in its body; the app does none of that. See §9 on verification status.

## 5. Preset parameter array (109 floats)

Order = Builty `enum TonexParameters` ("defined in the same order as they are sent by the Pedal"), ranges from Builty `tonex_params.c`, as used in `src/tonex_serial.rs` and `src/fx.rs`. Values are stored in display units (dB, ms, %, 0..10, model index) — no scaling.

"HW" column: ✔ = value confirmed on hardware (read/write observed, or present in the captured preset-148 dump with a plausible value); blank = from Builty only. The **148** column shows the value at that index in a real dump of preset 148 (`accepts_hardware_dump_preset_148` in `src/fx.rs`; indices 0..39 only).

### Noise gate, compressor (0–9)

| Idx | Parameter | Range / values | 148 | HW |
|---|---|---|---|---|
| 0 | Gate position | 0 pre / 1 post (default 0) | 0 | |
| 1 | Gate enable | 0/1 | 0 | |
| 2 | Gate threshold | -100..0 dB (default -64) | -100 | ✔ |
| 3 | Gate release | 5..500 ms (default 20) | 20 | |
| 4 | Gate depth | -100..-20 dB (default -60) | -60 | |
| 5 | Comp position | 0 pre / 1 post (default 1) | 1 | |
| 6 | Comp enable | 0/1 | 0 | |
| 7 | Comp threshold | -40..0 dB (default -14) | 0 | ✔ |
| 8 | Comp make-up gain | -30..10 dB (default -12) | -8 | ✔ |
| 9 | Comp attack | 1..51 ms (default 14) | 5 | ✔ |

### Amp block (10–35), including cabinet (23–33)

| Idx | Parameter | Range / values | 148 | HW |
|---|---|---|---|---|
| 10 | EQ position (0 Pre, 1 Post of the amp model) | 0/1 | 1.0 | ✔ (pedal-menu capture) |
| 11 | EQ bass | 0..10 | 4.3 | ✔ (MIDI CC 23 + read) |
| 12 | EQ bass freq | 75..600 Hz | 300 | ✔ (layout check) |
| 13 | EQ mid | 0..10 | 8.1 | ✔ (CC 25) |
| 14 | EQ mid Q | ~0.3..1.6 observed (menu ends unchecked) | 0.7 | ✔ (pedal-menu capture) |
| 15 | EQ mid freq | 150..5000 Hz (ends read off the pedal's own display) | 750 | ✔ |
| 16 | EQ treble | 0..10 | 2.6 | ✔ (CC 28) |
| 17 | EQ treble freq | 1000..4000 Hz | 2000 | ✔ (layout check) |
| 18 | **Amp enable** | 0/1 — serial-only, no MIDI CC | 1 | ✔ (write) |
| 19 | not named in code | | 0 | |
| 20 | Model gain | 0..10 | 5.5 | ✔ (CC 102, write, push) |
| 21 | Model volume | 0..10 | 4.2 | ✔ (CC 103) |
| 22 | not named in code | | 100 | |
| 23 | Builty "CABINET_UNKNOWN" (left alone) | | 1 | |
| 24 | Cab type | 0 Tone Model (captured cab), 1 VIR, 2 Off — see note | 0 | |
| 25 | VIR cab model | integer 0..39: 40 models on FW 1.6.24, the pedal clamps higher indices to the last one. Names in index order (as in the official TONEX software's list, order verified on the pedal at 0, 25 and 39): 16 DK, 18 CHAMP, 110 TONE, 112 DLX, 112 MARK, 112 CALI, 112 MAZ, 112 TINY, 115 BMAN, 212 TWIN, 212 BLUE, 212 H70, 212 JAZZ, 212 OPEN, 212 PPC, 212 REHO, 212 DZ, 412 1960, 412 B30, 412 B8000, 412 B9000, 412 BS, 412 DBAG, 412 HW, 412 F1, 412 T1, 412 V1, 412 M1, 412 PPC, 412 RETS, 412 RKB, 412 RKV, 412 PIG, 412 GRN, 412 XXL, 412 STD, 410 SVX, 115 SVX, 810 SVX, 810 OBC. The app still accepts reads up to 63 and shows them as "Cab N". | 25 | ✔ |
| 26 | VIR resonance | 0..10 | 5 | |
| 27 | Mic 1 type | 0 Condenser, 1 Dynamic, 2 Ribbon | 0 | |
| 28 | Mic 1 X | 0..10 | 1.5 | |
| 29 | Mic 1 Z | 0..10 | 0 | |
| 30 | Mic 2 type | 0 Condenser, 1 Dynamic, 2 Ribbon | 1 | |
| 31 | Mic 2 X | 0..10 (Builty lists 0..2, likely a slip) | 1.5 | |
| 32 | Mic 2 Z | 0..10 | 0 | |
| 33 | Mic blend | -100..100 | 0 | |
| 34 | Model presence | 0..10 | 0 | ✔ (CC 106) |
| 35 | Model depth | 0..10 | 8.4 | ✔ (CC 107) |

Cab type order: Builty's `TonexCabinetTypes` enum and web UI give 0 Tone Model / 1 VIR / 2 Off; Builty's own header comment (and brunothesatellite, reading a different index) claim the reverse.

Index conflict: brunothesatellite's tonexpedal puts amp enable at 17 and cab type at 22; Builty has 18 and 24. Hardware dumps match Builty (17 = 2000 Hz treble freq, 18 = 1).

### Reverb (36–62)

| Idx | Parameter | Range / values |
|---|---|---|
| 36 | Reverb position | 0 pre / 1 post |
| 37 | Reverb enable | 0/1 |
| 38 | Reverb model | 0 Spring 1, 1 Spring 2, 2 Spring 3, 3 Spring 4, 4 Room, 5 Plate |
| 39 + 4·m + 0 | Time | 0..10 |
| 39 + 4·m + 1 | Pre-delay | 0..500 ms |
| 39 + 4·m + 2 | Color | -10..10 |
| 39 + 4·m + 3 | Mix | 0..100 % |

Each model `m` (0..5) keeps its own 4-value block: Spring 1 = 39–42, Spring 2 = 43–46, Spring 3 = 47–50, Spring 4 = 51–54, Room = 55–58, Plate = 59–62. Dump of preset 148: 36 = 1, 37 = 1, 38 = 5 (Plate), 39 = 5.0.

### Modulation (63–93)

| Idx | Parameter | Range / values |
|---|---|---|
| 63 | Mod position | 0 pre / 1 post |
| 64 | Mod enable | 0/1 |
| 65 | Mod model | 0 Chorus, 1 Tremolo, 2 Phaser, 3 Flanger, 4 Rotary |

Per-model blocks (each starts with sync 0/1 and time signature 0..17):

| Model | Block | Sync | Time sig | Knobs |
|---|---|---|---|---|
| Chorus | 66–70 | 66 | 67 | 68 rate 0.1..10, 69 depth 0..100, 70 level 0..10 |
| Tremolo | 71–76 | 71 | 72 | 73 rate 0.1..10, 74 shape 0..10, 75 spread 0..100, 76 level 0..10 |
| Phaser | 77–81 | 77 | 78 | 79 rate 0.1..10, 80 depth 0..100, 81 level 0..10 |
| Flanger | 82–87 | 82 | 83 | 84 rate 0.1..10, 85 depth 0..100, 86 feedback 0..100, 87 level 0..10 |
| Rotary | 88–93 | 88 | 89 | 90 speed 0..400, 91 radius 0..300, 92 spread 0..100, 93 level 0..10 |

### Delay (94–108)

| Idx | Parameter | Range / values |
|---|---|---|
| 94 | Delay position | 0 pre / 1 post |
| 95 | Delay enable | 0/1 |
| 96 | Delay model | 0 Digital, 1 Tape |
| 97 + 6·m + 0 | Sync | 0/1 |
| 97 + 6·m + 1 | Time signature | 0..17 |
| 97 + 6·m + 2 | Time | 0..1000 ms |
| 97 + 6·m + 3 | Feedback | 0..100 % |
| 97 + 6·m + 4 | Mode | 0 normal / 1 ping-pong |
| 97 + 6·m + 5 | Mix | 0..100 % |

Digital = 97–102, Tape = 103–108.

Time signature (delay and modulation), 18 values 0..17: `1/32, 1/32 D, 1/32 T, 1/16, …, 1/1 T` (three variants — plain, dotted, triplet — per note value from 1/32 to 1/1).

All effect parameters (gate, comp, cab, mod, delay, reverb) are written by the app via `0x0207` section `02`; this path is HW-verified (`src/fx.rs` header).

### Outside the array

| Value | Where |
|---|---|
| Preset volume (0..10) | `0x0207` section `03` idx `00`; MIDI CC 12. Not located in the summary reply (§9). |
| Input trim, main volume | Global config `0x0206`, idx 9 / 10 (§4.5) |

## 6. Typical session

```
host  → hello                         (0x0000, body B9 02 02 10)
pedal → hello reply
host  → request state                 (body B9 02 81 01 02 10)
pedal → 0x0201 state                  → current preset index
host  → preset details idx=cur, 00    → name, 109 params, character
host  → request globals               (body B9 02 81 06 02 10)
pedal → 0x0206 globals
host  → preset details 0..149, 00     → all names + characters (~10 s)
...idle...
pedal → 0x0207 pushes                 (knob turned on pedal)
pedal → 0x0201 / 0x0204 pushes        (footswitch preset change)
host  → 0x0207 writes                 (editor changes)
```

Set `TX_DEBUG_SERIAL=1` to make the app log unhandled frames as hex on stderr.

## 7. MIDI control (USB-MIDI port, same cable)

The pedal listens on the channel set in GLOBAL SETUP (global idx 7, §4.5, default 1); the app reads it over serial and sends on it. Source: `src/main.rs` (`send_preset`, `bank_and_pc`, `knob_cc`, `TUNER_CC`).

### Preset select

Presets 1..150 (0-based index `i = preset - 1`):

```
B0 00 <i / 128>      Bank Select MSB (CC0)
C0    <i % 128>      Program Change
```

| Preset | CC0 | PC |
|---|---|---|
| 1 | 0 | 0 |
| 128 | 0 | 127 |
| 129 | 1 | 0 |
| 150 | 1 | 21 |

On-pedal bank/slot: 3 presets per bank, slots A/B/C; the app shows banks 0-based (`bank = (preset-1) / 3`, preset 141 → bank 46 slot C).

### Control Change map

| Control | CC | Values | Status |
|---|---|---|---|
| Model gain | 102 | 0..127 | HW-verified |
| Model volume | 103 | 0..127 | HW-verified |
| Presence | 106 | 0..127 | HW-verified |
| Depth | 107 | 0..127 | HW-verified |
| EQ bass | 23 | 0..127 | HW-verified |
| EQ mid | 25 | 0..127 | HW-verified |
| EQ treble | 28 | 0..127 | HW-verified |
| Preset volume | 12 | 0..127 | per project hardware notes |
| Tuner | 9 | 127 = on, 0 = off | HW-verified |
| Amp enable | — | **no CC exists**; use serial `0x0207` param 18 | |

Knob scaling is linear: `cc = round(v / 10 * 127)` for a 0..10 value (6.9 → 88, 5.0 → 64). Reading back over serial gives the quantized value (CC 88 → 6.929). CC numbers come from ThibaultDucray's TouchOSC template v4.3 (post Nov-2024 firmware MIDI chart), then verified on hardware.

## 8. Device behaviour notes

- Unsolicited frames can arrive at any time; a client must accept `0x0201`, `0x0204`, `0x0206`, `0x0207` interleaved with replies.
- Changes made via MIDI CC are reported back over serial as `0x0207` (the app relies on this for two-way sync).
- Tuner: the pedal sends **no pitch data** over USB, and does **not** report tuner on/off toggled on the pedal itself.
- Preset writes (`0x0203`, §4.3.1) get no acknowledgement frame; the active preset's model data is stored in a different (benign) form after a write.

## 9. Known limitations and open questions

| Topic | Status |
|---|---|
| Preset rename | **Supported**: full dump → patch the name buffer → write (`0x0203`, §4.3.1), verified on hardware. |
| Preset save (parameters) / upload | Not implemented. Saving edits would use the same write message with a modified body, but where the live parameters go in the body is not mapped; no `.tonex` upload path is known. |
| Preset volume at load | Not readable. The summary reply does not contain it in any identified place; the full dump holds a saved copy at unstable offsets inside the model blob. It is only known once the pedal pushes a `0x0207` section `03` change or the host writes it. (The code still range-checks one candidate, element 0 of the 2nd array in `BA 03`, and logs all candidates under `TX_DEBUG_SERIAL=1` — unverified.) |
| "Sections" in `0x0207` | Hypothesis: `<section> 00 <idx>` addresses float arrays in document order (Builty's old preset-tempo write `01 00 01`; `02` = 109-array confirmed; `03` = preset volume confirmed). How `03` maps onto the summary's arrays is not established. |
| Name-group float | `B9 02 [BC 21 …] BA 01 88 <f32>` after the name (from vit3k's capture): meaning unknown; it is **not** the preset volume. |
| Full dump `full = 01` | Reply type `0x0203` (§4.3); the app reads the header, body start and name buffer only. The model blob (~30 KB) is not decoded; offsets inside it vary between presets. |
| Header third field | Host request headers from Builty end in `80 10 01` / `80 10 03`. Under the HW-verified one-byte `80` rule that decodes as `0x10` plus a stray 4th item in a 3-item `B9 03` header (it only made sense when `80` was read as u16: `0x0110` / `0x0310`). The pedal accepts these requests anyway; pedal-originated frames use a literal (`10`, `02`) and no stray byte. Meaning unknown. |
| State `0x0201` list[1] | Second item of the `B9 02` list: `00` and `01` seen; meaning unknown. |
| Hello reply | Content not decoded. |
| Global config | 19 of the 22 floats identified on hardware (see the table in §4.5); idx 14 unknown, idx 0/1 (exp. calibration) deduced but unexercised. The `0x0206` body after the 22 floats is opaque. |
| Amp model names | Not stored on the device: metadata slots are empty even on factory presets; only "character" is populated. |
| VIR cab model names | Resolved: 40 models (0..39) on FW 1.6.24, see §5 param 25. |
| Array indices 10, 14, 15, 19, 22, 23 | Present in dumps, not identified. |
| Tuner | No pitch data and no notification of pedal-side tuner toggles (see §8). Tuner-mode quirk (FW 1.6.24, MIDI-invoked tuner, analog outs): Off shows the tuner screen but mutes, Through passes audio, and Muted unexpectedly also passes audio. |
| Indices ≥ 128 in `0x0207` | Would be extended-encoded; never needed for the 109-array. |
| Preset index ≥ 128 in requests | Body grows to 7 bytes while size stays `0x0006`; accepted by the pedal, correct size unknown. |

## 10. Provenance and credits

Built on earlier community protocol work, corrected against captures on a real TONEX Pedal (big floor unit):

- **Builty / TonexOneController** — `source/main/usb_tonex.c` (big-pedal driver: hello, state, preset details, parameter parsing, param-change, global config), `tonex_params.h` / `tonex_params.c` (parameter order and ranges), `web/index.html` (option lists).
- **vit3k / tonex_controller** — `protocol.md` (framing, CRC, value encoding; written for TONEX ONE).
- **brunothesatellite / tonexpedal** — `index.html` `syncUSB()` (name scan on the big pedal).
- **ThibaultDucray / TonexPedal-TouchOSC-Template** — MIDI CC numbers.

Corrections from hardware: `80` is a one-byte extended value (not u16); state reply layout (`B9 02` list); amp enable at 18 / cab type at 24; VIR model range; preset volume as `0x0207` section `03`; metadata strings empty except character.

TX Open Studio and this document are not affiliated with or endorsed by IK Multimedia. TONEX is a trademark of IK Multimedia. This document is provided for interoperability purposes.
