use hidapi::HidApi;

const ID_LOAD_DEFAULT_SETTINGS: u8 = 0x8E;
const ID_GET_DIGITAL_MAPPINGS: u8 = 0x82;
const ID_CLEAR_DIGITAL_MAPPINGS: u8 = 0x81;

const VENDOR_ID: u16 = 10462;
const PRODUCT_ID: u16 = 4418;

fn main() {
    let data = {
        let mut data = [0; 65];
        // data[1] = ID_GET_DIGITAL_MAPPINGS;
        // data[1] = 0xb4;
        data[1] = 0xb6;
        data[2] = 4;
        data[3] = 0xff;
        data
    };
    let mut buf = [0u8; 8];

    let api = HidApi::new().unwrap();
    for device_info in api.device_list() {
        println!("{:?}", device_info);
    }

    let device = api.open(VENDOR_ID, PRODUCT_ID).unwrap();
    loop {
        let bytes_written = device.write(&data).unwrap();
        println!("Written bytes: {}", bytes_written);

        let bytes_read = device.read(&mut buf[..]).unwrap();
        println!("Read: {:?}", &buf[..bytes_read]);
    }
}

// fn main() {}
