use btleplug::api::{Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Manager, Peripheral};
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

#[derive(Debug, serde::Serialize, Clone)]
pub struct BluetoothAdapter {
    pub id: String,
    pub name: String,
}

#[derive(Debug, serde::Serialize, Clone)]
pub struct BluetoothDevice {
    pub address: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
}

pub struct BluetoothManager {
    manager: Manager,
    // Store discovered peripherals by address for later connection
    discovered_peripherals: HashMap<String, Peripheral>,
    // Store discovered device info (name, rssi) by address
    discovered_devices: Vec<BluetoothDevice>,
    // Store the connected peripheral
    connected_peripheral: Option<Peripheral>,
    // Store the connected device address
    connected_device_address: Option<String>,
    // Store the write characteristic for sending commands
    write_characteristic: Option<Characteristic>,
    // Store the battery characteristic for reading battery level
    battery_characteristic: Option<Characteristic>,
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
        })
    }

    pub async fn get_adapters(&self) -> Result<Vec<BluetoothAdapter>, Box<dyn Error + Send + Sync>> {
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

    pub async fn scan_devices(&mut self, adapter_index: usize) -> Result<Vec<BluetoothDevice>, Box<dyn Error + Send + Sync>> {
        let adapters = self.manager.adapters().await?;
        let adapter = adapters.get(adapter_index)
            .ok_or("Invalid adapter index")?;

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
                    if name.contains("DG-LAB") || name.contains("COYOTE") || name.contains("47L") {
                        // Store the peripheral for later connection
                        self.discovered_peripherals.insert(address.clone(), peripheral);

                        // Store the device info
                        self.discovered_devices.push(BluetoothDevice {
                            address: address.clone(),
                            name: Some(name.clone()),
                            rssi: props.rssi,
                        });
                    }
                }
            }
        }

        // Stop scanning
        adapter.stop_scan().await?;

        println!("Discovered {} DG-LAB devices, stored {} peripherals", self.discovered_devices.len(), self.discovered_peripherals.len());

        Ok(self.discovered_devices.clone())
    }

    pub async fn connect_device(&mut self, adapter_index: usize, address: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        println!("Attempting to connect to device: {}", address);
        println!("Stored peripherals: {:?}", self.discovered_peripherals.keys().collect::<Vec<_>>());

        // First try to get from stored peripherals
        let peripheral = if let Some(p) = self.discovered_peripherals.get(address) {
            println!("Found device in stored peripherals");
            p.clone()
        } else {
            // Fall back to scanning adapter's peripherals
            println!("Device not in stored peripherals, checking adapter...");
            let adapters = self.manager.adapters().await?;
            let adapter = adapters.get(adapter_index)
                .ok_or("Invalid adapter index")?;

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

        for service in services {
            println!("Service: {}", service.uuid);

            for characteristic in service.characteristics {
                println!("  Characteristic: {}", characteristic.uuid);

                if characteristic.uuid == INSTRUCTION_CHAR_UUID {
                    println!("  -> Found instruction write characteristic!");
                    self.write_characteristic = Some(characteristic.clone());
                }

                if characteristic.uuid == BATTERY_CHAR_UUID {
                    println!("  -> Found battery characteristic!");
                    self.battery_characteristic = Some(characteristic.clone());
                }
            }
        }

        if self.write_characteristic.is_none() {
            println!("WARNING: Write characteristic not found - commands won't be sent to device");
        }

        // Store the connected peripheral and address
        self.connected_peripheral = Some(peripheral);
        self.connected_device_address = Some(address.to_string());

        println!("Successfully connected to device: {}", address);
        Ok(())
    }

    /// Write a command to the device (B0 or BF command)
    pub async fn write_command(&self, data: &[u8]) -> Result<(), Box<dyn Error + Send + Sync>> {
        let peripheral = self.connected_peripheral.as_ref()
            .ok_or("No device connected")?;

        let characteristic = self.write_characteristic.as_ref()
            .ok_or("Write characteristic not available")?;

        peripheral.write(characteristic, data, WriteType::WithoutResponse).await?;
        Ok(())
    }

    /// Read the battery level
    pub async fn read_battery(&self) -> Result<u8, Box<dyn Error + Send + Sync>> {
        let peripheral = self.connected_peripheral.as_ref()
            .ok_or("No device connected")?;

        let characteristic = self.battery_characteristic.as_ref()
            .ok_or("Battery characteristic not available")?;

        let data = peripheral.read(characteristic).await?;
        Ok(data.first().copied().unwrap_or(0))
    }

    /// Check if device is connected
    pub fn is_connected(&self) -> bool {
        self.connected_peripheral.is_some() && self.write_characteristic.is_some()
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
        if let Some(peripheral) = self.connected_peripheral.take() {
            peripheral.disconnect().await?;
            self.write_characteristic = None;
            self.battery_characteristic = None;
            self.connected_device_address = None;
            println!("Disconnected from device");
        }
        Ok(())
    }
}

// Global Bluetooth manager instance
pub static BLUETOOTH_MANAGER: tokio::sync::OnceCell<tokio::sync::Mutex<BluetoothManager>> = tokio::sync::OnceCell::const_new();

pub async fn get_bluetooth_manager() -> Result<&'static tokio::sync::Mutex<BluetoothManager>, Box<dyn Error + Send + Sync>> {
    BLUETOOTH_MANAGER.get_or_try_init(|| async {
        let manager = BluetoothManager::new().await?;
        Ok(tokio::sync::Mutex::new(manager))
    }).await
}