use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use std::collections::HashMap;
use std::error::Error;
use std::time::Duration;
use tokio::time;
use uuid::Uuid;

// DG-LAB Coyote UUIDs (from protocol documentation)
// Write characteristic for B0/BF commands
const INSTRUCTION_CHAR_UUID: Uuid = Uuid::from_u128(0x0000150a_0000_1000_8000_00805f9b34fb);
// Battery level characteristic
const BATTERY_CHAR_UUID: Uuid = Uuid::from_u128(0x00001500_0000_1000_8000_00805f9b34fb);

// Advertised primary service UUIDs — used at scan time (before connecting) to
// map a device to a friendly product label. Coyote 3.0 advertises 0x180C;
// Coyote 2.0 advertises the vendor base service 955a180b-….
const V3_SERVICE_UUID: Uuid = Uuid::from_u128(0x0000180c_0000_1000_8000_00805f9b34fb);
const V2_SERVICE_UUID: Uuid = Uuid::from_u128(0x955a180b_0fe2_f5aa_a094_84b8d4f3e8ad);

/// True if an advertised local_name looks like a DG-LAB / Coyote device.
fn is_dglab(name: &str) -> bool {
    name.contains("DG-LAB")
        || name.contains("COYOTE")
        || name.contains("47L")
        || name.contains("ESTIM01")
}

/// Map a Coyote's advertised name + services to a friendly product label.
/// The advertised `local_name` for a Coyote 3.0 is a bare numeric serial
/// (e.g. "47L1210…"), which reads as a meaningless number in the UI — so we
/// prefer the service UUID (robust) and fall back to name patterns.
fn coyote_product_name(local_name: &str, services: &[Uuid]) -> Option<String> {
    let n = local_name.to_uppercase();
    if services.contains(&V3_SERVICE_UUID) || n.starts_with("47L") {
        Some("Coyote 3.0".to_string())
    } else if services.contains(&V2_SERVICE_UUID) || n.contains("ESTIM01") || n.contains("D-LAB") {
        Some("Coyote 2.0".to_string())
    } else if n.contains("COYOTE") {
        Some("Coyote".to_string())
    } else {
        None
    }
}

// V2 Specific UUIDs (Base: 955Axxxx-0FE2-F5AA-A094-84B8D4F3E8AD)
const V2_PWM_AB2_UUID: Uuid = Uuid::from_u128(0x955a1504_0fe2_f5aa_a094_84b8d4f3e8ad); // Strength
const V2_PWM_A34_UUID: Uuid = Uuid::from_u128(0x955a1505_0fe2_f5aa_a094_84b8d4f3e8ad); // B Channel Waveform
const V2_PWM_B34_UUID: Uuid = Uuid::from_u128(0x955a1506_0fe2_f5aa_a094_84b8d4f3e8ad); // A Channel Waveform
                                                                                       // V2 Battery UUID (955a1500-...)
const V2_BATTERY_CHAR_UUID: Uuid = Uuid::from_u128(0x955a1500_0fe2_f5aa_a094_84b8d4f3e8ad);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeviceVersion {
    V2,
    V3,
}

#[derive(Debug, serde::Serialize, Clone)]
pub struct BluetoothAdapter {
    pub id: String,
    pub name: String,
}

#[derive(Debug, serde::Serialize, Clone)]
pub struct BluetoothDevice {
    pub address: String,
    pub name: Option<String>,
    /// Friendly product label derived at scan time (e.g. "Coyote 3.0"); None
    /// for devices we can't classify. The UI prefers this over `name`.
    pub product: Option<String>,
    pub rssi: Option<i16>,
}

pub struct BluetoothManager {
    manager: Manager,
    discovered_peripherals: HashMap<String, Peripheral>,
    discovered_devices: Vec<BluetoothDevice>,
    connected_peripheral: Option<Peripheral>,
    connected_device_address: Option<String>,

    // V3 Features
    write_characteristic: Option<Characteristic>,

    // V2 Features
    pub device_version: Option<DeviceVersion>,
    v2_char_intensity: Option<Characteristic>,
    v2_char_waveform_a: Option<Characteristic>, // Control Channel A (corresponding to PWM_B34)
    v2_char_waveform_b: Option<Characteristic>, // Control Channel B (corresponding to PWM_A34)

    battery_characteristic: Option<Characteristic>,

    // Last successful connect target — kept across handle-loss so the
    // reconnect task can retry without the user re-clicking. Cleared by
    // explicit disconnect (which also flips `auto_reconnect`).
    last_adapter_index: Option<usize>,
    last_address: Option<String>,
    pub auto_reconnect: bool,
}
impl BluetoothManager {
    pub async fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let manager = Manager::new().await?;
        Ok(Self {
            manager,
            discovered_peripherals: HashMap::new(),
            discovered_devices: Vec::new(),
            connected_peripheral: None,
            connected_device_address: None,
            write_characteristic: None,
            battery_characteristic: None,
            device_version: None,
            v2_char_intensity: None,
            v2_char_waveform_a: None,
            v2_char_waveform_b: None,
            last_adapter_index: None,
            last_address: None,
            auto_reconnect: false,
        })
    }

    pub async fn get_adapters(
        &self,
    ) -> Result<Vec<BluetoothAdapter>, Box<dyn Error + Send + Sync>> {
        let adapters = self.manager.adapters().await?;
        let mut adapter_list = Vec::new();

        for (index, adapter) in adapters.iter().enumerate() {
            let info = adapter.adapter_info().await?;
            adapter_list.push(BluetoothAdapter {
                id: index.to_string(),
                name: format!("{}: {}", index, info),
            });
        }

        Ok(adapter_list)
    }

    pub async fn scan_devices(
        &mut self,
        adapter_index: usize,
    ) -> Result<Vec<BluetoothDevice>, Box<dyn Error + Send + Sync>> {
        let adapters = self.manager.adapters().await?;
        let adapter = adapters.get(adapter_index).ok_or("Invalid adapter index")?;

        // Clear previously discovered peripherals and devices
        self.discovered_peripherals.clear();
        self.discovered_devices.clear();

        // Start scanning
        adapter.start_scan(ScanFilter::default()).await?;

        // Wait for devices to be discovered
        time::sleep(Duration::from_secs(5)).await;

        // Get discovered peripherals
        let peripherals = adapter.peripherals().await?;

        for peripheral in peripherals {
            let properties = peripheral.properties().await?;
            let address = peripheral.address().to_string();

            if let Some(props) = properties {
                // Filter for DG-LAB devices
                if let Some(name) = &props.local_name {
                    if is_dglab(name) {
                        let product = coyote_product_name(name, &props.services);

                        // Store the peripheral for later connection
                        self.discovered_peripherals
                            .insert(address.clone(), peripheral);

                        // Store the device info
                        self.discovered_devices.push(BluetoothDevice {
                            address: address.clone(),
                            name: Some(name.clone()),
                            product,
                            rssi: props.rssi,
                        });
                    }
                }
            }
        }

        // Stop scanning
        adapter.stop_scan().await?;

        println!(
            "Discovered {} DG-LAB devices, stored {} peripherals",
            self.discovered_devices.len(),
            self.discovered_peripherals.len()
        );

        Ok(self.discovered_devices.clone())
    }

    pub async fn connect_device(
        &mut self,
        adapter_index: usize,
        address: &str,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        // Idempotent: if we're already linked to this exact device, treat the
        // call as success instead of re-running connect(). btleplug's
        // peripheral.connect() errors when invoked against an already-connected
        // handle on Windows — which previously surfaced a spurious "Failed to
        // connect" in the UI whenever two connect paths raced (startup
        // auto-connect + a manual click, serialized by the manager mutex).
        if self.is_connected() && self.connected_device_address.as_deref() == Some(address) {
            println!("Already connected to {} — skipping reconnect", address);
            return Ok(());
        }

        println!("Attempting to connect to device: {}", address);
        println!(
            "Stored peripherals: {:?}",
            self.discovered_peripherals.keys().collect::<Vec<_>>()
        );

        // First try to get from stored peripherals
        let peripheral = if let Some(p) = self.discovered_peripherals.get(address) {
            println!("Found device in stored peripherals");
            p.clone()
        } else {
            // Fall back to scanning adapter's peripherals
            println!("Device not in stored peripherals, checking adapter...");
            let adapters = self.manager.adapters().await?;
            let adapter = adapters.get(adapter_index).ok_or("Invalid adapter index")?;

            let peripherals = adapter.peripherals().await?;

            peripherals
                .into_iter()
                .find(|p| p.address().to_string() == address)
                .ok_or_else(|| format!("Device not found: {}. Try scanning again.", address))?
        };

        // Connect to the device
        println!("Connecting to peripheral...");
        peripheral.connect().await?;

        // Discover services
        println!("Discovering services...");
        peripheral.discover_services().await?;

        // Find the write characteristic
        let services = peripheral.services();
        println!("Found {} services", services.len());

        self.write_characteristic = None;
        self.battery_characteristic = None;
        self.device_version = None;
        self.v2_char_intensity = None;
        self.v2_char_waveform_a = None;
        self.v2_char_waveform_b = None;

        for service in services {
            println!("Service: {}", service.uuid);
            for characteristic in service.characteristics {
                println!("  Characteristic: {}", characteristic.uuid);

                // V3 detection
                if characteristic.uuid == INSTRUCTION_CHAR_UUID {
                    println!("  -> Found V3 write characteristic!");
                    self.write_characteristic = Some(characteristic.clone());
                    self.device_version = Some(DeviceVersion::V3);
                }

                // V2 detection
                if characteristic.uuid == V2_PWM_AB2_UUID {
                    println!("  -> Found V2 Intensity characteristic!");
                    self.v2_char_intensity = Some(characteristic.clone());
                    self.device_version = Some(DeviceVersion::V2);
                }
                // The document states: PWM_B34 (1506) controls channel A
                if characteristic.uuid == V2_PWM_B34_UUID {
                    println!("  -> Found V2 Waveform A characteristic!");
                    self.v2_char_waveform_a = Some(characteristic.clone());
                }
                // Documentation states: PWM_A34 (1505) controls B channel
                if characteristic.uuid == V2_PWM_A34_UUID {
                    println!("  -> Found V2 Waveform B characteristic!");
                    self.v2_char_waveform_b = Some(characteristic.clone());
                }

                if characteristic.uuid == BATTERY_CHAR_UUID
                    || characteristic.uuid == V2_BATTERY_CHAR_UUID
                {
                    println!("  -> Found Battery characteristic!");
                    self.battery_characteristic = Some(characteristic.clone());
                }
            }
        }

        if self.device_version.is_none() {
            println!("WARNING: No supported device version identified - commands won't be sent");
        } else {
            println!("Device version identified: {:?}", self.device_version);
        }

        if self.write_characteristic.is_none() && self.device_version != Some(DeviceVersion::V2) {
            println!("WARNING: Write characteristic not found - commands won't be sent to device");
        }

        // Store the connected peripheral and address
        self.connected_peripheral = Some(peripheral);
        self.connected_device_address = Some(address.to_string());

        // Remember target for auto-reconnect after a dropped link.
        self.last_adapter_index = Some(adapter_index);
        self.last_address = Some(address.to_string());
        self.auto_reconnect = true;

        println!("Successfully connected to device: {}", address);
        Ok(())
    }

    /// Drop in-memory connection state without calling `peripheral.disconnect()`.
    /// Use when the OS has already closed the handle (HRESULT 0x80000013 etc.) —
    /// calling disconnect on a dead handle just errors. Keeps `last_*` and
    /// `auto_reconnect` so a follow-up reconnect task can retry the same device.
    pub fn mark_disconnected(&mut self) {
        self.connected_peripheral = None;
        self.write_characteristic = None;
        self.battery_characteristic = None;
        self.connected_device_address = None;
        self.device_version = None;
        self.v2_char_intensity = None;
        self.v2_char_waveform_a = None;
        self.v2_char_waveform_b = None;
    }

    /// Last successful (adapter_index, address) — drives auto-reconnect.
    pub fn last_connection_target(&self) -> Option<(usize, String)> {
        match (self.last_adapter_index, self.last_address.as_ref()) {
            (Some(i), Some(a)) => Some((i, a.clone())),
            _ => None,
        }
    }

    /// Write a command to the device (B0 or BF command)
    pub async fn write_command(&self, data: &[u8]) -> Result<(), Box<dyn Error + Send + Sync>> {
        let peripheral = self
            .connected_peripheral
            .as_ref()
            .ok_or("No device connected")?;

        let characteristic = self
            .write_characteristic
            .as_ref()
            .ok_or("Write characteristic not available")?;

        peripheral
            .write(characteristic, data, WriteType::WithoutResponse)
            .await?;
        Ok(())
    }

    /// Read the battery level
    pub async fn read_battery(&self) -> Result<u8, Box<dyn Error + Send + Sync>> {
        let peripheral = self
            .connected_peripheral
            .as_ref()
            .ok_or("No device connected")?;

        let characteristic = self
            .battery_characteristic
            .as_ref()
            .ok_or("Battery characteristic not available")?;

        let data = peripheral.read(characteristic).await?;
        Ok(data.first().copied().unwrap_or(0))
    }

    /// Check if device is connected
    pub fn is_connected(&self) -> bool {
        self.connected_peripheral.is_some()
            && (self.write_characteristic.is_some()
                || self.device_version == Some(DeviceVersion::V2))
    }

    /// Get the list of discovered devices (from last scan)
    pub fn get_discovered_devices(&self) -> Vec<BluetoothDevice> {
        self.discovered_devices.clone()
    }

    /// Get the connected device address
    pub fn get_connected_device_address(&self) -> Option<String> {
        self.connected_device_address.clone()
    }

    pub async fn disconnect_device(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        // Explicit user disconnect: cancel any pending auto-reconnect by
        // clearing the target + flag BEFORE awaiting peripheral.disconnect()
        // — that call can fail on a dead handle, but we still want the
        // reconnect task to give up.
        self.auto_reconnect = false;
        self.last_adapter_index = None;
        self.last_address = None;

        if let Some(peripheral) = self.connected_peripheral.take() {
            // Best-effort: a dead handle errors here; we still want to
            // clear local state so the UI reflects "disconnected".
            let disc_result = peripheral.disconnect().await;
            self.write_characteristic = None;
            self.battery_characteristic = None;
            self.connected_device_address = None;
            self.device_version = None;
            self.v2_char_intensity = None;
            self.v2_char_waveform_a = None;
            self.v2_char_waveform_b = None;
            println!("Disconnected from device");
            disc_result?;
        }
        Ok(())
    }
    pub async fn write_v2_data(
        &self,
        intensity: &[u8],
        wave_a: &[u8],
        wave_b: &[u8],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let peripheral = self
            .connected_peripheral
            .as_ref()
            .ok_or("No device connected")?;

        if let Some(char_int) = &self.v2_char_intensity {
            peripheral
                .write(char_int, intensity, WriteType::WithoutResponse)
                .await?;
        } else {
            println!("[WARN] V2 intensity characteristic not available");
        }
        if let Some(char_wa) = &self.v2_char_waveform_a {
            peripheral
                .write(char_wa, wave_a, WriteType::WithoutResponse)
                .await?;
        } else {
            println!("[WARN] V2 waveform A characteristic not available");
        }
        if let Some(char_wb) = &self.v2_char_waveform_b {
            peripheral
                .write(char_wb, wave_b, WriteType::WithoutResponse)
                .await?;
        } else {
            println!("[WARN] V2 waveform B characteristic not available");
        }
        Ok(())
    }
}

// Global Bluetooth manager instance
pub static BLUETOOTH_MANAGER: tokio::sync::OnceCell<tokio::sync::Mutex<BluetoothManager>> =
    tokio::sync::OnceCell::const_new();

pub async fn get_bluetooth_manager(
) -> Result<&'static tokio::sync::Mutex<BluetoothManager>, Box<dyn Error + Send + Sync>> {
    BLUETOOTH_MANAGER
        .get_or_try_init(|| async {
            let manager = BluetoothManager::new().await?;
            Ok(tokio::sync::Mutex::new(manager))
        })
        .await
}

/// True if the error indicates the OS closed our BLE handle out from under
/// us (device dropped, adapter reset, link supervision timeout). Match by
/// string because btleplug returns a `Box<dyn Error>` here and the
/// underlying variant is platform-specific (Windows: HRESULT 0x80000013;
/// CoreBluetooth: "Peripheral is disconnected"; BlueZ: "Not connected").
pub fn is_handle_dead(err: &(dyn Error + Send + Sync + 'static)) -> bool {
    let s = err.to_string();
    s.contains("object has been closed")
        || s.contains("0x80000013")
        || s.contains("NotConnected")
        || s.contains("Not connected")
        || s.contains("not connected")
        || s.contains("Peripheral is disconnected")
        || s.contains("device disconnected")
        || s.contains("No device connected")
}

/// Spawn a one-shot reconnect attempt against the last successful target.
/// Idempotent — a static guard prevents stacking multiple attempts. Honors
/// `auto_reconnect`: if the user has explicitly disconnected, the task
/// exits without retrying. Backs off 1s → 2s → 4s → … capped at 30s, with
/// no overall retry cap (a stim device dropped overnight should still
/// reconnect when it comes back).
pub fn spawn_reconnect_attempt() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static RECONNECTING: AtomicBool = AtomicBool::new(false);

    if RECONNECTING.swap(true, Ordering::SeqCst) {
        return;
    }

    tokio::spawn(async move {
        let mut delay_ms: u64 = 1000;
        loop {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;

            let manager = match get_bluetooth_manager().await {
                Ok(m) => m,
                Err(e) => {
                    println!("[reconnect] cannot get manager: {} — giving up", e);
                    break;
                }
            };
            let mut guard = manager.lock().await;

            if !guard.auto_reconnect {
                println!("[reconnect] auto-reconnect disabled — stopping");
                break;
            }
            if guard.is_connected() {
                // Something else (manual reconnect via UI) already restored it.
                break;
            }

            let (adapter_index, address) = match guard.last_connection_target() {
                Some(t) => t,
                None => {
                    println!("[reconnect] no last target — stopping");
                    break;
                }
            };

            println!("[reconnect] attempting reconnect to {}", address);

            // Drop the cached peripheral so connect_device falls into the
            // adapter-fallback path. Windows otherwise hands back the same
            // BluetoothLEDevice instance whose radio link is dead — connect()
            // returns Ok against the cache without actually re-linking, and
            // every subsequent write errors HRESULT 0x80000013.
            guard.discovered_peripherals.remove(&address);

            let connect_result = guard.connect_device(adapter_index, &address).await;
            if let Err(e) = connect_result {
                drop(guard);
                delay_ms = delay_ms.saturating_mul(2).min(30_000);
                println!("[reconnect] connect failed: {} — retry in {}ms", e, delay_ms);
                continue;
            }

            // connect_device returns Ok before the GATT link is necessarily
            // live on Windows. Probe with a real read; if it fails we treat
            // this whole attempt as failed so we don't flip the UI to
            // connected and then immediately back. Small settle delay first
            // gives the radio handshake time to land.
            drop(guard);
            tokio::time::sleep(Duration::from_millis(400)).await;
            let manager2 = match get_bluetooth_manager().await {
                Ok(m) => m,
                Err(_) => break,
            };
            let mut guard = manager2.lock().await;

            let battery_level = match guard.read_battery().await {
                Ok(level) => level,
                Err(e) => {
                    println!(
                        "[reconnect] connect ok but probe read failed: {} — treating as still down",
                        e
                    );
                    guard.mark_disconnected();
                    drop(guard);
                    delay_ms = delay_ms.saturating_mul(2).min(30_000);
                    continue;
                }
            };

            drop(guard);
            crate::device::reset_bf_snapshot().await;
            crate::emit_connection_changed("bluetooth", true, Some(address.clone()));
            crate::emit_battery_changed(battery_level);
            println!("[reconnect] success (battery {}%)", battery_level);
            break;
        }
        RECONNECTING.store(false, Ordering::SeqCst);
    });
}

/// Spawn a background task that reads the battery level every 30 seconds
/// and emits `battery-changed` to the frontend. The task self-exits the
/// first time it wakes up and finds the device disconnected, so callers
/// can invoke this once per connect without tracking lifetimes.
///
/// Guarded by a static flag so reconnects within the same session don't
/// stack multiple monitors — the existing task keeps running across a
/// disconnect/reconnect cycle as long as a new connection is established
/// before it next wakes up.
pub fn start_battery_monitor() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static RUNNING: AtomicBool = AtomicBool::new(false);

    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;

            let manager = match get_bluetooth_manager().await {
                Ok(m) => m,
                Err(_) => break,
            };
            let guard = manager.lock().await;
            if !guard.is_connected() {
                break;
            }

            match guard.read_battery().await {
                Ok(level) => {
                    drop(guard);
                    crate::emit_battery_changed(level);
                }
                Err(e) => {
                    drop(guard);
                    eprintln!("[BATTERY] Poll read failed: {}", e);
                }
            }
        }
        RUNNING.store(false, Ordering::SeqCst);
    });
}

// ---------------------------------------------------------------------------
// Backend-owned device scanning
// ---------------------------------------------------------------------------
// The backend owns the scan lifecycle so the frontend stays a thin renderer:
// it calls start_device_scan when the output panel opens and stop_device_scan
// when it closes, and otherwise just listens for `devices-discovered` events.
// The scan runs continuously (adapter stays in scan mode; we re-read the
// peripheral list every second) rather than the old start→sleep 5s→stop burst,
// so the list fills in live without the UI driving a timer.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as ScanOrdering};

static SCAN_WANTED: AtomicBool = AtomicBool::new(false);
static SCAN_RUNNING: AtomicBool = AtomicBool::new(false);
static SCAN_ADAPTER: AtomicUsize = AtomicUsize::new(0);

/// Begin (or retarget) the continuous scan loop on `adapter_index`. Idempotent:
/// a static guard prevents stacking multiple loops; changing the adapter while
/// running is picked up live via `SCAN_ADAPTER`.
pub fn start_device_scan(adapter_index: usize) {
    SCAN_ADAPTER.store(adapter_index, ScanOrdering::SeqCst);
    SCAN_WANTED.store(true, ScanOrdering::SeqCst);

    if SCAN_RUNNING.swap(true, ScanOrdering::SeqCst) {
        return; // a loop is already running and will honour the new target
    }

    tokio::spawn(async move {
        if let Err(e) = run_scan_loop().await {
            println!("[scan] loop ended with error: {}", e);
        }
        SCAN_RUNNING.store(false, ScanOrdering::SeqCst);
    });
}

/// Ask the scan loop to stop. The loop stops the adapter scan and exits on its
/// next tick (≤1s).
pub fn stop_device_scan() {
    SCAN_WANTED.store(false, ScanOrdering::SeqCst);
}

/// Read advertised properties for each peripheral and keep only DG-LAB devices,
/// tagging each with a friendly product label. Done without holding the manager
/// lock so connect/disconnect aren't blocked while we await per-device reads.
async fn collect_devices(peripherals: Vec<Peripheral>) -> Vec<(String, Peripheral, BluetoothDevice)> {
    let mut out = Vec::new();
    for peripheral in peripherals {
        let props = match peripheral.properties().await {
            Ok(p) => p,
            Err(_) => continue,
        };
        if let Some(props) = props {
            if let Some(name) = props.local_name.clone() {
                if is_dglab(&name) {
                    let address = peripheral.address().to_string();
                    let product = coyote_product_name(&name, &props.services);
                    let dev = BluetoothDevice {
                        address: address.clone(),
                        name: Some(name),
                        product,
                        rssi: props.rssi,
                    };
                    out.push((address, peripheral, dev));
                }
            }
        }
    }
    out
}

/// Replace the manager's discovered lists with this poll's results and return a
/// clone for emission.
async fn store_devices(
    manager_mutex: &tokio::sync::Mutex<BluetoothManager>,
    collected: Vec<(String, Peripheral, BluetoothDevice)>,
) -> Vec<BluetoothDevice> {
    let mut guard = manager_mutex.lock().await;
    guard.discovered_peripherals.clear();
    guard.discovered_devices.clear();
    for (address, peripheral, dev) in collected {
        guard.discovered_peripherals.insert(address, peripheral);
        guard.discovered_devices.push(dev);
    }
    guard.discovered_devices.clone()
}

async fn run_scan_loop() -> Result<(), Box<dyn Error + Send + Sync>> {
    let manager_mutex = get_bluetooth_manager().await?;
    let mut active_adapter: Option<usize> = None;
    let mut adapter: Option<Adapter> = None;

    while SCAN_WANTED.load(ScanOrdering::SeqCst) {
        let want_idx = SCAN_ADAPTER.load(ScanOrdering::SeqCst);

        // (Re)acquire the adapter and (re)start scanning if the target changed.
        if active_adapter != Some(want_idx) {
            if let Some(a) = adapter.take() {
                let _ = a.stop_scan().await;
            }
            let next = {
                let guard = manager_mutex.lock().await;
                guard.manager.adapters().await?.into_iter().nth(want_idx)
            };
            match next {
                Some(a) => {
                    a.start_scan(ScanFilter::default()).await?;
                    adapter = Some(a);
                    active_adapter = Some(want_idx);
                    println!("[scan] started on adapter {}", want_idx);
                }
                None => {
                    println!("[scan] invalid adapter index {} — stopping", want_idx);
                    break;
                }
            }
        }

        // Don't poll while a device is connected: Windows dislikes scanning
        // alongside an active GATT link, and there's nothing to discover then.
        let connected = { manager_mutex.lock().await.is_connected() };
        if !connected {
            if let Some(a) = &adapter {
                match a.peripherals().await {
                    Ok(peripherals) => {
                        let collected = collect_devices(peripherals).await;
                        let devices = store_devices(manager_mutex, collected).await;
                        crate::emit_devices_discovered(devices);
                    }
                    Err(e) => println!("[scan] peripherals() failed: {}", e),
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(1000)).await;
    }

    if let Some(a) = adapter.take() {
        let _ = a.stop_scan().await;
    }
    println!("[scan] stopped");
    Ok(())
}
