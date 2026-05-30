mod pty;

use bytes::{Buf, BufMut, Bytes, BytesMut};
use clap::Parser;
use clap_num::maybe_hex;
use futures_util::{Sink, SinkExt, Stream, StreamExt, TryStreamExt};
use nusb::list_devices;
use nusb::transfer::{Bulk, In, Out};
use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio_util::io::{CopyToBytes, SinkWriter, StreamReader};
use zerocopy::{FromBytes, IntoBytes};
use zerocopy_derive::{FromBytes, Immutable, IntoBytes, KnownLayout};

#[repr(C, packed)]
#[derive(FromBytes, Default, Immutable, KnownLayout, Debug, IntoBytes)]
struct KisHeader {
    sequence: u16,
    version: u8,
    portal: u8,
    command: u8,
    offset1: u8,
    offset2: u8,
    offset3: u8,
    args_len: u32,
}

#[repr(C, packed)]
#[derive(FromBytes, Default, Immutable, KnownLayout, Debug, IntoBytes)]
struct PaArgs {
    addr: u64,
    length: u32,
}

async fn endpoint_tx(
    interface: &nusb::Interface,
    ep_id: u8,
) -> Result<impl Sink<Bytes, Error = std::io::Error> + Send + 'static, nusb::Error> {
    let ep = interface.endpoint::<Bulk, Out>(ep_id)?;
    let writer = ep.writer(16384);
    Ok(futures_util::sink::unfold(
        writer,
        |mut writer, b: Bytes| async move {
            writer.write_all(&b).await?;
            writer.flush().await?;
            Ok::<_, std::io::Error>(writer)
        },
    ))
}

async fn endpoint_rx(
    interface: &nusb::Interface,
    ep_id: u8,
) -> Result<impl Stream<Item = std::io::Result<Bytes>> + Send + 'static, nusb::Error> {
    let ep = interface.endpoint::<Bulk, In>(ep_id | 0x80)?;
    Ok(futures_util::stream::try_unfold(ep, |mut ep| async move {
        while ep.pending() < 8 {
            let buffer = ep.allocate(16384);
            ep.submit(buffer);
        }
        let buffer = ep.next_complete().await.into_result()?;
        let data = Bytes::copy_from_slice(&buffer[..]);
        ep.submit(buffer);
        Ok(Some((data, ep)))
    }))
}

#[repr(u8)]
#[derive(Debug)]
enum KisPortal {
    Config = 0x01,
    //RSM = 0x10,
    Pam = 0x11,
    Ppm = 0x13,
}

#[repr(u8)]
enum KisCommand {
    //Pcr = 0,
    Pcw = 1,
    Par = 2,
    Paw = 3,
    //Pswd = 5,
}

const DATA_TX_FREE: u64 = 0x4014;
const DATA_TX8: u64 = 0x4004;
const DATA_TX16: u64 = 0x4008;
const DATA_TX24: u64 = 0x400c;
const DATA_TX32: u64 = 0x4010;

impl KisPortal {
    fn endpoint_id(&self, device_version: u16) -> Option<u8> {
        match (device_version, self) {
            (_, KisPortal::Config) => Some(1),

            // bcdDevice 1.20: M1 / Pro / Max
            (0x120, KisPortal::Pam) => Some(1),
            (0x120, KisPortal::Ppm) => Some(2),

            // bcdDevice 2.00: M2
            // bcdDevice 3.00: M2 Pro
            // bcdDevice 4.00: M3, M3 Max, M4, M4 Pro, A18 Pro
            (0x200 | 0x300 | 0x400, KisPortal::Pam) => Some(3),
            (0x200 | 0x300 | 0x400, KisPortal::Ppm) => Some(4),
            _ => None,
        }
    }
}

struct DebugUsb {
    device_version: u16,
    base: u64,
    interface: nusb::Interface,
    rx: HashMap<u8, Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>>,
    tx: HashMap<u8, Pin<Box<dyn Sink<Bytes, Error = std::io::Error> + Send>>>,
    capacity: usize,
}

#[derive(Debug, thiserror::Error)]
enum DebugUsbError {
    #[error("No DebugUSB device found")]
    DeviceNotFound,
    #[error("USB error: {0}")]
    Nusb(#[from] nusb::Error),
    #[error("Do not know {0:?} portal endpoint for device version {1:x}")]
    MissingEndpoint(KisPortal, u16),
    #[error("I/O Error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Timed out")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("Read error")]
    Read,
    #[error("Alignment error")]
    Alignment,
    #[error("Unknown base")]
    UnknownBase,
}

impl DebugUsb {
    async fn open(base: Option<u64>) -> Result<Self, DebugUsbError> {
        let device = list_devices()
            .await?
            .find(|dev| dev.vendor_id() == 0x05ac && dev.product_id() == 0x1881)
            .ok_or(DebugUsbError::DeviceNotFound)?;

        let device = device.open().await?;
        device.set_configuration(1).await?;
        let interface = device.claim_interface(0).await?;

        let mut dbgusb = Self {
            interface,
            base: base.unwrap_or(0),
            device_version: device.device_descriptor().device_version(),
            rx: Default::default(),
            tx: Default::default(),
            capacity: 0,
        };

        if base.is_none() {
            dbgusb.guess_base().await?;
        }

        Ok(dbgusb)
    }

    async fn get_rx(
        &mut self,
        ep_id: u8,
    ) -> Result<&mut Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>, DebugUsbError>
    {
        match self.rx.entry(ep_id) {
            std::collections::hash_map::Entry::Occupied(o) => Ok(o.into_mut()),
            std::collections::hash_map::Entry::Vacant(v) => {
                Ok(v.insert(Box::pin(endpoint_rx(&self.interface, ep_id).await?)))
            }
        }
    }

    async fn get_tx(
        &mut self,
        ep_id: u8,
    ) -> Result<&mut Pin<Box<dyn Sink<Bytes, Error = std::io::Error> + Send>>, DebugUsbError> {
        match self.tx.entry(ep_id) {
            std::collections::hash_map::Entry::Occupied(o) => Ok(o.into_mut()),
            std::collections::hash_map::Entry::Vacant(v) => {
                Ok(v.insert(Box::pin(endpoint_tx(&self.interface, ep_id).await?)))
            }
        }
    }

    async fn tx(&mut self, ep_id: u8, msg: Bytes) -> Result<(), DebugUsbError> {
        self.get_tx(ep_id).await?.send(msg).await?;
        Ok(())
    }

    async fn rx(&mut self, ep_id: u8) -> Result<Bytes, DebugUsbError> {
        Ok(self.get_rx(ep_id).await?.next().await.unwrap()?)
    }

    async fn req(&mut self, ep_id: u8, msg: Bytes) -> Result<Bytes, DebugUsbError> {
        log::trace!("out[{}]: {:x}", ep_id, msg);
        self.tx(ep_id, msg).await?;
        let res = tokio::time::timeout(Duration::from_millis(2500), self.rx(ep_id)).await??;
        log::trace!("in[{}]:  {:x}", ep_id, res);
        Ok(res)
    }

    async fn guess_base(&mut self) -> Result<(), DebugUsbError> {
        let cfg_ep = KisPortal::Config.endpoint_id(self.device_version).unwrap();
        let mut buf = BytesMut::new();
        buf.extend_from_slice(
            KisHeader {
                sequence: 0x0080,
                version: 0xa0,
                portal: 0,
                command: KisCommand::Par as u8,
                offset1: 0x01,
                offset2: 0x00,
                offset3: 0,
                args_len: 0xc,
            }
            .as_bytes(),
        );
        buf.extend_from_slice(
            PaArgs {
                addr: 0,
                length: 80,
            }
            .as_bytes(),
        );
        let mut descriptor = self.req(cfg_ep, buf.freeze()).await?;

        drop(descriptor.split_to(32));
        let mut base = descriptor.get_u64_le() & !0xffffff;

        if self.device_version >= 0x400 {
            base &= !0x1000000;
        }

        for off in [0x0, 0x400000, 0x700000] {
            self.base = base + off;
            log::info!("Trying base = 0x{:x}", self.base);
            if self.uart_tx_free().await.is_ok() {
                log::info!("Guessed base = 0x{:x}", self.base);
                return Ok(());
            }
        }

        self.base = 0;
        Err(DebugUsbError::UnknownBase)
    }

    async fn enable_portals(&mut self) -> Result<(), DebugUsbError> {
        let cfg_ep = KisPortal::Config.endpoint_id(self.device_version).unwrap();
        let mut buf = BytesMut::new();
        buf.extend_from_slice(
            KisHeader {
                sequence: 0xff02,
                version: 0xa0,
                portal: KisPortal::Config as u8,
                command: KisCommand::Pcw as u8,
                offset1: 0x16,
                offset2: 0x04,
                offset3: 0,
                args_len: 4,
            }
            .as_bytes(),
        );
        buf.put_u32_le(0x30000);
        self.req(cfg_ep, buf.freeze()).await?;
        Ok(())
    }

    async fn uart_rx(&mut self) -> Result<impl AsyncRead + Send + 'static, DebugUsbError> {
        self.enable_portals().await?;
        let pam_ep = KisPortal::Pam.endpoint_id(self.device_version).ok_or(
            DebugUsbError::MissingEndpoint(KisPortal::Pam, self.device_version),
        )?;
        let mut buf = BytesMut::new();
        buf.extend_from_slice(
            KisHeader {
                sequence: 0x1300,
                version: 0xa0,
                portal: KisPortal::Ppm as u8,
                command: KisCommand::Pcw as u8,
                offset1: 0x03,
                offset2: 0x04,
                offset3: 0,
                args_len: 4,
            }
            .as_bytes(),
        );
        buf.put_u32_le(1);
        self.req(pam_ep, buf.freeze()).await?;

        let ppm_ep = KisPortal::Ppm.endpoint_id(self.device_version).ok_or(
            DebugUsbError::MissingEndpoint(KisPortal::Ppm, self.device_version),
        )?;
        let uart_rx = endpoint_rx(&self.interface, ppm_ep).await?;

        let stream = uart_rx.try_filter_map(move |mut retbuf| {
            let hdr_bytes = retbuf.split_to(std::mem::size_of::<KisHeader>());
            let hdr = KisHeader::ref_from_bytes(&hdr_bytes).unwrap();

            if hdr.portal == KisPortal::Ppm as u8 {
                let words = retbuf.get_u32_le() as usize;
                let mut content = retbuf.split_to(words * 4);
                let bytes = retbuf.get_u32_le() as usize;
                content.truncate(bytes);

                log::debug!("uart:  {}", String::from_utf8_lossy(&content));

                futures_util::future::ready(Ok(Some(content)))
            } else {
                log::trace!("in[{}]:  {:#x?}", ppm_ep, hdr);
                futures_util::future::ready(Ok(None))
            }
        });

        Ok(StreamReader::new(stream))
    }

    async fn uart_tx_free(&mut self) -> Result<usize, DebugUsbError> {
        let pam_ep = KisPortal::Pam.endpoint_id(self.device_version).ok_or(
            DebugUsbError::MissingEndpoint(KisPortal::Pam, self.device_version),
        )?;

        let cmd = {
            let mut buf = BytesMut::new();
            buf.extend_from_slice(
                KisHeader {
                    sequence: 0x1155,
                    version: 0xa0,
                    portal: KisPortal::Pam as u8,
                    command: KisCommand::Par as u8,
                    offset1: 0,
                    offset2: 0,
                    offset3: 0,
                    args_len: 0xc,
                }
                .as_bytes(),
            );
            buf.extend_from_slice(
                PaArgs {
                    addr: self.base + 0x130000 + DATA_TX_FREE,
                    length: 0x04,
                }
                .as_bytes(),
            );

            buf.freeze()
        };
        let mut resp = self.req(pam_ep, cmd).await.map_err(std::io::Error::other)?;

        let hdr_bytes = resp.split_to(std::mem::size_of::<KisHeader>());
        let _hdr = KisHeader::ref_from_bytes(&hdr_bytes).unwrap();

        let word1 = resp.get_u32_le();
        let word2 = resp.get_u32_le();
        let _word3 = resp.get_u32_le();

        if word2 == 0 {
            return Err(DebugUsbError::Read);
        }

        Ok(word1 as usize)
    }

    async fn uart_tx(&mut self, data: Bytes) -> Result<(), DebugUsbError> {
        if data.len() > 4 && !data.len().is_multiple_of(4) {
            return Err(DebugUsbError::Alignment);
        }

        let pam_ep = KisPortal::Pam.endpoint_id(self.device_version).ok_or(
            DebugUsbError::MissingEndpoint(KisPortal::Pam, self.device_version),
        )?;
        let payload_len = data.len().next_multiple_of(4);
        let padding_bytes = payload_len - data.len();
        self.capacity -= data.len();
        let cmd = {
            let mut buf = BytesMut::new();

            buf.extend_from_slice(
                KisHeader {
                    sequence: 0x1160,
                    version: 0xa0,
                    portal: KisPortal::Pam as u8,
                    command: KisCommand::Paw as u8,
                    offset1: 0,
                    offset2: 0,
                    offset3: 0,
                    args_len: 12 + (payload_len as u32),
                }
                .as_bytes(),
            );

            let addr = self.base
                + 0x130000
                + match data.len() {
                    1 => DATA_TX8,
                    2 => DATA_TX16,
                    3 => DATA_TX24,
                    n => DATA_TX32,
                };
            buf.extend_from_slice(
                PaArgs {
                    addr,
                    length: payload_len as u32,
                }
                .as_bytes(),
            );

            buf.put(data);
            buf.put_bytes(0, padding_bytes);

            buf.freeze()
        };
        self.req(pam_ep, cmd).await.map_err(std::io::Error::other)?;
        Ok(())
    }

    async fn into_uart_tx(self) -> Result<impl AsyncWrite + Send + 'static, DebugUsbError> {
        let sink = futures_util::sink::unfold(self, move |mut dbgusb, mut b: Bytes| async move {
            while !b.is_empty() {
                const MAX_PACKET_SIZE: usize = 512;
                const MAX_PAYLOAD_SIZE: usize = MAX_PACKET_SIZE
                    - std::mem::size_of::<KisHeader>()
                    - std::mem::size_of::<PaArgs>()
                    - 4; // ?
                let mut payload_size = std::cmp::min(MAX_PAYLOAD_SIZE, b.len());
                if dbgusb.capacity < payload_size {
                    dbgusb.capacity = dbgusb.uart_tx_free().await.map_err(std::io::Error::other)?;
                }
                if dbgusb.capacity == 0 {
                    continue;
                }
                payload_size = std::cmp::min(payload_size, dbgusb.capacity);
                if payload_size > 4 {
                    payload_size = payload_size & !3;
                }
                let batch = b.split_to(payload_size);
                dbgusb.uart_tx(batch).await.map_err(std::io::Error::other)?;
            }
            Ok::<_, std::io::Error>(dbgusb)
        });

        Ok(SinkWriter::new(CopyToBytes::new(sink)))
    }
}

async fn debugusb_loop(args: &Args, pty: &mut pty::Pty) -> Result<(), DebugUsbError> {
    let mut dbgusb = DebugUsb::open(args.base).await?;
    log::info!("Device opened");

    let rx = dbgusb.uart_rx().await?;
    let tx = dbgusb.into_uart_tx().await?;
    let uart = tokio::io::join(rx, tx);
    let mut uart = std::pin::pin!(uart);
    tokio::io::copy_bidirectional(&mut uart, pty).await?;
    Ok(())
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long, value_parser=maybe_hex::<u64>)]
    base: Option<u64>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), DebugUsbError> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    let mut pty = pty::Pty::new()?;
    log::debug!("Allocated pty {}", pty.name());

    let remove_res = match tokio::fs::remove_file(&"/dev/m1n1").await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(()) => Ok(()),
    };
    remove_res?;

    match tokio::fs::symlink(pty.name(), &"/dev/m1n1").await {
        Ok(()) => log::debug!("Symlinked /dev/m1n1 to {}", pty.name()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            log::warn!(
                "No permissions to create /dev/m1n1 symlink using pty {}",
                pty.name()
            );
        }
        Err(e) => return Err(DebugUsbError::Io(e)),
    };

    log::info!("Waiting for device");
    loop {
        match debugusb_loop(&args, &mut pty).await {
            Err(DebugUsbError::DeviceNotFound) => {}
            Err(e) => {
                log::error!("Disconnected: {}", e);
                log::info!("Waiting for device");
            }
            Ok(()) => {
                log::info!("Waiting for device");
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
