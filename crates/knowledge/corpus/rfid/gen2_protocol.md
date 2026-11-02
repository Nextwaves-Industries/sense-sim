---
title: EPC Gen2 protocol for passive UHF RFID (ISO 18000-63)
domain: rfid
tags: EPC Gen2, Gen2v2, Gen2v3, ISO 18000-63, RAIN RFID, UHF RFID, Q algorithm, slot count, slotted ALOHA, anti-collision, inventory round, Query, QueryAdjust, QueryRep, ACK, RN16, session, S0, S1, S2, S3, inventoried flag, target A, target B, single target, dual target, SL flag, Select, Tag Focus, memory bank, EPC, TID, user memory, kill password, access password, FM0, Miller, M2, M4, M8, BLF, Tari, dense reader mode, DRM, dense interrogator, read rate, tag population, throughput
aliases: giao thức Gen2, thuật toán Q, phiên S0 S1 S2 S3, cờ inventoried, bộ nhớ thẻ RFID, đọc nhiều thẻ cùng lúc, tốc độ đọc thẻ, chế độ đầu đọc dày đặc, too many tags at once, read rate falls with many tags, tag collisions, how readers separate tags, how to filter tags
verified: 2026-11-02
---
# EPC Gen2 protocol for passive UHF RFID

## EPC Gen2 and ISO 18000-63 scope and version
EPC Gen2 is the GS1 air interface for passive UHF RFID, marketed as RAIN RFID. The current GS1 text checked here is Release 3.0.1, ratified February 2026, titled for 860 MHz to 930 MHz [1]. Vendors describe the same protocol family as "ISO 18000-63 and EPCglobal Gen2v2" [2]. Release 3 adds the inventory commands QueryX and QueryY, which can filter tags during round initialisation, alongside the classic Query [1]. Readers and tags sold today mostly advertise Gen2v2 [2][3], so do not assume QueryX support unless the datasheet says so. Gen2 fixes tag behaviour; it does not set power limits or channel plans, which come from local regulation [1].

## Gen2 inventory round command sequence
A Gen2 inventory round singulates tags one at a time. The reader sends Query (or QueryX) with slot-count parameter Q, and each participating tag loads a random slot counter between 0 and 2^Q - 1 [1]. A tag holding 0 backscatters a 16-bit random number (RN16) immediately. The reader answers with ACK carrying that RN16, and the tag then returns its EPC reply. The next QueryRep or QueryAdjust makes the identified tag invert its inventoried flag and go back to ready, and may bring the next tag into reply [1]. QueryRep decrements counters; QueryAdjust raises or lowers Q and makes tags re-draw a slot. If two tags answer in one slot the RN16s normally collide, tags receive a bad ACK and return to arbitrate, though a reader that resolves one RN16 at waveform level can still ACK that tag [1].

## Gen2 Q algorithm and slot count selection
In the Gen2 Q algorithm, Q is an integer from 0 to 15, so tag reply probability per round start runs from 1 down to 2^-15 = 0.000031 [1]. Annex D of the standard gives an example, informative only: start with Qfp = 4.0, send Q = round(Qfp), then after each slot apply the following [1].

| Slot result | Update to Qfp |
|---|---|
| No reply | Qfp = max(0, Qfp - C) |
| One reply | unchanged |
| More than one reply (collision) | Qfp = min(15, Qfp + C) |

C is typically 0.1 to 0.5, small when Q is large and larger when Q is small [1]. Rule of thumb: slotted ALOHA throughput peaks near 1/e = 36.8% of slots when 2^Q is close to the tag count, so about 100 tags want Q near 7. Readers adapt Q themselves, but a tag-population setting (for example 5 or 300 in Zebra profiles) seeds it [4]. A high setting on a small population wastes empty slots; a low setting on a large population causes collisions.

## Gen2 sessions S0 S1 S2 S3 and flag persistence
Every Gen2 tag keeps four independent inventoried flags, one per session, each valued A or B. A round works in exactly one session, and rounds in one session neither use nor change another session's flag, so two readers can inventory the same tags independently in different sessions [1]. Tags power up with S0 at A; S1, S2 and S3 keep a stored value only within their persistence time [1].

| Flag | While tag is powered | After power is lost |
|---|---|---|
| S0 | indefinite | none, resets to A |
| S1 | 500 ms to 5 s, not refreshed | 500 ms to 5 s |
| S2, S3 | indefinite | more than 2 s |
| SL | indefinite | more than 2 s |

Values are for the nominal temperature range; the extended range is not specified [1]. A tag can therefore stay at B long after it leaves an S2 field, and S1 can drop back to A even while powered [1].

## Choosing a Gen2 session and target by use case
Session choice sets how often an already-read tag answers again. Single target (A only) with S2 or S3 keeps read tags quiet, so weak tags get airtime in big populations. S0, or dual target A/B, repeats reads, which suits small sets and presence monitoring. Zebra's built-in FX reader profiles are a concrete reference [4]:

| Profile | Session, target | Tag population |
|---|---|---|
| Default | S0, A | 100 |
| Maximum data rate | S0, A/B | 300 |
| Dock door with motion sensor | S2, A | 300 |
| Conveyor (25 ms dwell per antenna) | S2, A | 5 |
| Transition point | S2, A | 300 |

To count all stationary tags with S2 or S3, inventory A to B and then B back to A; the standard describes exactly this two-step process [1]. Keonn describes Impinj Tag Focus as S1 target A with chip support (M4 and newer), where read tags stay silent while the field is on and return to A 1-2 s after the field drops [5].

## Gen2 Select command and SL flag filtering
The Gen2 Select command partitions tags before a round using MemBank, Pointer, Length and Mask fields. Matching and non-matching tags then assert or deassert SL, or set an inventoried flag to A or B in one chosen session, according to the Action field [1]. Successive Selects give union and intersection, and the Sel field in Query targets SL, not-SL or all [1]. SL applies across all four sessions; an inventoried flag belongs to one [1]. Use Select when a reader should answer for one product group (an EPC prefix) in a mixed field, or when a write must reach a single known tag. Select matches on memory content, not location, so it does not fix stray reads from nearby lanes: tags of the same group next door still match.

## Gen2 tag memory banks: Reserved, EPC, TID, User
Gen2 tags have four logical memory banks [1].

| Bank | Contents |
|---|---|
| Reserved | Kill password at 00h-1Fh, access password at 20h-3Fh, when implemented |
| EPC | StoredCRC 00h-0Fh, StoredPC 10h-1Fh, EPC code from 20h, optional XPC from 210h |
| TID | ISO/IEC 15963 class identifier plus data identifying the features the tag supports |
| User | Optional application data |

Bank sizes depend on the chip. Impinj M830 has 128 EPC bits and no user memory; M850 has 96 EPC bits and 32 user bits [2]. A longer EPC adds backscatter time per read, so keep it to the size the encoding requires. Do not plan on user memory in a design without confirming the chosen chip has it. Encoding with a write needs about 3 dB more power than reading (see the link budget notes), so write stations should be close to the tag.

## Gen2 backscatter link: FM0, Miller, BLF and data rate
The Gen2 reader sets the tag's backscatter link frequency (BLF) with the DR and TRcal parameters, and picks encoding M in the Query [1]. Tari, the reader's reference interval, runs from 6.25 us to 25 us [1].

| M | Encoding | Tag data rate | Overall range |
|---|---|---|---|
| 1 | FM0 baseband | BLF | 40-640 kbps |
| 2 | Miller | BLF/2 | 5-320 kbps across M2, M4, M8 |
| 4 | Miller | BLF/4 | |
| 8 | Miller | BLF/8 | |

Higher M is slower but more robust. Impinj states the R700's best receive sensitivity comes from Dense Reader M8, and that its 1100 reads per second maximum needs reader mode 0 in an FCC-like region [3]. Zebra FX modes range from 640 kHz FM0 down to 120-160 kHz M2 with Tari up to 25 us, and its default AutoMAC mode picks link settings from the RF environment [4]. Pick fast modes only for a quiet site with short tag dwell; otherwise leave adaptive or dense modes on.

## Dense reader mode and reader-on-tag collisions
Dense reader mode (dense interrogator mode) separates reader carriers from tag replies in frequency. Gen2 Annex G explains that with two or more readers, range and read rate improve when reader transmissions are kept from colliding spectrally with tag responses, and leaves the actual channel plan to local regulation [1]. ETSI EN 302 208 describes tags responding in low-power channels beside the reader's high-power channel, which lets several readers share a channel and reduces intermodulation that disturbs tags [6]. It is the preferred method in the ETSI lower band and mandatory wording in the upper band [6]. Use a subcarrier mode (Miller M2, M4 or M8) wherever more than one reader can hear another. If readers still interfere, stagger them in time: only one active dock door at a time avoids cross-reads and collisions, according to a University of Ottawa survey of portal work [7].

## Gen2 read rate versus tag population sizing
Headline read rates are reader maxima, not per-tag rates. Impinj quotes up to 1100 reads per second for the R700, depending on configuration and tags in view, in mode 0 only [3]. Reads per tag fall roughly as total rate divided by tag count, and collisions take a further share as the population grows. To size a moving read, work it out as: minimum time in field = tags in field / usable unique reads per second, plus margin for retries. Example: 200 tags at 300 unique reads per second need at least 0.67 s of illumination; a pallet at walking pace usually has more, while a fast conveyor may not. Antennas on one reader take turns, so dwell per antenna divides the time available; Zebra's conveyor profile uses 25 ms per antenna [4]. Use S2 or S3 single target for such moving reads so tags already read do not consume slots.

## Applying this in Sense Sim
Sense Sim has no Q, session, target, Select or link-mode parameter. It models RFID throughput only through the profile field `read_rate_hz` (400 for `rfid-reader` and `rfid-portal`, 200 for `rfid-desktop`). Per-tag reads per second are about read_rate_hz / n / (1 + 0.002 n) for n readable tags on that reader, so 100 tags gives 3.3 reads/s each and 500 tags gives 0.4. Those counts include repeat reads, so they resemble S0 or dual target, not unique tags per second.

Use `rtls_run` with `seconds` long enough for the slowest tag, then check `rfid_reads_per_s` and per-tag reads; `rtls_auto_deploy` also reports `rfid_unread`. Each placed reader device is its own 400 reads/s, while a real four-port reader shares one radio, so divide by the antennas sharing a reader before trusting throughput. Set `address` on `rtls_add_device` to give a tag a specific EPC. Give session and Q advice in text, because the simulator cannot test it.

## Sources
1. EPC Radio-Frequency Identity Generation-2 UHF RFID Standard, Release 3.0.1 — GS1, Feb 2026 — https://ref.gs1.org/standards/gen2
2. Impinj M800 Series product brief specification table — Impinj, 2024 — https://www.impinj.com/getmedia/42a028ff-b05f-468e-83af-ddd6534b57f1/Impinj-ProductBrief-M800-Series-SpecTable-120224.pdf
3. Impinj R700 Series RAIN RFID Readers Datasheet v2.0 — Impinj, 2023 — https://www.cisper.com/datasheets/impinj/Impinj_R700_Series_RAIN_RFID_Readers_Datasheet_2023.pdf
4. FX Series RFID Fixed Reader Integration Guide — Zebra Technologies, 2024 — https://www.zebra.com/content/dam/zebra_new_ia/en-us/manuals/rfid/fxseries-ig-en.pdf
5. Tag Focus — Keonn AdvanNet wiki — https://wiki.keonn.com/software/advannet/advanced-features/tag-focus
6. ETSI EN 302 208 V3.3.1 — ETSI, 2020 — https://www.etsi.org/deliver/etsi_en/302200_302299/302208/03.03.01_60/en_302208v030301p.pdf
7. Survey on UHF RFID portals — Borisenko, Li, Bolic, University of Ottawa, 2010 — https://ruor.uottawa.ca/bitstreams/0bf8d884-d6a2-4203-bd06-1e6af5f6cec3/download
