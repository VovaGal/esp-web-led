//! HTTP Server with JSON POST handler
//!
//! Go to 192.168.71.1 to test

use core::convert::TryInto;
use std::thread::{self};

use embedded_svc::{
    http::{Headers, Method},
    io::{Read, Write},
    wifi::{self, AccessPointConfiguration, AuthMethod},
};

use embedded_hal_0_2::PwmPin;
use esp_idf_hal::{
    ledc::{LowSpeed, SpeedMode},
    prelude::*,
};
use esp_idf_svc::hal::{
    ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver, Resolution},
    prelude::Peripherals,
    units::Hertz,
};
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    http::server::EspHttpServer,
    nvs::EspDefaultNvsPartition,
    wifi::{BlockingWifi, EspWifi},
};

use log::*;

use serde::Deserialize;

use rgb_led::{RGB8, WS2812RMT};

use esp_idf_hal::sys::esp;
use esp_idf_hal::sys::ledc_set_freq;

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASS");
static INDEX_HTML: &str = include_str!("../http_server_page.html");

// Max payload length
const MAX_LEN: usize = 128;

// Need lots of stack to parse JSON
const STACK_SIZE: usize = 10240;

// Wi-Fi channel, between 1 and 11
const CHANNEL: u8 = 11;

#[derive(Deserialize, Debug)]
struct FormData {
    r: u32,
    g: u32,
    b: u32,
    f: u32,
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;

    let mut ws_led = WS2812RMT::new(peripherals.pins.gpio5, peripherals.rmt.channel0)?;
    ws_led.set_pixel(RGB8::new(0, 10, 0))?;

    let mut chip_led = WS2812RMT::new(peripherals.pins.gpio10, peripherals.rmt.channel1)?;
    chip_led.set_pixel(RGB8::new(10, 0, 0))?;

    // Configure and Initialize LEDC Timer Driver
    let mut timer_driver = LedcTimerDriver::new(
        peripherals.ledc.timer0,
        &TimerConfig::default()
            .frequency(1000.Hz())
            .resolution(Resolution::Bits14),
    )
    .unwrap();

    // Setup Wifi

    let (sendr, recvr) = std::sync::mpsc::channel();

    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?,
        sys_loop,
    )?;

    //

    connect_wifi(&mut wifi)?;

    let mut server = create_server()?;

    server.fn_handler("/", Method::Get, |req| {
        req.into_ok_response()?
            .write_all(INDEX_HTML.as_bytes())
            .map(|_| ())
    })?;

    server.fn_handler::<anyhow::Error, _>("/post", Method::Post, move |mut req| {
        let len = req.content_len().unwrap_or(0) as usize;

        if len > MAX_LEN {
            req.into_status_response(413)?
                .write_all("Request too big".as_bytes())?;
            return Ok(());
        }

        let mut buf = vec![0; len];
        req.read_exact(&mut buf)?;
        // error!("JSON ERROR\n{}", unsafe {
        //     String::from_utf8_unchecked(buf.clone())
        // });
        let mut resp = req.into_ok_response()?;

        if let Ok(form) = serde_json::from_slice::<FormData>(&buf) {
            info!("{:?}", form);
            // write!(resp, "Red: {} Green: {} Blue: {}", form.r, form.g, form.b)?;
            write!(resp, "Form: {:#?}", form)?;

            {
                // *dur.lock().unwrap() = form.age;
                sendr.send(form).unwrap();
            }
        } else {
            resp.write_all("JSON error".as_bytes())?;
            error!("JSON ERROR\n{}", unsafe {
                String::from_utf8_unchecked(buf)
            });
        }

        Ok(())
    })?;

    // Keep wifi and the server running beyond when main() returns (forever)
    // Do not call this if you ever want to stop or access them later.
    // Otherwise you can either add an infinite loop so the main task
    // never returns, or you can move them to another thread.
    // https://doc.rust-lang.org/stable/core/mem/fn.forget.html

    core::mem::forget(wifi);
    core::mem::forget(server);

    // Main task no longer needed, free up some memory

    let thread = thread::spawn(move || {
        let mut driver = LedcDriver::new(
            peripherals.ledc.channel0,
            timer_driver,
            peripherals.pins.gpio4,
        )
        .unwrap();
        //
        loop {
            let colour = recvr.recv().unwrap();
            esp!(unsafe { ledc_set_freq(LowSpeed::SPEED_MODE, driver.timer(), colour.f) }).unwrap();
            // timer_driver.set_frequency(colour.f.Hz()).unwrap();
            ws_led
                .set_pixel(RGB8::new(colour.r as u8, colour.g as u8, colour.b as u8))
                .unwrap();
            chip_led
                .set_pixel(RGB8::new(colour.g as u8, colour.r as u8, colour.b as u8))
                .unwrap();
            driver.set_duty(driver.get_max_duty() / 2).unwrap();
        }
    });

    core::mem::forget(thread);

    // thread.join().unwrap();
    Ok(())
}

fn connect_wifi(wifi: &mut BlockingWifi<EspWifi<'static>>) -> anyhow::Result<()> {
    // If instead of creating a new network you want to serve the page
    // on your local network, you can replace this configuration with
    // the client configuration from the http_client example.
    let wifi_configuration = wifi::Configuration::AccessPoint(AccessPointConfiguration {
        ssid: SSID.try_into().unwrap(),
        ssid_hidden: false,
        auth_method: AuthMethod::WPA2Personal,
        password: PASSWORD.try_into().unwrap(),
        channel: CHANNEL,
        ..Default::default()
    });

    wifi.set_configuration(&wifi_configuration)?;

    wifi.start()?;
    info!("Wifi started");

    // If using a client configuration you need
    // to connect to the network with:
    //
    //  ```
    //  wifi.connect()?;
    //  info!("Wifi connected");
    // ```

    wifi.wait_netif_up()?;
    info!("Wifi netif up");

    info!(
        "Created Wi-Fi with WIFI_SSID `{}` and WIFI_PASS `{}`",
        SSID, PASSWORD
    );

    Ok(())
}

fn create_server() -> anyhow::Result<EspHttpServer<'static>> {
    let server_configuration = esp_idf_svc::http::server::Configuration {
        stack_size: STACK_SIZE,
        ..Default::default()
    };

    Ok(EspHttpServer::new(&server_configuration)?)
}
