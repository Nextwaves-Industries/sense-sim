---
title: UWB PHY and standards (IEEE 802.15.4a/z, HRP, LRP, FiRa)
domain: uwb
tags: uwb, ultra-wideband, ieee 802.15.4a, 802.15.4z, hrp, lrp, bprf, hprf, sts, scrambled timestamp sequence, fira, channel 5, channel 9, 6489.6 mhz, 7987.2 mhz, 499.2 mhz, prf, preamble, sfd, data rate, 6.81 mbps, 850 kbps, air time, frame duration
aliases: siêu băng rộng, băng siêu rộng, chuẩn UWB, kênh 5, kênh 9, tần số UWB, tốc độ dữ liệu UWB, độ dài preamble, thời gian phát khung, ultra wide band standard, impulse radio
verified: 2026-11-02
---
# UWB PHY and standards

## HRP versus LRP UWB physical layers
IEEE 802.15.4 defines two UWB PHY families. Both are held to the same mean spectral density limit, so HRP (high-rate pulse) sends many weak pulses and LRP (low-rate pulse) sends fewer, stronger ones [1].

| Property | HRP UWB | LRP UWB |
|---|---|---|
| Origin | 802.15.4-2011 HRP, extended by 802.15.4z | 802.15.4f-2012, extended by 802.15.4z |
| Peak pulse rate | 499.2 MHz | 2 MHz, 4 MHz added by 15.4z |
| Data rates, base standard | 110 kbps, 850 kbps, 6.81 Mbps | 31.25 kbps, 250 kbps, 1 Mbps |
| Data rates added by 15.4z | 27.24 Mbps | 2, 3, 4, 5, 6, 8, 10 Mbps |
| Example chips | Qorvo DW1000 and DW3000, NXP SR040/SR150, Apple U1 | 3db Access 3DB6830, Zebra UWB chip |

Guidance: treat HRP as the default for indoor positioning. In the 2022 chip survey only the 3db Access and Zebra parts used LRP, and HRP and LRP devices cannot interoperate [1].

## HRP UWB channel plan: channel 5 and channel 9
The HRP PHY defines 16 channels; two carry almost all current positioning products. Band edges below are computed as centre frequency plus or minus half the bandwidth.

| Channel | Centre | Bandwidth | Occupied band (computed) | Wavelength (computed) |
|---|---|---|---|---|
| 5 | 6489.6 MHz | 499.2 MHz | 6240.0 to 6739.2 MHz | 46.2 mm |
| 9 | 7987.2 MHz | 499.2 MHz | 7737.6 to 8236.8 MHz | 37.5 mm |

Centre frequency and bandwidth come from the IEEE table as reproduced in [1] and the DW3000 datasheet [2]. Every chip in the 2022 survey supported channel 5, and all except the DW1000 also supported channel 9 [1].

Free-space path loss is 1.8 dB higher on channel 9 at the same distance (computed: 20 log10(7987.2/6489.6)). The DW3000 sensitivity is also about 1 dB worse on channel 9 [2]. Channel choice is often forced by regulation, not link budget: check the region first, because Vietnam's licence-exempt list and China's rules since 2025 cover channel 9 but not channel 5 (see UWB regulation).

## HRP pulse repetition frequency: BPRF and HPRF modes
The base 802.15.4 HRP PHY defines mean pulse repetition frequencies of 4, 16 and 64 MHz. 802.15.4z drops 4 and 16 MHz and adds 128 and 256 MHz. The 64 MHz option keeps the older BPM-BPSK modulation and is named BPRF (Base PRF). The higher rates use BPSK only and are named HPRF (High PRF) [1].

Compatibility rules an engineer should act on:
- HPRF is optional in 802.15.4z and not every chip supports it, so BPRF is the common interoperability denominator [1].
- The DW3000 family implements BPRF only, plus legacy 802.15.4-2011 frames at 16 and 64 MHz PRF. It is backward compatible with DW1000 only on channel 5 at 6.8 Mbps and 850 kbps [2].
- A DW1000 tag therefore cannot join a channel 9 DW3000 network. Mixed fleets must run channel 5, which is a problem wherever channel 5 is not permitted.

## Scrambled Timestamp Sequence (STS) and secure ranging
The Scrambled Timestamp Sequence (STS) is the main security addition in 802.15.4z. It is a cryptographically generated pulse sequence placed after the start frame delimiter (SFD), where it can sit before or after the payload or stand alone in a no-data frame. Because the sequence has no periodicity, it gives the receiver a clean channel estimate and makes a timestamp much harder to spoof. Both ends must share the keys, and the standard leaves key distribution to higher layers [1].

STS costs link budget and air time. On the DW3000, sensitivity at 6.8 Mbps with a 64-symbol preamble and 64-symbol STS is -94.3 dBm on channel 5 and -93.2 dBm on channel 9, versus -100 and -99.3 dBm at 850 kbps with a 1024-symbol preamble and no STS [2].

Guidance: asset-tracking RTLS rarely needs STS. Specify it when location grants access: doors, vehicles or payment.

## FiRa, CCC and Apple profiles on top of 802.15.4z
IEEE 802.15.4z defines only the PHY and parts of the MAC, so interoperability comes from profiles built on HRP. FiRa, the Car Connectivity Consortium (CCC, Digital Key 3.0) and Apple Nearby Interaction each define their own choices of parameters, discovery and setup. The compared standards use overlapping but not identical PHY options, so two 15.4z chips are not automatically compatible [1].

FiRa targets ranging between devices such as phones and accessories. Infrastructure RTLS with fixed anchors and blink tags usually runs a vendor protocol such as uplink TDoA (rule of thumb). Being FiRa-certified does not imply a chip supports a given RTLS vendor's network. NXP states that its SR040 and SR150 are FiRa certified [3][4].

## UWB frame structure and air-time calculation
An HRP frame is a preamble (SYNC), an SFD, a PHY header (PHR) and a payload, with an optional STS. Air time is the sum of symbol count times symbol duration. DW3000 symbol durations at 64 MHz PRF are 1017.63 ns for SHR symbols, 128.21 ns per PHR symbol or data bit at 6.81 Mbps, and 1025.64 ns at 850 kbps [2].

Air time for a 12-byte payload (computed unless noted):

| Configuration | Preamble + SFD | PHR + data | Total |
|---|---|---|---|
| 6.81 Mbps, 64-symbol preamble | 73 us | 16 to 21 us | about 90 to 95 us |
| 6.81 Mbps, 128-symbol preamble | 138 us | 16 to 21 us | about 151 us (datasheet profile: 135 + 16 us) |
| 850 kbps, 1024-symbol preamble | 1050 us | 169 us | about 1.2 ms |

The DW3000 datasheet quotes 170 us per frame in its DS-TWR profile with embedded data [2]. The 8-fold air-time gap between short and long preambles trades directly against sensitivity and range, and sets network capacity.

## Timestamp resolution and multipath resolution of UWB
DW1000 and DW3000 class radios timestamp with a resolution of about 15.65 ps, derived from a 63.8976 MHz clock upscaled to about 64 GHz. That equals roughly 4.7 mm of light travel (computed; 15 ps is 4.5 mm in [6]) [5]. Timestamp resolution is not ranging accuracy.

Accuracy is limited by bandwidth and multipath. A 499.2 MHz channel gives a time resolution near 2 ns, about 0.6 m (computed as c divided by bandwidth). Paths that differ by less than that merge in the channel estimate. This is why LOS ranging reaches centimetres but indirect paths produce decimetres to metres of bias.

Datasheet ranging performance for the DW3000 is about plus or minus 6 cm in line of sight after calibration, about plus or minus 15 cm without, and 1.5 cm standard deviation measured at -85 dBm [2].

## Applying this in Sense Sim
The built-in UWB profiles `uwb-anchor`, `uwb-tag` and `uwb-tag-tdoa` are listed by `rtls_list_profiles`. The anchor profile models channel 5 (`freq_mhz` 6489.6), and transmit power is expressed as `uwb_tx_dbm_mhz` over a 499.2 MHz channel, so -41.3 dBm/MHz is -14.3 dBm total.

- To model channel 9, add a profile JSON with `freq_mhz` 7987.2 to a `devices` folder scanned at start-up (`%APPDATA%\SenseSim\devices`, or `devices` next to the executable), then pass its id as `profile` to `rtls_add_device`, `rtls_update_device` or `rtls_optimize_anchors`. With the default sensitivity the computed link range falls from 40 m to about 32 m.
- `sensitivity_dbm` defaults to -95, close to the DW3000 6.8 Mbps short-preamble figure (-94.3 dBm). For a long-preamble 850 kbps design use about -100 dBm and raise `max_range_m`, since the range is capped by it.
- The simulator does not model PRF, preamble, STS or data rate directly. Express their effect through `sensitivity_dbm`, and estimate air time by hand.

## Sources
1. An Overview of UWB Standards and Organizations (IEEE 802.15.4, FiRa, Apple): Interoperability Aspects and Future Research Directions, D. Coppens et al. — IEEE Access, 2022 — https://arxiv.org/abs/2202.02190
2. DW3000 Datasheet, version 1.1 — Decawave (Qorvo), 2020 — https://www.mouser.com/datasheet/2/412/DW3000_Data_Sheet-3532945.pdf
3. Trimension SR040 product page — NXP, checked 2026-11-02 — https://www.nxp.com/products/wireless/secure-ultra-wideband-uwb/trimension-sr040-reliable-uwb-solution-for-iot:SR040
4. Trimension SR150 product page — NXP, checked 2026-11-02 — https://nxp.com/products/SR150
5. Robust and Scalable Techniques for TWR and TDoA based localization using Ultra Wide Band Radios, R. Ramesh et al. — arXiv, 2020 — https://arxiv.org/abs/2008.04248
6. Decawave UWB clock drift correction and power self-calibration, J. Sidorenko et al. — arXiv, 2019 — https://arxiv.org/abs/1902.11085
