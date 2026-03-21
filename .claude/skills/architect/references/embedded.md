# Embedded Rust: Embassy, ESP32, and Raspberry Pi

## Embassy Framework

Embassy is an async/await executor and HAL for embedded systems.

### Key Concepts

**Embassy Executor**: Async runtime for embedded
```rust
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Spawn concurrent tasks
    spawner.spawn(task1()).unwrap();
    spawner.spawn(task2()).unwrap();
}

#[embassy_executor::task]
async fn task1() {
    loop {
        // Async work
        Timer::after(Duration::from_secs(1)).await;
    }
}
```

**Embassy Time**: Async delays without blocking
```rust
use embassy_time::{Duration, Timer};

async fn blink() {
    loop {
        led.set_high();
        Timer::after(Duration::from_millis(500)).await;
        led.set_low();
        Timer::after(Duration::from_millis(500)).await;
    }
}
```

**Embassy Channels**: Share data between tasks
```rust
use embassy_sync::channel::Channel;

static CHANNEL: Channel<CriticalSectionRawMutex, SensorData, 10> = Channel::new();

#[embassy_executor::task]
async fn sensor_task() {
    loop {
        let data = read_sensor().await;
        CHANNEL.send(data).await;
    }
}

#[embassy_executor::task]
async fn process_task() {
    loop {
        let data = CHANNEL.receive().await;
        process(data);
    }
}
```

## ESP32 with Embassy

### Project Setup

**Cargo.toml**:
```toml
[dependencies]
embassy-executor = { version = "0.5", features = ["nightly"] }
embassy-time = "0.3"
esp-hal = "0.16"
esp-backtrace = { version = "0.11", features = ["esp32", "panic-handler", "exception-handler", "println"] }
esp-println = { version = "0.9", features = ["esp32"] }

[profile.release]
opt-level = "z"
```

**.cargo/config.toml**:
```toml
[build]
target = "xtensa-esp32-none-elf"

[target.xtensa-esp32-none-elf]
runner = "espflash flash --monitor"
```

### Common ESP32 Patterns

#### GPIO Control
```rust
use esp_hal::{
    clock::ClockControl,
    embassy,
    gpio::{GpioPin, Input, Output, PullDown, PushPull},
    peripherals::Peripherals,
    prelude::*,
};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let peripherals = Peripherals::take();
    let system = peripherals.SYSTEM.split();
    let clocks = ClockControl::boot_defaults(system.clock_control).freeze();

    embassy::init(&clocks, TimerGroup0);

    let io = IO::new(peripherals.GPIO, peripherals.IO_MUX);

    let mut led = Output::new(io.pins.gpio2, Level::Low);
    let button = Input::new(io.pins.gpio0, Pull::Up);

    loop {
        if button.is_low() {
            led.toggle();
        }
        Timer::after(Duration::from_millis(100)).await;
    }
}
```

#### WiFi
```rust
use esp_wifi::{
    wifi::{WifiController, WifiDevice, WifiEvent, WifiStaDevice, WifiState},
    EspWifiInitFor,
};

async fn connect_wifi(
    controller: &mut WifiController<'static>,
    ssid: &str,
    password: &str,
) -> Result<(), WifiError> {
    controller.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: ssid.try_into().unwrap(),
        password: password.try_into().unwrap(),
        ..Default::default()
    }))?;

    controller.start().await?;
    controller.connect().await?;

    // Wait for IP
    loop {
        if matches!(controller.is_connected(), Ok(true)) {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    Ok(())
}
```

#### I2C Sensors
```rust
use esp_hal::i2c::I2C;

async fn read_sensor() {
    let i2c = I2C::new(
        peripherals.I2C0,
        io.pins.gpio21,  // SDA
        io.pins.gpio22,  // SCL
        100u32.kHz(),
        &clocks,
    );

    let mut buffer = [0u8; 2];
    i2c.write_read(SENSOR_ADDR, &[REGISTER], &mut buffer).ok();

    let value = u16::from_be_bytes(buffer);
}
```

#### SPI
```rust
use esp_hal::spi::{master::Spi, SpiMode};

let spi = Spi::new(
    peripherals.SPI2,
    40u32.MHz(),
    SpiMode::Mode0,
    &clocks,
).with_pins(
    Some(io.pins.gpio18),  // SCK
    Some(io.pins.gpio23),  // MOSI
    Some(io.pins.gpio19),  // MISO
    Some(io.pins.gpio5),   // CS
);
```

#### PWM for Servos/LEDs
```rust
use esp_hal::ledc::{LEDC, LSGlobalClkSource, LowSpeed, timer};

let mut ledc = LEDC::new(peripherals.LEDC, &clocks);
ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);

let mut lstimer0 = ledc.get_timer::<LowSpeed>(timer::Number::Timer0);
lstimer0
    .configure(timer::config::Config {
        duty: timer::config::Duty::Duty5Bit,
        clock_source: timer::LSClockSource::APBClk,
        frequency: 50u32.Hz(),
    })
    .unwrap();

let mut channel0 = ledc.get_channel(channel::Number::Channel0, io.pins.gpio2);
channel0
    .configure(channel::config::Config {
        timer: &lstimer0,
        duty_pct: 50,
        pin_config: channel::config::PinConfig::PushPull,
    })
    .unwrap();
```

### ESP32 Memory Considerations

```rust
// Use references, not copies
fn process(data: &[u8]) {  // ✅
    // ...
}

fn process(data: [u8; 1024]) {  // ❌ Copies on stack
    // ...
}

// Static allocation for shared resources
static BUFFER: Mutex<RefCell<[u8; 1024]>> =
    Mutex::new(RefCell::new([0u8; 1024]));

// Avoid heap allocation where possible
// Use heapless collections
use heapless::Vec;
let mut vec: Vec<u8, 32> = Vec::new();
```

## Raspberry Pi with Rust

### GPIO with rppal
```rust
use rppal::gpio::Gpio;
use std::thread;
use std::time::Duration;

fn main() -> Result<()> {
    let gpio = Gpio::new()?;
    let mut led = gpio.get(17)?.into_output();

    loop {
        led.set_high();
        thread::sleep(Duration::from_secs(1));
        led.set_low();
        thread::sleep(Duration::from_secs(1));
    }
}
```

### I2C on Pi
```rust
use rppal::i2c::I2c;

fn read_sensor() -> Result<u16> {
    let mut i2c = I2c::new()?;
    i2c.set_slave_address(0x48)?;

    let mut buffer = [0u8; 2];
    i2c.block_read(0x00, &mut buffer)?;

    Ok(u16::from_be_bytes(buffer))
}
```

### SPI on Pi
```rust
use rppal::spi::{Bus, Mode, SlaveSelect, Spi};

fn spi_transfer() -> Result<Vec<u8>> {
    let spi = Spi::new(Bus::Spi0, SlaveSelect::Ss0, 1_000_000, Mode::Mode0)?;

    let tx_buffer = vec![0x01, 0x02, 0x03];
    let rx_buffer = spi.transfer(&tx_buffer)?;

    Ok(rx_buffer)
}
```

### PWM on Pi
```rust
use rppal::pwm::{Channel, Polarity, Pwm};

fn servo_control() -> Result<()> {
    let pwm = Pwm::with_frequency(
        Channel::Pwm0,
        50.0,  // 50 Hz for servos
        0.075,  // 7.5% duty cycle (center position)
        Polarity::Normal,
        true,
    )?;

    Ok(())
}
```

## Common Embedded Patterns

### Debouncing Buttons
```rust
#[embassy_executor::task]
async fn button_handler(mut button: Input<'static>) {
    const DEBOUNCE_MS: u64 = 50;

    loop {
        button.wait_for_falling_edge().await;
        Timer::after(Duration::from_millis(DEBOUNCE_MS)).await;

        if button.is_low() {
            // Button confirmed pressed
            handle_button_press();
        }
    }
}
```

### Watchdog Timer
```rust
use esp_hal::rtc_cntl::Rtc;

let mut rtc = Rtc::new(peripherals.RTC_CNTL);
rtc.rwdt.enable();
rtc.rwdt.start(2000u64.millis());  // 2 second timeout

loop {
    do_work();
    rtc.rwdt.feed();  // Reset watchdog
}
```

### Power Management
```rust
use esp_hal::prelude::*;

// Light sleep
esp_hal::rtc_cntl::sleep::light_sleep(Duration::from_secs(5));

// Deep sleep
esp_hal::rtc_cntl::sleep::deep_sleep(Duration::from_secs(60));
```

### Interrupt-Driven IO
```rust
static BUTTON_PRESSED: AtomicBool = AtomicBool::new(false);

#[embassy_executor::task]
async fn button_task(button: AnyPin) {
    let mut button = Input::new(button, Pull::Up);

    loop {
        button.wait_for_falling_edge().await;
        BUTTON_PRESSED.store(true, Ordering::Relaxed);
    }
}
```

## Cross-Platform Abstractions

```rust
// Define trait for hardware abstraction
trait Led {
    fn on(&mut self);
    fn off(&mut self);
    fn toggle(&mut self);
}

// ESP32 implementation
impl Led for Output<'static, GpioPin<2>> {
    fn on(&mut self) { self.set_high(); }
    fn off(&mut self) { self.set_low(); }
    fn toggle(&mut self) { self.toggle(); }
}

// Pi implementation
impl Led for OutputPin {
    fn on(&mut self) { self.set_high(); }
    fn off(&mut self) { self.set_low(); }
    fn toggle(&mut self) { self.toggle(); }
}
```

## Testing Embedded Code

### Unit Tests with mocks
```rust
#[cfg(test)]
mod tests {
    struct MockSensor {
        value: u16,
    }

    impl Sensor for MockSensor {
        fn read(&self) -> u16 {
            self.value
        }
    }

    #[test]
    fn test_processing() {
        let sensor = MockSensor { value: 42 };
        assert_eq!(process_reading(&sensor), expected);
    }
}
```

### On-device testing
```rust
#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // Run tests on device
    test_gpio();
    test_i2c();
    esp_println::println!("All tests passed!");
}
```

## Debugging Tips

1. **Use esp-println for ESP32** - Logging over UART
2. **defmt for size-constrained targets** - Efficient logging
3. **probe-rs for debugging** - GDB alternative
4. **RTT (Real-Time Transfer)** - Fast logging without UART
5. **LED blink patterns** - Visual debugging

## Performance Tips

1. **Minimize allocations** - Use stack or static
2. **Use DMA where available** - Offload CPU
3. **Tune clock speeds** - Balance power vs performance
4. **Profile with probes** - Find bottlenecks
5. **Optimize hot loops** - Assembly inspection

## Resource Management

```rust
// Use RAII for cleanup
struct I2cGuard<'a> {
    i2c: I2C<'a, I2C0>,
}

impl Drop for I2cGuard<'_> {
    fn drop(&mut self) {
        // Release I2C bus
    }
}
```
