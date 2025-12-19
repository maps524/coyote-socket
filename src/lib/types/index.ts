export interface SerialPortInfo {
  name: string;
  description?: string;
}

export interface BluetoothDevice {
  address: string;
  name?: string;
  rssi?: number;
}

export interface ChannelParams {
  frequency: number;
  frequency_balance: number;
  intensity_balance: number;
}

export interface CoyoteCommand {
  type: 'B0' | 'BF';
  data: number[];
}

export interface TCodeCommand {
  axis: string;
  position: number;
  interval?: number;
}

// Re-export modulation types
export type {
  ParameterSourceType,
  CurveType,
  NoInputBehavior,
  ParameterSource,
  ChannelConfig,
  GeneralSettings
} from './modulation';

export {
  defaultChannelAConfig,
  defaultChannelBConfig,
  defaultGeneralSettings
} from './modulation';