/**
 * T-Code parsing and handling
 * Based on the original Python implementation
 */

export interface TCodeCommand {
  axis: string;
  position: number;
  interval?: number;
}

export interface AxisCommand {
  [key: string]: string;
}

/**
 * Parse T-Code commands from input data
 * Matches the handleCommand function logic from the original Python code
 */
export function parseTCode(data: string): {
  isDeviceInfo: boolean;
  commands: TCodeCommand[];
  deviceInfoType?: string;
} {
  // Check for device info commands
  if (data.includes('D')) {
    if (data.includes('D0')) {
      return { isDeviceInfo: true, commands: [], deviceInfoType: 'version' };
    } else if (data.includes('D1')) {
      return { isDeviceInfo: true, commands: [], deviceInfoType: 'tcode' };
    } else if (data.includes('D2')) {
      return { isDeviceInfo: true, commands: [], deviceInfoType: 'limits' };
    } else if (data.includes('DSTOP')) {
      return { isDeviceInfo: true, commands: [], deviceInfoType: 'stop' };
    }
  }

  // Parse axis commands
  const axisRegex = /(?:(L0|L1|L2|R0|R1|R2)([^\s]*))/g;
  const matches: RegExpMatchArray[] = [];
  let match: RegExpExecArray | null;
  
  while ((match = axisRegex.exec(data)) !== null) {
    matches.push(match);
  }
  
  const commands: TCodeCommand[] = [];

  for (const match of matches) {
    const axis = match[1];
    const commandStr = match[2];
    
    const tcode = parseAxisCommand(axis, commandStr);
    if (tcode) {
      commands.push(tcode);
    }
  }

  return { isDeviceInfo: false, commands };
}

/**
 * Parse individual axis command
 */
function parseAxisCommand(axis: string, commandStr: string): TCodeCommand | null {
  try {
    // Extract position and optional interval
    const positionMatch = commandStr.match(/^(\d+)(?:I(\d+))?$/);
    if (!positionMatch) {
      return null;
    }

    const position = parseInt(positionMatch[1]);
    const interval = positionMatch[2] ? parseInt(positionMatch[2]) : undefined;

    return {
      axis,
      position,
      interval
    };
  } catch (error) {
    console.error('Error parsing axis command:', error);
    return null;
  }
}

/**
 * Get position with ramping support
 * Matches the get_position function from the original Python code
 */
export function getPosition(
  input: string,
  power: number[],
  midpoint: boolean,
  outputLimit: { min: number; max: number }
): number[] {
  try {
    const positionMatch = input.match(/^(\d+)(?:I(\d+))?$/);
    if (!positionMatch) {
      return power;
    }

    const position = parseInt(positionMatch[1]);
    const interval = positionMatch[2] ? parseInt(positionMatch[2]) : undefined;
    const positionLength = positionMatch[1].length;

    if (interval !== undefined) {
      // Handle interval-based ramping
      const currentPwr = power.length > 0 ? power[power.length - 1] : 0;
      
      // Reduce backlog if too long
      if (power.length > 4) {
        const delta = Math.ceil((power[power.length - 1] - power[1]) / 2);
        power = [power[1], power[1] + delta, power[power.length - 1]];
      }

      // Generate ramping intervals
      const intervals = Math.floor(interval / 25);
      const targetPwr = normalizePosition(position, positionLength, midpoint);
      const increment = (targetPwr - currentPwr) / (intervals || 1);

      for (let x = 0; x < intervals; x++) {
        power.push(Math.round(currentPwr + ((x + 1) * increment)));
      }

      return power;
    } else {
      // Regular position command
      const normalizedPower = normalizePosition(position, positionLength, midpoint);
      return [normalizedPower];
    }
  } catch (error) {
    console.error('Error in getPosition:', error);
    return power;
  }
}

/**
 * Normalize position value based on command format
 * Direct mapping: L00000 = 0 (off), L09999 = 200 (max)
 */
function normalizePosition(position: number, positionLength: number, midpoint: boolean): number {
  const maxValue = Math.pow(10, positionLength);

  if (!midpoint) {
    return normalize(position, {
      actual: { lower: 0, upper: maxValue },
      desired: { lower: 0, upper: 200 }
    });
  } else {
    const delta = Math.abs(maxValue / 2 - position);
    return normalize(delta, {
      actual: { lower: 0, upper: maxValue / 2 },
      desired: { lower: 0, upper: 200 }
    });
  }
}

/**
 * Normalize values between ranges
 */
function normalize(
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
 * Generate device info responses
 */
export function generateDeviceInfoResponse(type: string): string {
  switch (type) {
    case 'version':
      return 'v1.0\r\n';
    case 'tcode':
      return 'T-Code v0.3\r\n';
    case 'limits':
      return [
        'L0 0 9999 Up',
        'R0 0 9999 Twist',
        'R1 0 9999 Roll',
        'R2 0 9999 Pitch',
        'V0 0 9999 Vibe1',
        'V1 0 9999 Vibe2',
        'V2 0 9999 Vibe3',
        'V3 0 9999 Vibe4',
        'A0 0 9999 Valve',
        'A1 0 9999 Suck',
        '\r\n'
      ].join('\n');
    default:
      return '';
  }
}