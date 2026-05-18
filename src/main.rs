mod pty;

use anyhow::Context;
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
    indexLo: u8,
    indexHiRplSizeLo: u8,
    rplSizeHi: u8,
    reqSize: u32,
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
) -> anyhow::Result<impl Sink<Bytes, Error = std::io::Error> + Send + 'static> {
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
) -> anyhow::Result<impl Stream<Item = std::io::Result<Bytes>> + Send + 'static> {
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
enum KisPortal {
    Config = 0x01,
    //RSM = 0x10,
    PAM = 0x11,
    PPM = 0x13,
}

#[repr(u8)]
enum KisCommand {
    PCR = 0,
    PCW = 1,
    PAR = 2,
    PAW = 3,
    PSWD = 5,
}

impl KisPortal {
    fn endpoint_id(&self, device_version: u16) -> Option<u8> {
        match (device_version, self) {
            (_, KisPortal::Config) => Some(1),

            // bcdDevice 1.20: M1 Pro
            (0x120, KisPortal::PAM) => Some(1),
            (0x120, KisPortal::PPM) => Some(2),

            // bcdDevice 4.00: M4, A18 Pro
            (0x400, KisPortal::PAM) => Some(3),
            (0x400, KisPortal::PPM) => Some(4),
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
}

impl DebugUsb {
    async fn open(base: u64) -> anyhow::Result<Self> {
        let device = list_devices()
            .await
            .context("list devices")?
            .find(|dev| dev.vendor_id() == 0x05ac && dev.product_id() == 0x1881)
            .ok_or(anyhow::anyhow!("device not found"))?;

        let device = device.open().await.context("open device")?;
        device
            .set_configuration(1)
            .await
            .context("set configuration")?;
        let interface = device.claim_interface(0).await.context("claim interface")?;

        Ok(Self {
            interface,
            base,
            device_version: device.device_descriptor().device_version(),
            rx: Default::default(),
            tx: Default::default(),
        })
    }

    async fn get_rx(
        &mut self,
        ep_id: u8,
    ) -> anyhow::Result<&mut Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>> {
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
    ) -> anyhow::Result<&mut Pin<Box<dyn Sink<Bytes, Error = std::io::Error> + Send>>> {
        match self.tx.entry(ep_id) {
            std::collections::hash_map::Entry::Occupied(o) => Ok(o.into_mut()),
            std::collections::hash_map::Entry::Vacant(v) => {
                Ok(v.insert(Box::pin(endpoint_tx(&self.interface, ep_id).await?)))
            }
        }
    }

    async fn tx(&mut self, ep_id: u8, msg: Bytes) -> anyhow::Result<()> {
        self.get_tx(ep_id).await?.send(msg).await?;
        Ok(())
    }

    async fn rx(&mut self, ep_id: u8) -> anyhow::Result<Bytes> {
        Ok(self.get_rx(ep_id).await?.next().await.unwrap()?)
    }

    async fn req(&mut self, ep_id: u8, msg: Bytes) -> anyhow::Result<Bytes> {
        //println!("out: {:x}", msg);
        self.tx(ep_id, msg).await?;
        tokio::time::timeout(Duration::from_millis(250), self.rx(ep_id)).await?
    }

    async fn enable_portals(
        &mut self,
        portals: impl IntoIterator<Item = KisPortal>,
    ) -> anyhow::Result<()> {
        let cfg_ep = KisPortal::Config.endpoint_id(self.device_version).unwrap();
        let mut buf = BytesMut::new();
        buf.extend_from_slice(
            KisHeader {
                sequence: 0xff02,
                version: 0xa0,
                portal: KisPortal::Config as u8,
                command: KisCommand::PCW as u8,
                indexLo: 0x16,
                indexHiRplSizeLo: 0x04,
                rplSizeHi: 0,
                reqSize: 4,
            }
            .as_bytes(),
        );
        buf.put_u32_le(0x30000);
        self.req(cfg_ep, buf.freeze()).await?;
        Ok(())
    }

    async fn uart_rx(&mut self) -> anyhow::Result<impl AsyncRead + Send + 'static> {
        self.enable_portals([]).await?;
        let pam_ep = KisPortal::PAM
            .endpoint_id(self.device_version)
            .ok_or(anyhow::anyhow!(
                "Do not know PAM portal endpoint for device version {:x}",
                self.device_version
            ))?;
        let mut buf = BytesMut::new();
        buf.extend_from_slice(
            KisHeader {
                sequence: 0x1300,
                version: 0xa0,
                portal: KisPortal::PPM as u8,
                command: KisCommand::PCW as u8,
                indexLo: 0x03,
                indexHiRplSizeLo: 0x04,
                rplSizeHi: 0,
                reqSize: 4,
            }
            .as_bytes(),
        );
        buf.put_u32_le(1);
        self.req(pam_ep, buf.freeze()).await?;

        let ppm_ep = KisPortal::PPM
            .endpoint_id(self.device_version)
            .ok_or(anyhow::anyhow!(
                "Do not know PPM portal endpoint for device version {:x}",
                self.device_version
            ))?;
        let uart_rx = endpoint_rx(&self.interface, ppm_ep).await?;

        let stream = uart_rx.try_filter_map(|mut retbuf| {
            let hdr_bytes = retbuf.split_to(std::mem::size_of::<KisHeader>());
            let hdr = KisHeader::ref_from_bytes(&hdr_bytes).unwrap();

            if hdr.portal == KisPortal::PPM as u8 {
                let words = retbuf.get_u32_le() as usize;
                let mut content = retbuf.split_to(words * 4);
                let bytes = retbuf.get_u32_le() as usize;
                content.truncate(bytes);

                //print!("{}", String::from_utf8_lossy(&content));

                futures_util::future::ready(Ok(Some(content)))
            } else {
                println!("in: {:#x?}", hdr);
                futures_util::future::ready(Ok(None))
            }
        });

        Ok(StreamReader::new(stream))
    }

    async fn uart_tx(self) -> anyhow::Result<impl AsyncWrite + Send + 'static> {
        let pam_ep = KisPortal::PAM
            .endpoint_id(self.device_version)
            .ok_or(anyhow::anyhow!(
                "Do not know PAM portal endpoint for device version {:x}",
                self.device_version
            ))?;

        let sink = futures_util::sink::unfold(self, move |mut dbgusb, mut b: Bytes| async move {
            let with_padding = b.split_off(b.len() - b.len() % 4);
            for msg in [b, with_padding] {
                if msg.is_empty() {
                    continue;
                }
                for (hdr, args) in [
                    (
                        KisHeader {
                            sequence: 0x1100,
                            version: 0xa0,
                            portal: KisPortal::PAM as u8,
                            command: KisCommand::PAR as u8,
                            indexLo: 0,
                            indexHiRplSizeLo: 0,
                            rplSizeHi: 0,
                            reqSize: 0xc,
                        },
                        PaArgs {
                            addr: dbgusb.base + 0x13402c,
                            length: 0x04,
                        },
                    ),
                    (
                        KisHeader {
                            sequence: 0x1155,
                            version: 0xa0,
                            portal: KisPortal::PAM as u8,
                            command: KisCommand::PAR as u8,
                            indexLo: 0,
                            indexHiRplSizeLo: 0,
                            rplSizeHi: 0,
                            reqSize: 0xc,
                        },
                        PaArgs {
                            addr: dbgusb.base + 0x134014,
                            length: 0x04,
                        },
                    ),
                ] {
                    let cmd = {
                        let mut buf = BytesMut::new();
                        buf.extend_from_slice(hdr.as_bytes());
                        buf.extend_from_slice(args.as_bytes());

                        buf.freeze()
                    };
                    let res = dbgusb
                        .req(pam_ep, cmd)
                        .await
                        .map_err(std::io::Error::other)?;
                    //println!("in:  {:x}", res);
                }

                let padding_bytes = (4 - (msg.len() % 4)) % 4;
                let cmd = {
                    let mut buf = BytesMut::new();

                    buf.extend_from_slice(
                        KisHeader {
                            sequence: 0x1160,
                            version: 0xa0,
                            portal: KisPortal::PAM as u8,
                            command: KisCommand::PAW as u8,
                            indexLo: 0,
                            indexHiRplSizeLo: 0,
                            rplSizeHi: 0,
                            reqSize: (12 + msg.len() + padding_bytes) as u32,
                        }
                        .as_bytes(),
                    );

                    buf.extend_from_slice(
                        PaArgs {
                            addr: dbgusb.base + 0x134000 + 4 * (4 - padding_bytes as u64),
                            length: (msg.len() + padding_bytes) as u32,
                        }
                        .as_bytes(),
                    );

                    buf.put(msg);
                    buf.put_bytes(0, padding_bytes);

                    buf.freeze()
                };
                let res = dbgusb
                    .req(pam_ep, cmd)
                    .await
                    .map_err(std::io::Error::other)?;
                //println!("in:  {:x}", res);
            }
            Ok::<_, std::io::Error>(dbgusb)
        });

        Ok(SinkWriter::new(CopyToBytes::new(sink)))
    }
}

async fn debugusb_loop(args: &Args, pty: &mut pty::Pty) -> anyhow::Result<()> {
    let mut dbgusb = DebugUsb::open(args.base).await?;

    let rx = dbgusb.uart_rx().await?;
    let tx = dbgusb.uart_tx().await?;
    let uart = tokio::io::join(rx, tx);
    let mut uart = std::pin::pin!(uart);
    tokio::io::copy_bidirectional(&mut uart, pty).await?;
    Ok(())
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long, value_parser=maybe_hex::<u64>)]
    base: u64,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let mut pty = pty::Pty::new()?;
    println!("{}", pty.name());

    let remove_res = match tokio::fs::remove_file(&"/dev/m1n1").await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(()) => Ok(()),
    };
    remove_res?;
    tokio::fs::symlink(pty.name(), &"/dev/m1n1").await?;

    loop {
        if let Err(e) = debugusb_loop(&args, &mut pty).await {
            println!("{:?}", e);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
