# Coyote Pulse Host V2 Protocol (English Translation)

> Original: README_V2.md (Chinese)
> Translated: 2024

---

## Bluetooth Characteristics

| Service UUID | Characteristic UUID | Property | Name | Size (BYTE) |
| :----------: | :-----------------: | :------: | :--: | :---------: |
| 0x180A | 0x1500 | Read/Notify | Battery_Level | 1 byte |
| 0x180B | 0x1504 | Read/Write/Notify | PWM_AB2 | 3 bytes |
| 0x180B | 0x1505 | Read/Write | PWM_A34 | 3 bytes |
| 0x180B | 0x1506 | Read/Write | PWM_B34 | 3 bytes |

| Name | Description | Data Definition |
| :--: | :---------: | :-------------: |
| Battery_Level | Device current battery | 1 byte (integer 0-100) |
| PWM_AB2 | AB channel intensity | Bits 23-22 (reserved), Bits 21-11 (A channel actual intensity), Bits 10-0 (B channel actual intensity) |
| PWM_A34 | **B channel** waveform data | Bits 23-20 (reserved), Bits 19-15 (Az), Bits 14-5 (Ay), Bits 4-0 (Ax) |
| PWM_B34 | **A channel** waveform data | Bits 23-20 (reserved), Bits 19-15 (Bz), Bits 14-5 (By), Bits 4-0 (Bx) |

> **Note:** The channel labels appear swapped in the original documentation (PWM_A34 controls B channel, PWM_B34 controls A channel). This is also noted in the Python code as "channels switched".

## Bluetooth Name

Pulse Host 2.0: `D-LAB ESTIM01`

> Base UUID: 955A`xxxx`-0FE2-F5AA-A094-84B8D4F3E8AD (replace xxxx with service UUID)

## Basic Principles

The Coyote has two independent pulse generation modules built-in, corresponding to channels A and B respectively. Each pulse generation module consists of two parts: a power module and a waveform control module. We control the pulse generation module through four variables in the Bluetooth protocol: **S, X, Y, Z**.

## Power Module (S)

> S: PWM_AB2 characteristic

The power module controls the pulse voltage, which is the channel intensity (the number inside the circle in the App interface). This corresponds to parameter **S** in the Bluetooth protocol, with a range of **0-2047** (cannot exceed 2047).

In our APP, each intensity point increase adds 7 (the actual intensity value set in the pulse host is 7 times the value displayed in the APP).

When we write different values of parameter S to the APP, the channel intensity will change immediately and remain constant.

## Waveform Control Module (X, Y, Z)

> X: 5 bits of data at bits 4-0 in PWM_A34 or PWM_B34
> Y: 10 bits of data at bits 14-5 in PWM_A34 or PWM_B34
> Z: 5 bits of data at bits 19-15 in PWM_A34 or PWM_B34

The waveform control module controls the pattern of pulse occurrence and changes in pulse width. The pulse pattern and pulse width changes are saved in the form of built-in waveforms or custom waveforms.

## Pulse Pattern Control

The Coyote's program divides each second into 1000 milliseconds, and a pulse can be generated within each millisecond. We use parameters **X** and **Y** in the Bluetooth protocol to encode the pulse pattern:

- **X**: Represents sending X pulses continuously for X milliseconds
- **Y**: Represents waiting Y milliseconds after X pulses before sending another X pulses (and repeating)

| Parameter | Range |
|-----------|-------|
| X | 0-31 |
| Y | 0-1023 |

### Examples

- Parameters **[1, 9]**: Send 1 pulse every 9ms, total time 10ms, pulse frequency = **100Hz**

- Parameters **[5, 95]**: Send 5 pulses every 95ms, total time 100ms. Since these 5 pulses are continuous and only last 5ms, the user will only feel one (five-in-one) pulse. Therefore, the perceived pulse frequency = **10Hz**

## Frequency Value

```
Frequency = X + Y
Actual pulse frequency = Frequency / 1000
```

This is a characteristic value representing the relationship between X and Y values. You can calculate the most suitable X and Y values by setting the Frequency value. Range: **10-1000**

### Optimal X, Y Ratio Formula

```
X = ((Frequency / 1000) ^ 0.5) * 15
Y = Frequency - X
```

This produces the best effect.

If the X:Y ratio is greater than 1:9 (e.g., [8, 2]), the overall feel of the waveform will become weaker.

## Pulse Width Control

A single pulse consists of two symmetrical positive and negative unipolar pulses. The height (voltage) of the two unipolar pulses is determined by the channel's intensity. We control the strength of the pulse sensation by controlling the pulse width. **Wider pulses = stronger sensation, narrower pulses = weaker sensation.** Rhythmic changes in pulse width can create different pulse sensations.

Pulse width is controlled by parameter **Z**, range: **0-31**

**Actual pulse width = Z × 5μs**

Example: When Z = 20, pulse width = 5 × 20μs = **100μs**

> **Tip:** When pulse width is greater than 100μs (Z > 20), pulses are more likely to cause stinging sensations.

## Creating Continuously Changing Waveforms

Since waveform parameters are not fixed but constantly changing, in the Coyote's design, **each set of [X, Y, Z] parameters is only valid for 0.1 seconds**.

This means every time you write a set of [X, Y, Z] parameters to the device, the device will output the corresponding 0.1s waveform and then **stop output**.

**If you need the waveform to maintain 100Hz frequency, 100μs width, and continuous output, you need to send parameters [1, 9, 20] to the device every 0.1 seconds.**

> **Tip:** You can also modify the Frequency value every 0.1 seconds and use the Frequency formula to automatically generate X and Y values.

## More Examples

### Accelerating Frequency Waveform
To create a waveform with constantly increasing frequency, try sending the following data in sequence every 0.1 seconds:

```
[5,135,20] [5,125,20] [5,115,20] [5,105,20] [5,95,20]
[4,86,20]  [4,76,20]  [4,66,20]  [3,57,20]  [3,47,20]
[3,37,20]  [2,28,20]  [2,18,20]  [1,14,20]  [1,9,20]
```

### Alternating Frequency Waveform
To create a waveform that constantly switches between two frequencies, try sending the following data in sequence every 0.1 seconds:

```
[5,95,20] [5,95,20] [5,95,20] [5,95,20] [5,95,20]
[1,9,20]  [1,9,20]  [1,9,20]  [1,9,20]  [1,9,20]
```

### "Thrust" Sensation Waveform
To create a waveform with constant frequency but a "thrust" sensation, try sending the following data in sequence every 0.1 seconds:

```
[1,9,4]  [1,9,8]  [1,9,12] [1,9,16] [1,9,18]
[1,9,19] [1,9,20] [1,9,0]  [1,9,0]  [1,9,0]
```

> **Tip:** The human body perceives frequency changes slowly, so if frequency changes too quickly, it won't create a sense of rhythm. However, frequent changes in pulse width can create diverse sensations.

---

# V2 vs V3 Protocol Comparison

## Key Differences

| Aspect | V2 | V3 |
|--------|----|----|
| **Bluetooth Name** | D-LAB ESTIM01 | 47L121000 |
| **Base UUID** | 955Axxxx-0FE2-F5AA-A094-84B8D4F3E8AD | 0000xxxx-0000-1000-8000-00805f9b34fb |
| **Command Structure** | Separate characteristics (PWM_AB2, PWM_A34, PWM_B34) | Combined B0 command (20 bytes) |
| **Intensity Range** | 0-2047 (11 bits) | 0-200 (1 byte) |
| **Intensity Scaling** | App value × 7 | Direct value |
| **Update Rate** | 100ms (0.1s) | 100ms |
| **Sub-values per Update** | **1** set of X,Y,Z | **4** sets of Freq,Int |
| **Waveform Control** | X (pulse count), Y (gap), Z (width) | Frequency (encoded), Intensity (%) |
| **Channel Swap Bug** | Yes (documented in code) | Fixed (but verify) |

## Critical Finding: Sub-value Resolution

### V2: 1 Value per 100ms
```
T=0ms:    Send [X,Y,Z] → Device plays for 100ms
T=100ms:  Send [X,Y,Z] → Device plays for 100ms
T=200ms:  Send [X,Y,Z] → Device plays for 100ms
```
**Resolution: 100ms** - You can only change parameters once per 100ms window.

### V3: 4 Values per 100ms
```
T=0ms:    Send B0 with [F0,I0], [F1,I1], [F2,I2], [F3,I3]
          Device plays:
            0-25ms:   F0, I0
            25-50ms:  F1, I1
            50-75ms:  F2, I2
            75-100ms: F3, I3
T=100ms:  Send next B0...
```
**Resolution: 25ms** - V3 gives **4× better temporal resolution** within the same 100ms window!

## Why V3 is Better for Dynamic Content

| Use Case | V2 Limitation | V3 Advantage |
|----------|---------------|--------------|
| **Ramping** | Stair-step jumps every 100ms | Smooth 25ms micro-ramps |
| **Impact effects** | 100ms minimum duration | 25ms precision for sharp hits |
| **Rhythm sync** | Limited to 10Hz pattern changes | 40Hz pattern changes possible |
| **Following fast input** | Loses detail between updates | 4 samples capture more detail |

## Waveform Parameter Translation

### V2 X,Y → V3 Frequency

V2's Frequency value (X+Y, range 10-1000) maps to V3's encoded frequency:

```
V2 Frequency (10-1000) → V3 Period (ms) → V3 Encoded Value (10-240)

V2 Frequency = X + Y
V3 Period = V2 Frequency (they're equivalent in ms)
V3 Encoded = convertPeriod(V2 Frequency)
```

### V2 Z → V3 Intensity Balance?

V2's Z parameter (pulse width, 0-31, actual width = Z×5μs) doesn't have a direct V3 equivalent in the B0 command. However, V3's **Intensity Balance** in the BF command serves a similar perceptual purpose.

| V2 | V3 | Effect |
|----|-----|--------|
| Z (pulse width) | Intensity Balance (BF command) | Controls pulse "character" |
| Higher Z = wider pulse = stronger | Higher value = stronger low-freq feel | Similar perceptual outcome |

## Migration Notes

If porting V2 waveforms to V3:

1. **Frequency**: Use same value, apply `convertPeriod()` encoding
2. **Intensity**: Scale from 0-2047 to 0-200 (divide by ~10.2)
3. **Pulse Width (Z)**: Convert to Intensity Balance setting (experimentation needed)
4. **Timing**: You now have 4 sub-slots per update - use them for smoother transitions!

## Conclusion

**V3 is objectively better** for our use case because:
1. Same 100ms constraint, but 4× the resolution within that window
2. Combined command structure is simpler to work with
3. Balance parameters give perceptual control without complex X,Y,Z math
4. The 4-value system enables the sampling/downsampling architecture we designed
