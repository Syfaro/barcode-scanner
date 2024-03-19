use enum_iterator::Sequence;
use enumflags2::{bitflags, BitFlags};
use futures::{FutureExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_serial::SerialPortBuilderExt;
use tokio_util::sync::CancellationToken;

static KEY_LOOKUP: phf::Map<u8, char> = phf::phf_map! {
    4u8 => 'a',
    5u8 => 'b',
    6u8 => 'c',
    7u8 => 'd',
    8u8 => 'e',
    9u8 => 'f',
    10u8 => 'g',
    11u8 => 'h',
    12u8 => 'i',
    13u8 => 'j',
    14u8 => 'k',
    15u8 => 'l',
    16u8 => 'm',
    17u8 => 'n',
    18u8 => 'o',
    19u8 => 'p',
    20u8 => 'q',
    21u8 => 'r',
    22u8 => 's',
    23u8 => 't',
    24u8 => 'u',
    25u8 => 'v',
    26u8 => 'w',
    27u8 => 'x',
    28u8 => 'y',
    29u8 => 'z',
    30u8 => '1',
    31u8 => '2',
    32u8 => '3',
    33u8 => '4',
    34u8 => '5',
    35u8 => '6',
    36u8 => '7',
    37u8 => '8',
    38u8 => '9',
    39u8 => '0',
    40u8 => '\n',
};

#[bitflags]
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModifierKeys {
    ControlLeft = 0b00000001,
    ShiftLeft = 0b00000010,
    AltLeft = 0b00000100,
    GuiLeft = 0b00001000,
    ControlRight = 0b00010000,
    ShiftRight = 0b00100000,
    AltRight = 0b01000000,
    GuiRight = 0b10000000,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Device {
    pub(crate) name: String,
    pub(crate) device_type: DeviceType,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum DeviceType {
    Hid {
        usage_page: u16,
        usage_id: u16,
        vendor_id: u16,
        product_id: u16,
    },
    Serial {
        path: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Sequence, Serialize, Deserialize)]
pub(crate) enum HidType {
    #[default]
    Keyboard,
    Pos,
}

impl std::fmt::Display for HidType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keyboard => write!(f, "Keyboard"),
            Self::Pos => write!(f, "Point of Sale"),
        }
    }
}

pub(crate) async fn list_devices() -> eyre::Result<Vec<Device>> {
    let mut scanners: Vec<_> = async_hid::DeviceInfo::enumerate()
        .await?
        .map(|device| Device {
            name: format!(
                "{} ({}:{} - {}, {})",
                device.name,
                hex::encode(device.vendor_id.to_be_bytes()),
                hex::encode(device.product_id.to_be_bytes()),
                device.usage_id,
                device.usage_page
            ),
            device_type: DeviceType::Hid {
                usage_page: device.usage_page,
                usage_id: device.usage_id,
                vendor_id: device.vendor_id,
                product_id: device.product_id,
            },
        })
        .collect()
        .await;

    let serialports = tokio::task::spawn_blocking(tokio_serial::available_ports)
        .await??
        .into_iter()
        .map(|port| Device {
            name: port.port_name.clone(),
            device_type: DeviceType::Serial {
                path: port.port_name,
            },
        });
    scanners.extend(serialports);

    scanners.sort_by_key(|scanner| scanner.clone());
    scanners.dedup();

    Ok(scanners)
}

pub(crate) async fn start_scanner(
    token: CancellationToken,
    device_type: DeviceType,
    baud_rate: Option<u32>,
    hid_type: Option<HidType>,
) -> eyre::Result<tokio::sync::mpsc::Receiver<eyre::Result<String>>> {
    let (tx, rx) = tokio::sync::mpsc::channel(1);

    eyre::ensure!(
        !matches!(device_type, DeviceType::Serial { .. } if baud_rate.is_none()),
        "baud rate must be specified for serial port"
    );

    tracing::info!(?device_type, "attempting to connect to device");

    // async-hid has types that are !Send so they need to be spawned locally.
    // However, there's no easy way to use a `LocalSet` without spawning a
    // blocking task to wait on that set, so all of that is managed here.
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();

        rt.block_on(async {
            let local = tokio::task::LocalSet::new();

            let err_tx = tx.clone();

            let fut = match device_type {
                DeviceType::Hid {
                    usage_page,
                    usage_id,
                    vendor_id,
                    product_id,
                } => match hid_type.unwrap_or_default() {
                    HidType::Keyboard => {
                        hid_scanner_keyboard(token, tx, usage_page, usage_id, vendor_id, product_id)
                            .boxed_local()
                    }
                    HidType::Pos => {
                        hid_scanner_pos(token, tx, usage_page, usage_id, vendor_id, product_id)
                            .boxed_local()
                    }
                },
                DeviceType::Serial { path } => serial_scanner(
                    token,
                    tx,
                    path,
                    baud_rate.expect("baud rate must be specified"),
                )
                .boxed_local(),
            };

            if let Err(err) = local.run_until(fut).await {
                tracing::error!("scanner encountered error: {err}");
                if let Err(err) = err_tx.send(Err(err)).await {
                    tracing::error!("could not send scanner error: {err}");
                }
            }
        })
    });

    Ok(rx)
}

#[tracing::instrument(skip(token, tx, usage_page, usage_id))]
async fn hid_scanner_keyboard(
    token: CancellationToken,
    tx: tokio::sync::mpsc::Sender<eyre::Result<String>>,
    usage_page: u16,
    usage_id: u16,
    vendor_id: u16,
    product_id: u16,
) -> eyre::Result<()> {
    let device = async_hid::DeviceInfo::enumerate()
        .await?
        .filter(|device| {
            futures::future::ready(device.matches(usage_page, usage_id, vendor_id, product_id))
        })
        .next()
        .await
        .ok_or_else(|| {
            eyre::eyre!(
                "could not find hid device {}:{}",
                hex::encode(vendor_id.to_be_bytes()),
                hex::encode(product_id.to_be_bytes())
            )
        })?
        .open(async_hid::AccessMode::Read)
        .await?;

    let mut buf = [0u8; 8];
    let mut inp = String::new();

    let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                // If we have no input, no processing is needed.
                if inp.is_empty() { continue; }

                if let Err(err) = tx.send(Ok(inp.clone())).await {
                    tracing::error!("could not send scanner value: {err}");
                    break;
                }

                // Clear the input after sending.
                inp.clear();
            }
            _ = tx.closed() => {
                tracing::info!("receiver closed, ending task");
                break;
            }
            _ = token.cancelled() => {
                tracing::info!("task cancelled, ending");
                break;
            }
            input = device.read_input_report(&mut buf) => {
                let size = input?;

                tracing::trace!(size, buf = hex::encode(&buf[0..size]), "got input report");

                let mod_keys = BitFlags::<ModifierKeys>::from_bits(buf[0]).expect("impossible modifier key flags");

                // Iterate through each potentially pressed key, combine with
                // shifts, and append to the input buffer.
                for key_byte in &buf[2..size] {
                    if *key_byte == 0x00 { continue };

                    let Some(key) = KEY_LOOKUP.get(key_byte) else {
                        tracing::warn!(key_byte, "got unknown keycode");
                        continue;
                    };

                    let key = if mod_keys.contains(ModifierKeys::ShiftLeft | ModifierKeys::ShiftRight) {
                        key.to_ascii_uppercase()
                    } else {
                        *key
                    };

                    inp.push(key);
                }

                // Reset interval to keep waiting for more keys before sending.
                interval.reset();
                tracing::trace!(current_input = inp, "finished processing input report");
            }
        }
    }

    Ok(())
}

#[tracing::instrument(skip(token, tx, usage_page, usage_id))]
async fn hid_scanner_pos(
    token: CancellationToken,
    tx: tokio::sync::mpsc::Sender<eyre::Result<String>>,
    usage_page: u16,
    usage_id: u16,
    vendor_id: u16,
    product_id: u16,
) -> eyre::Result<()> {
    let device = async_hid::DeviceInfo::enumerate()
        .await?
        .filter(|device| {
            futures::future::ready(device.matches(usage_page, usage_id, vendor_id, product_id))
        })
        .next()
        .await
        .ok_or_else(|| {
            eyre::eyre!(
                "could not find hid device {}:{}",
                hex::encode(vendor_id.to_be_bytes()),
                hex::encode(product_id.to_be_bytes())
            )
        })?
        .open(async_hid::AccessMode::Read)
        .await?;

    let mut buf = [0u8; 64];
    let mut inp = Vec::<u8>::new();

    loop {
        tokio::select! {
            _ = tx.closed() => {
                tracing::info!("receiver closed, ending task");
                break;
            }
            _ = token.cancelled() => {
                tracing::info!("task cancelled, ending");
                break;
            }
            input = device.read_input_report(&mut buf) => {
                let _read_size = input?;

                let data_len = buf[0] as usize;
                tracing::trace!(data_len, buf = hex::encode(&buf[0..data_len + 1]), "got input report");

                // On the first input report, we have length, a fixed 0x0215,
                // then the relevant data. It should be skipped over, using the
                // length of the data indicated in the packet, offset by the
                // number of bytes we don't need to read.
                let useful_bytes = if inp.is_empty() {
                    &buf[3..=data_len]
                } else {
                    &buf[1..=data_len]
                };

                inp.extend(useful_bytes);

                // Barcode scanners are often set to end data with a \r\n, but
                // we can't really be certain it's the real end if the input
                // report just happened to end there.
                //
                // TODO: Consider if this should also have an interval-based
                // solution for determining the end.
                if data_len != 63 {
                    tracing::debug!(size = inp.len(), "packet finished");

                    let s = String::from_utf8_lossy(&inp);
                    tx.send(Ok(s.to_string())).await.unwrap();
                    inp.clear();
                }
            }
        }
    }

    Ok(())
}

#[tracing::instrument(skip(token, tx))]
async fn serial_scanner(
    token: CancellationToken,
    tx: tokio::sync::mpsc::Sender<eyre::Result<String>>,
    path: String,
    baud_rate: u32,
) -> eyre::Result<()> {
    let mut port = tokio_serial::new(path, baud_rate).open_native_async()?;

    let mut buf = [0u8; 4096];
    let mut inp = String::new();

    let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                // If we have no input, no processing is needed.
                if inp.is_empty() { continue; }

                if let Err(err) = tx.send(Ok(inp.clone())).await {
                    tracing::error!("could not send scanner value: {err}");
                    break;
                }

                // Clear the input after sending.
                inp.clear();
            }
            _ = tx.closed() => {
                tracing::info!("receiver closed, ending task");
                break;
            }
            _ = token.cancelled() => {
                tracing::info!("task cancelled, ending");
                break;
            }
            input = tokio::io::AsyncReadExt::read(&mut port, &mut buf) => {
                let size = input?;

                if size == 0 { continue }

                let s = String::from_utf8_lossy(&buf[0..size]);
                tracing::trace!("read data from port: {s}");

                inp.push_str(&s);

                // Reset interval to keep waiting for more keys before sending.
                interval.reset();
            }
        }
    }

    Ok(())
}
