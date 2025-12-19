/**
 * DG-LAB Coyote Protocol Implementation
 * Based on the original Python implementation
 */

export interface CoyoteB0Command {
  interpretationA: number; // 0-3
  interpretationB: number; // 0-3
  intensityA: number; // 0-200
  intensityB: number; // 0-200
  waveformAfrequency: number[]; // 4 bytes
  waveformAintensity: number[]; // 4 bytes
  waveformBfrequency: number[]; // 4 bytes
  waveformBintensity: number[]; // 4 bytes
}

export interface CoyoteBFCommand {
  limitA: number; // 0-200
  limitB: number; // 0-200
  frequencyBalanceA: number; // 0-255
  frequencyBalanceB: number; // 0-255
  intensityBalanceA: number; // 0-255
  intensityBalanceB: number; // 0-255
}

/**
 * Convert period (ms) to frequency index for Coyote device
 * Matches the convertPeriod function from the original Python code
 */
export function convertPeriod(period: number): number {
  if (period >= 5 && period <= 100) {
    return period;
  } else if (period >= 101 && period <= 600) {
    return Math.round((period - 100) / 5 + 100);
  } else if (period >= 601 && period <= 1000) {
    return Math.round((period - 600) / 10 + 200);
  } else {
    throw new Error("Channel waveform frequency out of bounds.");
  }
}

/**
 * Normalize values between different ranges
 * Matches the normalize function from the original Python code
 */
export function normalize(
  x: number,
  bounds: {
    actual: { lower: number; upper: number };
    desired: { lower: number; upper: number };
  }
): number {
  return Math.ceil(
    bounds.desired.lower +
      ((x - bounds.actual.lower) *
        (bounds.desired.upper - bounds.desired.lower)) /
        (bounds.actual.upper - bounds.actual.lower)
  );
}

/**
 * Get output value scaled to range limits
 * Matches the get_output function from the original Python code
 */
export function getOutput(entry: number, outputLimit: { min: number; max: number }): number {
  return normalize(entry, {
    actual: { lower: 0, upper: 200 },
    desired: { 
      lower: (outputLimit.min * 200) / 200, 
      upper: (outputLimit.max * 200) / 200 
    }
  });
}

/**
 * Generate 0xB0 command bytes for Coyote device
 */
export function generateB0Command(command: CoyoteB0Command): Uint8Array {
  const buffer = new ArrayBuffer(21); // 1 + 1 + 1 + 1 + 4 + 4 + 4 + 4 = 20 bytes + 1 for head
  const view = new DataView(buffer);
  let offset = 0;

  // 0xB0 command head
  view.setUint8(offset++, 0xB0);

  // Serial number (4 bits) + interpretation methods (4 bits each)
  const serialAndInterpretation = 
    (0 << 4) | // serial number (0000)
    (command.interpretationA << 2) |
    command.interpretationB;
  view.setUint8(offset++, serialAndInterpretation);

  // Intensity values
  view.setUint8(offset++, command.intensityA);
  view.setUint8(offset++, command.intensityB);

  // Waveform data (4 bytes each)
  for (let i = 0; i < 4; i++) {
    view.setUint8(offset++, command.waveformAfrequency[i] || 0);
  }
  for (let i = 0; i < 4; i++) {
    view.setUint8(offset++, command.waveformAintensity[i] || 0);
  }
  for (let i = 0; i < 4; i++) {
    view.setUint8(offset++, command.waveformBfrequency[i] || 0);
  }
  for (let i = 0; i < 4; i++) {
    view.setUint8(offset++, command.waveformBintensity[i] || 0);
  }

  return new Uint8Array(buffer);
}

/**
 * Generate 0xBF command bytes for Coyote device
 */
export function generateBFCommand(command: CoyoteBFCommand): Uint8Array {
  const buffer = new ArrayBuffer(7); // 1 + 2 + 2 + 2 = 7 bytes
  const view = new DataView(buffer);
  let offset = 0;

  // 0xBF command head
  view.setUint8(offset++, 0xBF);

  // Limits
  view.setUint8(offset++, command.limitA);
  view.setUint8(offset++, command.limitB);

  // Frequency balance
  view.setUint8(offset++, command.frequencyBalanceA);
  view.setUint8(offset++, command.frequencyBalanceB);

  // Intensity balance
  view.setUint8(offset++, command.intensityBalanceA);
  view.setUint8(offset++, command.intensityBalanceB);

  return new Uint8Array(buffer);
}