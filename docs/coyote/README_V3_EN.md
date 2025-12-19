# Coyote Pulse Host V3 Protocol (English Translation)

> Original: README_V3.md (Chinese)
> Translated: 2024

---

## Bluetooth Characteristics

| Service UUID | Characteristic UUID | Property | Name | Size (BYTE) | Description |
| :----------: | :-----------------: | :------: | :--: | :---------: | :---------: |
| 0x180C | 0x150A | Write | WRITE | Max 20 bytes | All commands are input through this characteristic |
| 0x180C | 0x150B | Notify | NOTIFY | Max 20 bytes | All response messages are returned through this characteristic |
| 0x180A | 0x1500 | Read/Notify | READ/NOTIFY | 1 byte | Battery level information |

> Base UUID: 0000`xxxx`-0000-1000-8000-00805f9b34fb (replace xxxx with Service/Characteristic UUID)

## Bluetooth Names

Pulse Host 3.0: `47L121000`

Wireless Sensor: `47L120100`

## Basic Principles

The Coyote has two independent pulse generation modules built-in, corresponding to channels A and B respectively.

Each pulse generation module consists of two parts: channel intensity and channel waveform data.

The pulse generation module is controlled by six variables:
1. Channel Intensity
2. Channel Intensity Soft Upper Limit
3. Waveform Frequency
4. Waveform Intensity
5. Frequency Balance Parameter 1
6. Frequency Balance Parameter 2

## Bluetooth Commands

Unlike the V2 protocol, data does not need endian conversion.

### B0 Command

B0 command writes channel intensity changes and channel waveform data. **Command data length is 20 bytes, written every 100ms.** Both channels' data are in the same command.

```
0xB0 (1 byte command HEAD) +
Serial Number (4 bits) +
Intensity Value Interpretation Method (4 bits) +
Channel A Intensity Setting Value (1 byte) +
Channel B Intensity Setting Value (1 byte) +
Channel A Waveform Frequency x4 (4 bytes) +
Channel A Waveform Intensity x4 (4 bytes) +
Channel B Waveform Frequency x4 (4 bytes) +
Channel B Waveform Intensity x4 (4 bytes)
```

#### Serial Number

Serial number range: `0b0000` ~ `0b1111`

If the input data modifies the pulse host's channel intensity, set serial number > 0. The pulse host will respond via B1 message with the same serial number, returning the modified channel intensity through characteristic 0x150B.

If you don't need the pulse host to return the channel intensity feedback, just set the serial number to `0b0000`.

Additionally, to avoid issues, when modifying channel intensity via B0 command with a non-zero serial number, it's recommended to wait for the B1 response from 0x150B with the same serial number before making further channel intensity modifications.

#### Intensity Value Interpretation Method

The 4 bits of intensity value interpretation method are divided into two parts:
- High 2 bits: Channel A interpretation method
- Low 2 bits: Channel B interpretation method

Interpretation methods:
| Value | Meaning |
|-------|---------|
| `0b00` | No change to corresponding channel intensity; the intensity setting value is ignored |
| `0b01` | Relative increase; if Channel A intensity setting is 15, then Channel A intensity increases by 15 |
| `0b10` | Relative decrease; if Channel A intensity setting is 17, then Channel A intensity decreases by 17 |
| `0b11` | Absolute change; if Channel A intensity setting is 32, then Channel A intensity is set to 32 |

**Examples:**

Assuming current pulse host has Channel A intensity = 10, Channel B intensity = 10:

1. Interpretation = `0b0000`, A setting = 5, B setting = 8
   → After B0: A = 10, B = 10 (no change)

2. Interpretation = `0b0100`, A setting = 5, B setting = 8
   → After B0: A = 15, B = 10 (A increased by 5)

3. Interpretation = `0b0010`, A setting = 5, B setting = 8
   → After B0: A = 10, B = 2 (B decreased by 8)

4. Interpretation = `0b0011`, A setting = 5, B setting = 8
   → After B0: A = 10, B = 8 (B set absolutely)

5. Interpretation = `0b0110`, A setting = 5, B setting = 8
   → After B0: A = 15, B = 2 (A increased, B decreased)

6. Interpretation = `0b1101`, A setting = 5, B setting = 8
   → After B0: A = 5, B = 18 (A set absolutely, B increased)

#### Channel Intensity Setting Value

Channel intensity setting value length is 1 byte, valid range: **0 ~ 200**. Values outside this range are treated as 0.

The Coyote host's absolute intensity range for each channel is also **0 ~ 200**.

**Examples:**

Assuming current pulse host has Channel A intensity = 10:

1. Interpretation = `0b0100`, A setting = 195
   → After B0: A = 200 (capped at max)

2. Interpretation = `0b1000`, A setting = 20
   → After B0: A = 0 (capped at min)

3. Interpretation = `0b0100`, A setting = 201
   → After B0: A = 10 (invalid value, no change)

4. Interpretation = `0b1100`, A setting = 201
   → After B0: A = 0 (invalid value treated as 0 for absolute set)

#### Channel Waveform Frequency / Channel Waveform Intensity

- **Waveform Frequency**: 1 byte, range **10 ~ 240**
- **Waveform Intensity**: 1 byte, range **0 ~ 100**

**⚠️ CRITICAL TIMING INFORMATION:**

> In the B0 command, **every 100ms** you must send 4 sets of waveform frequency and intensity for both channels. **Each frequency-intensity pair represents 25ms of waveform output. 4 sets of data represent 100ms of data.**

If any value in a channel's waveform data is outside the valid range, the pulse host will **discard all 4 sets of data for that channel**.

#### Frequency Value Conversion

In your program, you can limit the value range to **10 ~ 1000**, then convert to the waveform frequency to send using this algorithm:

```
Input range: 10 ~ 1000

waveformFrequency = when(inputValue) {
    in 10..100 -> {
        inputValue
    }
    in 101..600 -> {
        (inputValue - 100) / 5 + 100
    }
    in 601..1000 -> {
        (inputValue - 600) / 10 + 200
    }
    else -> {
        10
    }
}
```

**Examples (Channel A waveform data):**

1. Frequency x4 = {10,10,20,30}, Intensity = {0,5,10,50}
   → After B0: Channel A outputs waveform normally

2. Frequency x4 = {10,10,20,30}, Intensity = {0,5,10,**101**}
   → After B0: Channel A **discards all 4 sets**, no waveform output

**Tip:** If you only want to output waveform to a single channel, set at least one invalid value in the other channel's data (e.g., an intensity value > 100), as shown in example 2 above.

---

### BF Command

> ### 🚨 WARNING: BF command takes effect immediately after writing, with no return value. You MUST re-write the BF command to set soft limits every time you reconnect to the device, to prevent unexpected soft limit values! 🚨

BF command writes the pulse host's channel intensity soft upper limit + waveform frequency balance parameter + waveform intensity balance parameter. **Command data length is 7 bytes.**

```
0xBF (1 byte command HEAD) +
AB Channel Intensity Soft Upper Limit (2 bytes) +
AB Channel Waveform Frequency Balance Parameter (2 bytes) +
AB Channel Waveform Intensity Balance Parameter (2 bytes)
```

#### Channel Intensity Soft Upper Limit

The channel intensity soft upper limit restricts the maximum value that the pulse host channel intensity can reach. **This setting is saved even when powered off.** Value range: **0 ~ 200**. Values outside this range will not modify the soft limit.

Example: If you set AB channel soft limits to 150 and 30, then no matter how you modify intensity via the scroll wheel or B0 command:
- Channel A intensity will only be in range **0 ~ 150**
- Channel B intensity will only be in range **0 ~ 30**
- The pulse host channel intensity will **never exceed** the soft limit.

#### Frequency Balance Parameter 1

The waveform frequency balance parameter adjusts the perceived feel of high and low frequency waveforms. **This setting is saved even when powered off.** Value range: **0 ~ 255**.

This parameter controls the relative perceived intensity of different frequency waveforms at a fixed channel intensity. **The larger the value, the stronger the low-frequency waveform impact feels.**

#### Frequency Balance Parameter 2

The waveform intensity balance parameter adjusts the waveform pulse width. **This setting is saved even when powered off.** Value range: **0 ~ 255**.

This parameter controls the relative perceived intensity of different frequency waveforms at a fixed channel intensity. **The larger the value, the stronger the low-frequency waveform stimulation.**

---

## Bluetooth Response Messages

All data callbacks from the pulse host are returned via the 0x180C → 0x150B characteristic Notify. Please bind notify to this characteristic after successfully connecting to the pulse host.

### B1 Message

When the pulse host intensity changes, it will immediately return the current intensity value via B1 message. If the change was caused by a B0 command, the serial number in the returned B1 will match the serial number of the command that caused the change; otherwise, the serial number is 0.

```
0xB1 (1 byte command HEAD) +
Serial Number (1 byte) +
Channel A Current Actual Intensity (1 byte) +
Channel B Current Actual Intensity (1 byte)
```

### BE Message (Deprecated)

---

## More Examples

In summary, unlike V2's separate channel intensity/waveform data commands, V3 combines both channels' intensity and waveform data into a single B0 command. Here are some examples:

> Data = Command HEAD + Serial Number + Intensity Interpretation Method + Channel A Intensity Setting + Channel B Intensity Setting + Channel A Waveform Frequency{x,x,x,x} + Channel A Waveform Intensity{x,x,x,x} + Channel B Waveform Frequency{x,x,x,x} + Channel B Waveform Intensity{x,x,x,x}

### No.1: Don't modify channel intensity, Channel A continuously outputs waveform

```
1→ 0xB0+0b0000+0b0000+0+0+{10,10,10,10}+{0,10,20,30}+{0,0,0,0}+{0,0,0,101}
   (HEX: 0xB00000000A0A0A0A000A141E0000000000000065)

2→ 0xB0+0b0000+0b0000+0+0+{15,15,15,15}+{40,50,60,70}+{0,0,0,0}+{0,0,0,101}
   (HEX: 0xB00000000F0F0F0F28323C460000000000000065)

3→ 0xB0+0b0000+0b0000+0+0+{30,30,30,30}+{80,90,100,100}+{0,0,0,0}+{0,0,0,101}
   (HEX: 0xB00000001E1E1E1E505A64640000000000000065)

4→ 0xB0+0b0000+0b0000+0+0+{40,60,80,100}+{100,90,90,90}+{0,0,0,0}+{0,0,0,101}
   (HEX: 0xB0000000283C5064645A5A5A0000000000000065)
...
```

### No.2: Pulse host current Channel A intensity = 10, Channel A continuously outputs waveform

```
1→ 0xB0+0b0000+0b0100+5+0+{10,10,10,10}+{0,10,20,30}+{0,0,0,0}+{0,0,0,101}
   (HEX: 0xB00405000A0A0A0A000A141E0000000000000065)

2→ 0xB0+0b0000+0b0000+0+0+{15,15,15,15}+{40,50,60,70}+{0,0,0,0}+{0,0,0,101}
   (HEX: 0xB00000000F0F0F0F28323C460000000000000065)

3→ 0xB0+0b0000+0b0000+0+0+{30,30,30,30}+{80,90,100,100}+{0,0,0,0}+{0,0,0,101}
   (HEX: 0xB00000001E1E1E1E505A64640000000000000065)

4→ 0xB0+0b0001+0b0100+10+0+{40,60,80,100}+{100,90,90,90}+{0,0,0,0}+{0,0,0,101}
   (HEX: 0xB0140A00283C5064645A5A5A0000000000000065)
...
```

In step 1: Set Channel A intensity +5, after setting the pulse host Channel A intensity will +5, becoming 15, but won't return the modified intensity via 0x150B because serial number = 0.

In step 4: Set Channel A intensity +10, after setting the pulse host Channel A intensity will +10, becoming 25, and will return Channel A intensity = 25 via 0x150B with serial number = 1.

### No.3: Pulse host current Channel A intensity = 10, Channel A continuously outputs waveform (with wheel interaction)

```
1→ 0xB0+0b0000+0b0000+0+0+{10,10,10,10}+{0,10,20,30}+{0,0,0,0}+{0,0,0,101}
   (HEX: 0xB00000000A0A0A0A000A141E0000000000000065)

2→ 0xB0+0b0000+0b0000+0+0+{15,15,15,15}+{40,50,60,70}+{0,0,0,0}+{0,0,0,101}
   (HEX: 0xB00000000F0F0F0F28323C460000000000000065)

3→ Scroll up once on Channel A scroll wheel then release

4→ 0xB0+0b0000+0b0000+0+0+{30,30,30,30}+{80,90,100,100}+{0,0,0,0}+{0,0,0,101}
   (HEX: 0xB00000001E1E1E1E505A64640000000000000065)

5→ 0xB0+0b0000+0b0000+0+0+{40,60,80,100}+{100,90,90,90}+{0,0,0,0}+{0,0,0,101}
   (HEX: 0xB0000000283C5064645A5A5A0000000000000065)
...
```

In step 3: Scrolling up once on Channel A wheel then releasing causes pulse host Channel A intensity to +1, returning Channel A intensity = 11 via 0x150B with serial number = 0.

### No.4: Don't modify channel intensity, both AB channels continuously output waveform

```
1→ 0xB0+0b0000+0b0000+0+0+{10,10,10,10}+{0,10,20,30}+{10,10,10,10}+{0,0,0,0}
   (HEX: 0xB00000000A0A0A0A000A141E0A0A0A0A00000000)

2→ 0xB0+0b0000+0b0000+0+0+{15,15,15,15}+{40,50,60,70}+{10,10,10,10}+{10,10,10,10}
   (HEX: 0xB00000000F0F0F0F28323C460A0A0A0A0A0A0A0A)

3→ 0xB0+0b0000+0b0000+0+0+{30,30,30,30}+{80,90,100,100}+{10,10,10,10}+{0,0,0,10}
   (HEX: 0xB00000001E1E1E1E505A64640A0A0A0A0000000A)

4→ 0xB0+0b0000+0b0000+0+0+{40,60,80,100}+{0,90,90,90}+{10,10,10,10}+{0,0,0,10}
   (HEX: 0xB0000000283C5064005A5A5A0A0A0A0A0000000A)
...
```

---

## Serial Number and Intensity Input Example (Channel A)

```kotlin
isInputAllowed = true           // Whether intensity input is currently allowed
accumulatedStrengthValueA = 0   // Accumulated unwritten intensity change value for Channel A
deviceStrengthValueA = 0        // Pulse host current Channel A intensity value
orderNo = 0                     // Serial number
inputOrderNo = 0                // Serial number written in B0
strengthParsingMethod = 0b0000  // Intensity value interpretation method
strengthSettingValueA = 0       // Channel A intensity setting value

// Channel A intensity data processing function
fun strengthDataProcessingA(): Unit {
    if (isInputAllowed == true) {
        strengthParsingMethod = if (accumulatedStrengthValueA > 0) {
            0b0100
        } else if (accumulatedStrengthValueA < 0) {
            0b1000
        } else {
            0b0000
        }
        orderNo += 1
        inputOrderNo = orderNo
        isInputAllowed = false
        strengthSettingValueA = abs(accumulatedStrengthValueA)  // Take absolute value
        accumulatedStrengthValueA = 0
    } else {
        orderNo = 0
        strengthParsingMethod = 0b0000
        strengthSettingValueA = 0
    }
}

// Channel A intensity response message processing function
fun strengthDataCallback(returnOrderNo: Int, returnStrengthValueA: Int): Unit {
    // returnOrderNo: returned input serial number
    // returnStrengthValueA: returned pulse host current Channel A intensity

    deviceStrengthValueA = returnStrengthValueA
    if (returnOrderNo == inputOrderNo) {
        isInputAllowed = true
        strengthParsingMethod = 0b0000
        strengthSettingValueA = 0
        inputOrderNo = 0
    }
}

// Set Channel A intensity to 0
fun strengthZero(): Unit {
    strengthParsingMethod = 0b1100
    strengthSettingValueA = 0
    orderNo = 1
    inputOrderNo = orderNo
}
```

### Sequence Example (time-ordered, numbers don't represent specific moments)

```
1  → Press Channel A intensity '+' button
     accumulatedStrengthValueA += 1 (value = 1)

2  → (100ms cycle) B0 preparing to write
     strengthDataProcessingA()
     BLE WRITE 150A (0xB0 + orderNo(0b0001) + strengthParsingMethod(0b0100) + strengthSettingValueA(1) + ...)

3  → (100ms cycle) B0 preparing to write
     strengthDataProcessingA()
     BLE WRITE 150A (0xB0 + orderNo(0b0000) + strengthParsingMethod(0b0000) + strengthSettingValueA(1) + ...)

3  → 150B returns Channel A intensity value
     BLE NOTIFY 150B (0xB1 + returnOrderNo(1) + returnStrengthValueA(1) + ...)
     Returned serial number = 1, returned Channel A intensity = 1
     strengthDataCallback(1, 1)

4  → Press Channel A intensity '+' button
     accumulatedStrengthValueA += 1 (value = 1)

5  → Press Channel A intensity '+' button
     accumulatedStrengthValueA += 1 (value = 2)

6  → Press Channel A intensity '+' button
     accumulatedStrengthValueA += 1 (value = 3)

7  → (100ms cycle) B0 preparing to write
     strengthDataProcessingA()
     BLE WRITE 150A (0xB0 + orderNo(0b0001) + strengthParsingMethod(0b0100) + strengthSettingValueA(3) + ...)

8  → Press Channel A intensity '+' button
     accumulatedStrengthValueA += 1 (value = 1)

9  → (100ms cycle) B0 preparing to write
     strengthDataProcessingA()
     BLE WRITE 150A (0xB0 + orderNo(0b0000) + strengthParsingMethod(0b0000) + strengthSettingValueA(0) + ...)

10 → 150B returns Channel A intensity value
     BLE NOTIFY 150B (0xB1 + returnOrderNo(1) + returnStrengthValueA(4) + ...)
     Returned serial number = 1, returned Channel A intensity = 4
     strengthDataCallback(1, 4)

11 → Press Channel A intensity '+' button
     accumulatedStrengthValueA += 1 (value = 2)

12 → (100ms cycle) B0 preparing to write
     strengthDataProcessingA()
     BLE WRITE 150A (0xB0 + orderNo(0b0001) + strengthParsingMethod(0b0100) + strengthSettingValueA(2) + ...)

13 → Press Channel A intensity '-' button
     accumulatedStrengthValueA -= 1 (value = -1)

14 → 150B returns Channel A intensity value
     BLE NOTIFY 150B (0xB1 + returnOrderNo(1) + returnStrengthValueA(6) + ...)
     Returned serial number = 1, returned Channel A intensity = 6
     strengthDataCallback(1, 6)

15 → (100ms cycle) B0 preparing to write
     strengthDataProcessingA()
     BLE WRITE 150A (0xB0 + orderNo(0b0010) + strengthParsingMethod(0b1000) + strengthSettingValueA(1) + ...)

16 → 150B returns Channel A intensity value
     BLE NOTIFY 150B (0xB1 + returnOrderNo(2) + returnStrengthValueA(5) + ...)
     Returned serial number = 2, returned Channel A intensity = 5
     strengthDataCallback(1, 5)

17 → (100ms cycle) B0 preparing to write
     strengthZero()
     BLE WRITE 150A (0xB0 + orderNo(0b0001) + strengthParsingMethod(0b1100) + strengthSettingValueA(0) + ...)

18 → 150B returns Channel A intensity value
     BLE NOTIFY 150B (0xB1 + returnOrderNo(1) + returnStrengthValueA(0) + ...)
     Returned serial number = 1, returned Channel A intensity = 0
     strengthDataCallback(1, 0)
...
```

---

# Analysis: Is 100ms a Hardware Constraint?

## Evidence from Documentation

### Direct Statement (Line 29)
> "B0 command writes channel intensity changes and channel waveform data, command data length is 20 bytes, **written every 100ms**."

### Waveform Timing (Line 55)
> "In the B0 command, **every 100ms** you must send 4 sets of waveform frequency and intensity for both channels. **Each frequency-intensity pair represents 25ms of waveform output**. 4 sets of data represent 100ms of data."

## Interpretation

| Aspect | Finding | Implication |
|--------|---------|-------------|
| **Protocol Design** | Explicitly designed around 100ms cycles | The 4×25ms structure is intentional |
| **Firmware Expectation** | Device expects data every 100ms | Firmware likely has 100ms processing loop |
| **Waveform Playback** | Each sub-value = 25ms of output | Device buffers and plays back at fixed rate |
| **Not Explicitly Forbidden** | Doc says "written every 100ms" not "must not exceed" | Faster *might* work |

## Likely Hardware Behavior

The device firmware probably:
1. **Buffers** the 4 waveform values when B0 arrives
2. **Plays back** one value every 25ms (internal timer)
3. **Expects** new B0 at ~100ms to refill buffer

### What Happens If We Send Faster?

| Scenario | Likely Result |
|----------|---------------|
| Send at 50ms | Device may discard half the data (buffer overwrite) |
| Send at 25ms | 75% data loss, potential timing glitches |
| Send at 200ms | Buffer underrun, gaps in output |

## Conclusion

**100ms is effectively a hardware constraint** - not because the BLE link can't handle faster, but because:

1. The device firmware is designed around 100ms processing cycles
2. The 4-value buffer represents exactly 100ms of output
3. Sending faster would likely overwrite the buffer before playback completes

### The Real Optimization Path

Since we can't change the 100ms constraint, the optimization is in **how we fill those 4 values**:

1. **Better sampling** of input during the 100ms window
2. **Smarter downsampling** algorithms (as documented in `input-sampling-architecture.md`)
3. **Upstream compensation** - source sends 100ms early

### Referenced Documents

The document mentions V2 protocol for comparison. If you have `README_V2.md` or similar, that might provide additional context on protocol evolution.
