use evdev::Device;

fn main() {
    let mut d = Device::open("/dev/input/event2").unwrap();
    println!("{d}");
    println!("Events:");
    loop {
        for ev in d.fetch_events().unwrap() {
            println!("{ev:?}");
        }
    }
}
