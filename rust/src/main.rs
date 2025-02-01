use anyhow::Context;
use rusb::{DeviceHandle, GlobalContext, Result};
use std::fmt::format;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::id;
use std::time::Duration;

#[derive(Debug)]
struct SwitchProEmulator {
    usb_gadget: UsbGadget,
    handle: Option<DeviceHandle<GlobalContext>>,
    endpoint_in: u8,
    endpoint_out: u8,
}

#[derive(Debug, Clone, Copy)]
struct StickState {
    x: u8,
    y: u8,
}

#[derive(Debug)]
struct ControllerState {
    buttons: u32,
    left_stick: StickState,
    right_stick: StickState,
}

impl Default for ControllerState {
    fn default() -> Self {
        Self {
            buttons: 0,
            left_stick: StickState { x: 128, y: 128 },
            right_stick: StickState { x: 128, y: 128 },
        }
    }
}

const SWITCH_PRO_VID: u16 = 0x057E;
const SWITCH_PRO_PID: u16 = 0x2009;

impl SwitchProEmulator {
    fn new(usb_gadget: UsbGadget) -> Self {
        Self {
            usb_gadget,
            handle: None,
            endpoint_in: 0x81,
            endpoint_out: 0x01,
        }
    }

    fn send_input_report(&self, state: &ControllerState) -> Result<()> {
        let report = [
            0x00, // Report ID
            (state.buttons & 0xFF) as u8,
            ((state.buttons >> 8) & 0xFF) as u8,
            state.left_stick.x,
            state.left_stick.y,
            state.right_stick.x,
            state.right_stick.y,
        ];

        if let Some(handle) = &self.handle {
            handle.write_interrupt(self.endpoint_out, &report, Duration::from_millis(100))?;
        }

        Ok(())
    }

    fn receive_output_report(&self) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; 64];

        if let Some(handle) = &self.handle {
            let size =
                handle.read_interrupt(self.endpoint_in, &mut buf, Duration::from_millis(100))?;
            buf.truncate(size);
        }

        Ok(buf)
    }
}

#[derive(Debug)]
struct UsbGadget {
    gadget_path: PathBuf,
    hid_function_path: PathBuf,
    udc_path: PathBuf,
    udc: String,
}

impl UsbGadget {
    fn new(udc: &str) -> anyhow::Result<Self> {
        Self::load_kernel_module("libcomposite")?;
        Self::load_kernel_module("usb_f_hid")?;

        let gadget_path = Path::new("/sys/kernel/config/usb_gadget/switch_pro").to_owned();
        let function_hid_path = gadget_path.join("configs/c.1/hid.usb0");
        let udc_path = gadget_path.join("UDC");

        let slf = Self {
            gadget_path,
            hid_function_path: function_hid_path,
            udc_path,
            udc: udc.to_string(),
        };

        slf.setup_configfs()?;

        Ok(slf)
    }

    fn load_kernel_module(module_name: &str) -> anyhow::Result<()> {
        kmod::Context::new()
            .context("Failed initializing kmod context")?
            .module_new_from_name(module_name)
            .with_context(|| {
                format!(
                    "Failed when getting handle for '{}' kernel module",
                    module_name
                )
            })?
            .insert_module(0, &[])
            .or_else(|err| match err {
                kmod::errors::Error::InsertModule(errno) => {
                    if errno.0 == libc::EEXIST {
                        Ok(())
                    } else {
                        Err(err)
                    }
                }
                _ => Err(err),
            })
            .with_context(|| format!("Failed when inserting '{}' kernel module", module_name))
    }

    // Helper function to set up configfs for USB gadget
    fn setup_configfs(&self) -> anyhow::Result<()> {
        self.disable_gadget()?;
        self.remove_hid_function()?;

        // Create gadget directory
        fs::create_dir_all(&self.gadget_path)
            .with_context(|| format!("Failed while creating directory {:?}", &self.gadget_path))?;

        // Set USB device information
        let id_vendor = self.gadget_path.join("idVendor");
        fs::write(&id_vendor, format!("{:#04x}", SWITCH_PRO_VID))
            .with_context(|| format!("Failed when writing into {:?}", id_vendor))?;
        let id_product = self.gadget_path.join("idProduct");
        fs::write(&id_product, format!("{:#04x}", SWITCH_PRO_PID))
            .with_context(|| format!("Failed when writing into {:?}", id_product))?;

        // Set USB device descriptors
        let bcd_device_path = self.gadget_path.join("bcdDevice");
        fs::write(&bcd_device_path, "0x0100")
            .with_context(|| format!("Failed when writing into {:?}", bcd_device_path))?;
        let bcd_usb_path = self.gadget_path.join("bcdUSB");
        fs::write(&bcd_usb_path, "0x0200")
            .with_context(|| format!("Failed when writing into {:?}", bcd_usb_path))?;
        let b_device_class_path = self.gadget_path.join("bDeviceClass");
        fs::write(&b_device_class_path, "0x00")
            .with_context(|| format!("Failed when writing into {:?}", b_device_class_path))?;
        let b_device_sub_class_path = self.gadget_path.join("bDeviceSubClass");
        fs::write(&b_device_sub_class_path, "0x00")
            .with_context(|| format!("Failed when writing into {:?}", b_device_sub_class_path))?;
        let b_device_protocol_path = self.gadget_path.join("bDeviceProtocol");
        fs::write(&b_device_protocol_path, "0x00")
            .with_context(|| format!("Failed when writing into {:?}", b_device_protocol_path))?;
        let b_max_packet_size0_path = self.gadget_path.join("bMaxPacketSize0");
        fs::write(&b_max_packet_size0_path, "64")
            .with_context(|| format!("Failed when writing into {:?}", b_max_packet_size0_path))?;

        // Configure strings
        let strings_path = self.gadget_path.join("strings/0x409");
        fs::create_dir_all(&strings_path)
            .with_context(|| format!("Failed when creating directory {:?}", strings_path))?;
        let manufacturer_path = strings_path.join("manufacturer");
        fs::write(&manufacturer_path, "Nintendo")
            .with_context(|| format!("Failed when writing into {:?}", manufacturer_path))?;
        let product_path = strings_path.join("product");
        fs::write(&product_path, "Pro Controller")
            .with_context(|| format!("Failed when writing into {:?}", product_path))?;

        // Configure HID function
        let function_path = self.gadget_path.join("functions/hid.usb0");
        fs::create_dir_all(&function_path)
            .with_context(|| format!("Failed when creating directory {:?}", function_path))?;
        let protocol_path = function_path.join("protocol");
        fs::write(&protocol_path, "0")
            .with_context(|| format!("Failed when writing into {:?}", protocol_path))?;
        let subclass_path = function_path.join("subclass");
        fs::write(&subclass_path, "0")
            .with_context(|| format!("Failed when writing into {:?}", subclass_path))?;
        let report_length_path = function_path.join("report_length");
        fs::write(&report_length_path, "64")
            .with_context(|| format!("Failed when writing into {:?}", report_length_path))?;

        // Write HID report descriptor
        let report_desc: &[u8] = &[
            0x05, 0x01, // Usage Page (Generic Desktop Ctrls)
            0x09, 0x05, // Usage (Game Pad)
            0xA1, 0x01, // Collection (Application)
            0x15, 0x00, // Logical Minimum (0)
            0x25, 0x01, // Logical Maximum (1)
            0x35, 0x00, // Physical Minimum (0)
            0x45, 0x01, // Physical Maximum (1)
            0x75, 0x01, // Report Size (1)
            0x95, 0x10, // Report Count (16)
            0x05, 0x09, // Usage Page (Button)
            0x19, 0x01, // Usage Minimum (0x01)
            0x29, 0x10, // Usage Maximum (0x10)
            0x81, 0x02, // Input (Data,Var,Abs,No Wrap,Linear)
            0x05, 0x01, // Usage Page (Generic Desktop Ctrls)
            0x25, 0x07, // Logical Maximum (7)
            0x46, 0x3B, 0x01, // Physical Maximum (315)
            0x75, 0x04, // Report Size (4)
            0x95, 0x01, // Report Count (1)
            0x65, 0x14, // Unit (System: English Rotation, Length: Centimeter)
            0x09, 0x39, // Usage (Hat switch)
            0x81, 0x42, // Input (Data,Var,Abs,No Wrap,Linear)
            0x65, 0x00, // Unit (None)
            0x95, 0x01, // Report Count (1)
            0x81, 0x01, // Input (Const,Array,Abs)
            0x26, 0xFF, 0x00, // Logical Maximum (255)
            0x46, 0xFF, 0x00, // Physical Maximum (255)
            0x09, 0x30, // Usage (X)
            0x09, 0x31, // Usage (Y)
            0x09, 0x32, // Usage (Z)
            0x09, 0x35, // Usage (Rz)
            0x75, 0x08, // Report Size (8)
            0x95, 0x04, // Report Count (4)
            0x81, 0x02, // Input (Data,Var,Abs)
            0xC0, // End Collection
        ];
        let report_desc_path = function_path.join("report_desc");
        fs::write(&report_desc_path, report_desc)
            .with_context(|| format!("Failed when writing into {:?}", report_desc_path))?;

        // Create configuration
        let config_c1_path = self.gadget_path.join("configs/c.1");
        fs::create_dir_all(&config_c1_path)
            .with_context(|| format!("Failed when creating directory {:?}", config_c1_path))?;
        let max_power_path = config_c1_path.join("MaxPower");
        fs::write(&max_power_path, "500")
            .with_context(|| format!("Failed when writing into {:?}", max_power_path))?;

        // Link HID function to configuration
        assert_eq!(config_c1_path.join("hid.usb0"), self.hid_function_path);
        std::os::unix::fs::symlink(&function_path, &self.hid_function_path).with_context(|| {
            format!(
                "Failed when symlinking {:?} to {:?}",
                function_path, &self.hid_function_path
            )
        })?;

        // Enable gadget (you would need to symlink the UDC device here)
        assert_eq!(self.udc_path, self.gadget_path.join("UDC"));

        fs::write(&self.udc_path, &self.udc)
            .with_context(|| format!("Failed when writing into {:?}", &self.udc_path))?;

        Ok(())
    }

    fn disable_gadget(&self) -> anyhow::Result<()> {
        if self.udc_path.exists() {
            fs::write(&self.udc_path, "")
                .with_context(|| format!("Failed when writing into {:?}", self.udc_path))?;
        }
        Ok(())
    }

    fn remove_hid_function(&self) -> anyhow::Result<()> {
        if self.hid_function_path.exists() {
            fs::remove_file(&self.hid_function_path)
                .with_context(|| format!("Failed when removing {:?}", self.hid_function_path))?;
        }
        Ok(())
    }
}

impl Drop for UsbGadget {
    fn drop(&mut self) {
        if let Err(err) = self.disable_gadget() {
            eprintln!("Failed to disable USB gadget: {:?}", err);
        }
        if let Err(err) = self.remove_hid_function() {
            eprintln!("Failed to remove USB function: {:?}", err);
        }
    }
}

// https://github.com/libretro/retroarch-joypad-autoconfig/blob/master/sdl2/Nintendo%20Switch%20Pro%20Controller.cfg
// https://github.com/DanielOgorchock/linux/blob/7811b8f1f00ee9f195b035951749c57498105d52/drivers/hid/hid-nintendo.c#L1175
#[derive(Debug, Clone, Copy)]
pub enum Button {
    Y = 0,
    X = 1,
    B = 2,
    A = 3,
    SrR = 4,
    SlR = 5,
    R = 6,
    ZR = 7,
    MINUS = 8,
    PLUS = 9,
    RSTICK = 10,
    LSTICK = 11,
    HOME = 12,
    CAPTURE = 13,
    DOWN = 16,
    UP = 17,
    RIGHT = 18,
    LEFT = 19,
    SrL = 20,
    SlL = 21,
    L = 22,
    ZL = 23,
}

impl ControllerState {
    pub fn press_button(&mut self, button: Button) {
        self.buttons |= 1 << (button as u32);
    }

    pub fn release_button(&mut self, button: Button) {
        self.buttons &= !(1 << (button as u16));
    }

    pub fn set_left_stick(&mut self, x: u8, y: u8) {
        self.left_stick = StickState { x, y };
    }

    pub fn set_right_stick(&mut self, x: u8, y: u8) {
        self.right_stick = StickState { x, y };
    }

    // Helper for diagonal movement
    pub fn set_left_stick_angle(&mut self, angle_degrees: f32, intensity: f32) {
        let radians = angle_degrees.to_radians();
        let x = ((128.0 + (radians.cos() * 127.0 * intensity)) as u8).clamp(0, 255);
        let y = ((128.0 + (radians.sin() * 127.0 * intensity)) as u8).clamp(0, 255);
        self.set_left_stick(x, y);
    }
}

// Example usage in main():
fn main() -> anyhow::Result<()> {
    let usb_gadget = UsbGadget::new("fe800000.usb")?;

    let mut emulator = SwitchProEmulator::new(usb_gadget);

    let mut state = ControllerState::default();
    let mut input = String::new();

    println!("Enter commands (a, b, x, y, l, r, zl, zr, up, down, left, right, quit):");

    while let Ok(_) = std::io::stdin().read_line(&mut input) {
        let command = input.trim().to_lowercase();

        match command.as_str() {
            "a" => state.press_button(Button::A),
            "b" => state.press_button(Button::B),
            "x" => state.press_button(Button::X),
            "y" => state.press_button(Button::Y),
            "l" => state.press_button(Button::L),
            "r" => state.press_button(Button::R),
            "zl" => state.press_button(Button::ZL),
            "zr" => state.press_button(Button::ZR),
            "minus" => state.press_button(Button::MINUS),
            "plus" => state.press_button(Button::PLUS),
            "lstick" => state.press_button(Button::LSTICK),
            "rstick" => state.press_button(Button::RSTICK),
            "home" => state.press_button(Button::HOME),
            "capture" => state.press_button(Button::CAPTURE),
            "up" => state.set_left_stick(128, 0),
            "down" => state.set_left_stick(128, 255),
            "left" => state.set_left_stick(0, 128),
            "right" => state.set_left_stick(255, 128),
            "center" => state.set_left_stick(128, 128),
            "quit" => break,
            _ => println!("    Unknown command"),
        }

        emulator.send_input_report(&state)?;

        // Reset state after sending
        state = ControllerState::default();
        input.clear();
    }

    // Example: Move stick right
    state.set_left_stick(255, 128); // Full right (x=255, y=center)
    emulator.send_input_report(&state)?;
    std::thread::sleep(Duration::from_millis(100));

    // Example: Diagonal movement (45 degrees, 50% intensity)
    state.set_left_stick_angle(45.0, 0.5);
    emulator.send_input_report(&state)?;

    // Example: Press multiple buttons
    state = ControllerState::default(); // Reset state
    state.press_button(Button::L);
    state.press_button(Button::R);
    state.press_button(Button::A);
    emulator.send_input_report(&state)?;
    std::thread::sleep(Duration::from_millis(100));

    // Example: Home + Capture screenshot
    state = ControllerState::default();
    state.press_button(Button::Home);
    state.press_button(Button::Capture);
    emulator.send_input_report(&state)?;

    Ok(())
}

// Here's a more practical example of how to implement common game actions:
impl ControllerState {
    pub fn perform_jump(&mut self, emulator: &SwitchProEmulator) -> Result<()> {
        // Press A
        self.press_button(Button::A);
        emulator.send_input_report(self)?;
        std::thread::sleep(Duration::from_millis(100));

        // Release A
        self.release_button(Button::A);
        emulator.send_input_report(self)?;

        Ok(())
    }

    pub fn dash_right(&mut self, emulator: &SwitchProEmulator, duration_ms: u64) -> Result<()> {
        // Hold B and move stick right
        self.press_button(Button::B);
        self.set_left_stick(255, 128);
        emulator.send_input_report(self)?;

        std::thread::sleep(Duration::from_millis(duration_ms));

        // Release everything
        self.release_button(Button::B);
        self.set_left_stick(128, 128);
        emulator.send_input_report(self)?;

        Ok(())
    }

    pub fn crouch(&mut self, emulator: &SwitchProEmulator) -> Result<()> {
        // Move stick down
        self.set_left_stick(128, 255);
        emulator.send_input_report(self)?;

        Ok(())
    }
}

// Example of how to use the action methods:
fn run_action_sequence(emulator: &SwitchProEmulator) -> Result<()> {
    let mut state = ControllerState::default();

    // Perform a sequence of actions
    state.dash_right(emulator, 500)?; // Dash right for 500ms
    state.perform_jump(emulator)?; // Jump
    state.crouch(emulator)?; // Crouch

    // Reset to neutral state
    state = ControllerState::default();
    emulator.send_input_report(&state)?;

    Ok(())
}
